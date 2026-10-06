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

/// Loads `<cwd>/.env`. Variables already set in the environment win.
///
/// Call this before starting any thread that reads the environment; the CLI does
/// so first thing in `dev`.
pub fn load_env_file(cwd: &Path) {
    let Ok(content) = std::fs::read_to_string(cwd.join(".env")) else {
        return;
    };
    for (key, value) in parse_env_file(&content) {
        if std::env::var_os(&key).is_none() {
            set_env(&key, &value);
        }
    }
}

#[allow(unsafe_code)]
fn set_env(key: &str, value: &str) {
    // SAFETY: called from the CLI entry point before worker tasks read the environment.
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
}
