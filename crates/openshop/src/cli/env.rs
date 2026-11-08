//! `.env` loading for development.

use std::path::Path;

fn parse_env_line(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }

    let assignment = trimmed
        .strip_prefix("export")
        .filter(|rest| rest.starts_with(char::is_whitespace))
        .map(str::trim_start)
        .unwrap_or(trimmed);
    let (key, value) = assignment.split_once('=')?;
    let key = key.trim_end();
    let mut characters = key.chars();
    let valid_key = characters
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && characters.all(|character| character.is_ascii_alphanumeric() || character == '_');
    if !valid_key {
        return None;
    }

    let mut value = value.trim_start();
    let quoted = value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"')) || (value.starts_with('\'') && value.ends_with('\'')));
    if quoted {
        value = &value[1..value.len() - 1];
    }
    Some((key.to_string(), value.to_string()))
}

/// Variables defined in a `.env` file, in order.
pub fn parse_env_file(content: &str) -> Vec<(String, String)> {
    content.lines().filter_map(parse_env_line).collect()
}

/// Loads `<cwd>/.env`. Variables already set in the environment win. Returns
/// how many it set.
///
/// Setting an environment variable is only sound while no other thread reads
/// the environment: call this before the async runtime starts. `App::main`
/// does so for `dev`, and then there is nothing left to set.
pub fn load_env_file(cwd: &Path) -> usize {
    let Ok(content) = std::fs::read_to_string(cwd.join(".env")) else {
        return 0;
    };
    let mut set = 0;
    for (key, value) in parse_env_file(&content) {
        if std::env::var_os(&key).is_none() {
            set_env(&key, &value);
            set += 1;
        }
    }
    set
}

/// Loads the `.env` file of the project for the commands of development:
/// `dev`, and `run` and `cron`, which run a flow by hand in the environment
/// of `dev`. Returns how many variables it set.
pub fn load_dev_env(args: &[String], cwd: &Path) -> usize {
    match args.first().map(String::as_str) {
        Some("dev" | "run" | "cron") => load_env_file(cwd),
        _ => 0,
    }
}

#[allow(unsafe_code)]
fn set_env(key: &str, value: &str) {
    // SAFETY: called before the runtime starts, so that no other thread reads the environment.
    // A caller that runs its own runtime is told so by `run_with_args`.
    unsafe { std::env::set_var(key, value) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_assignments_comments_and_quotes() {
        let parsed = parse_env_file(
            "# comment\n\nDATABASE_URL=postgresql://localhost/db\nexport SHOPIFY_API_KEY = \"key 1\"\nSINGLE='a=b'\nEMPTY=\nNOT VALID=1\n1BAD=2\njust text\r\nWINDOWS=ok\r\n",
        );
        assert_eq!(
            parsed,
            vec![
                ("DATABASE_URL".to_string(), "postgresql://localhost/db".to_string()),
                ("SHOPIFY_API_KEY".to_string(), "key 1".to_string()),
                ("SINGLE".to_string(), "a=b".to_string()),
                ("EMPTY".to_string(), String::new()),
                ("WINDOWS".to_string(), "ok".to_string()),
            ]
        );
    }

    #[test]
    fn keeps_unbalanced_quotes_verbatim() {
        assert_eq!(
            parse_env_file("A=\"open\nB='"),
            vec![
                ("A".to_string(), "\"open".to_string()),
                ("B".to_string(), "'".to_string())
            ]
        );
    }

    #[test]
    fn run_and_cron_load_the_env_file_of_dev() {
        for command in ["run", "cron"] {
            let dir = std::env::temp_dir().join(format!("openshop-env-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let name = format!("OPENSHOP_TEST_ENV_{}", command.to_ascii_uppercase());
            std::fs::write(dir.join(".env"), format!("{name}=from-file\n")).unwrap();
            assert_eq!(load_dev_env(&[command.to_string(), "arg".to_string()], &dir), 1, "{command}");
            assert_eq!(std::env::var(&name).unwrap(), "from-file");
            std::fs::remove_dir_all(dir).ok();
        }
    }

    #[test]
    fn only_dev_loads_the_env_file_and_sets_what_is_missing() {
        let dir = std::env::temp_dir().join(format!("openshop-env-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(".env"),
            "OPENSHOP_TEST_ENV_NEW=from-file\nOPENSHOP_TEST_ENV_KEPT=from-file\n",
        )
        .unwrap();
        // SAFETY: names no other test or code reads.
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("OPENSHOP_TEST_ENV_KEPT", "from-process")
        };
        let args = |args: &[&str]| args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();

        for command in [&["start"][..], &["worker"], &["migrate"], &[]] {
            assert_eq!(load_dev_env(&args(command), &dir), 0, "{command:?}");
        }
        assert!(std::env::var_os("OPENSHOP_TEST_ENV_NEW").is_none());

        assert_eq!(load_dev_env(&args(&["dev"]), &dir), 1);
        assert_eq!(std::env::var("OPENSHOP_TEST_ENV_NEW").unwrap(), "from-file");
        assert_eq!(std::env::var("OPENSHOP_TEST_ENV_KEPT").unwrap(), "from-process");
        // Loaded once: a second pass, as the one in `run`, has nothing left to set.
        assert_eq!(load_dev_env(&args(&["dev"]), &dir), 0);
        std::fs::remove_dir_all(dir).ok();
    }
}
