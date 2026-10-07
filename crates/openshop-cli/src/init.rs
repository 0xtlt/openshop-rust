//! `openshop init <dir>`: scaffold a project from the embedded template.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use openshop::db::migrate::generate_migrations;

const OPENSHOP_VERSION: &str = env!("CARGO_PKG_VERSION");
const OPENSHOP_REPOSITORY: &str = "https://github.com/0xtlt/openshop-rust";

/// Template files as `(path in the project, content)`.
const TEMPLATE: &[(&str, &str)] = &[
    ("Cargo.toml", include_str!("../templates/minimal/Cargo.toml.tmpl")),
    ("src/main.rs", include_str!("../templates/minimal/src/main.rs")),
    (
        "src/flows/mod.rs",
        include_str!("../templates/minimal/src/flows/mod.rs"),
    ),
    (
        "src/flows/sync_orders.rs",
        include_str!("../templates/minimal/src/flows/sync_orders.rs"),
    ),
    (
        "src/providers/mod.rs",
        include_str!("../templates/minimal/src/providers/mod.rs"),
    ),
    (
        "src/providers/warehouse.rs",
        include_str!("../templates/minimal/src/providers/warehouse.rs"),
    ),
    (
        "shopify.app.toml",
        include_str!("../templates/minimal/shopify.app.toml"),
    ),
    (
        "shopify.web.toml",
        include_str!("../templates/minimal/shopify.web.toml"),
    ),
    (".env.example", include_str!("../templates/minimal/_env.example")),
    (".gitignore", include_str!("../templates/minimal/_gitignore")),
    (".dockerignore", include_str!("../templates/minimal/_dockerignore")),
    ("Dockerfile", include_str!("../templates/minimal/Dockerfile")),
    (
        "docker-compose.yml",
        include_str!("../templates/minimal/docker-compose.yml"),
    ),
    ("Procfile", include_str!("../templates/minimal/Procfile")),
    ("README.md", include_str!("../templates/minimal/README.md")),
];

/// Files of the custom admin UI bundle created by `openshop init-ui`.
const UI_TEMPLATE: &[(&str, &str)] = &[
    ("Cargo.toml", include_str!("../templates/ui/Cargo.toml.tmpl")),
    ("Trunk.toml", include_str!("../templates/ui/Trunk.toml")),
    ("index.html", include_str!("../templates/ui/index.html")),
    ("src/main.rs", include_str!("../templates/ui/src/main.rs")),
    (".gitignore", include_str!("../templates/ui/_gitignore")),
];

/// What to add to the application so that it serves the example page.
const UI_NEXT_STEPS: &str = r#"
Register the page and the bundle in your configuration:

    .custom_pages([CustomAdminNavigationItem {
        label: "Hello".to_string(),
        path: "/hello".to_string(),
    }])
    .admin_page(AdminPage::new("/hello").loader("greeting", |ctx, _input| async move {
        Ok(json!({ "message": format!("Hello {}", ctx.shop) }))
    }))
    .admin_ui(UiSource::Dir("ui/dist".into()))

Then build the bundle (once: `rustup target add wasm32-unknown-unknown` and `cargo install trunk`):

    cd ui && trunk build --release

`openshop dev` rebuilds it for you when `ui/` changes.
"#;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitResult {
    pub target_dir: PathBuf,
    pub package_name: String,
    pub app_name: String,
}

/// A valid Cargo package name derived from a directory name.
pub fn to_package_name(name: &str) -> String {
    let mut normalized = String::new();
    for character in name.trim().to_lowercase().chars() {
        if character.is_ascii_alphanumeric() || character == '_' {
            normalized.push(character);
        } else if !normalized.ends_with('-') {
            normalized.push('-');
        }
    }
    let normalized = normalized.trim_matches(|character| character == '-' || character == '_');

    match normalized.chars().next() {
        None => "openshop-app".to_string(),
        Some(first) if first.is_ascii_digit() => format!("app-{normalized}"),
        Some(_) => normalized.to_string(),
    }
}

fn is_empty_dir(path: &Path) -> bool {
    match std::fs::read_dir(path) {
        Ok(mut entries) => entries.next().is_none(),
        Err(_) => !path.exists(),
    }
}

fn toml_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

/// How the generated project depends on the framework.
fn openshop_source(openshop_path: Option<&Path>) -> String {
    match openshop_path {
        Some(path) => format!("path = {}", toml_string(&path.display().to_string())),
        // Pinned to the release that created the project.
        None => format!(
            "git = {}, tag = {}",
            toml_string(OPENSHOP_REPOSITORY),
            toml_string(&format!("v{OPENSHOP_VERSION}"))
        ),
    }
}

/// The package name declared in a `Cargo.toml`, if any.
fn package_name(manifest: &str) -> Option<String> {
    let mut in_package = false;
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            in_package = line == "[package]";
        } else if in_package
            && let Some(value) = line.strip_prefix("name")
            && let Some(value) = value.trim_start().strip_prefix('=')
        {
            return Some(value.trim().trim_matches('"').to_string());
        }
    }
    None
}

/// `openshop init-ui [dir]`: scaffolds the admin UI bundle of a project with
/// custom admin pages (default directory: `ui`).
pub fn run_init_ui(cwd: &Path, args: &[String]) -> Result<PathBuf> {
    let openshop_path = args
        .iter()
        .find_map(|arg| arg.strip_prefix("--openshop-path="))
        .map(PathBuf::from);
    let target = args
        .iter()
        .find(|arg| !arg.starts_with("--"))
        .map(String::as_str)
        .unwrap_or("ui");

    let Ok(manifest) = std::fs::read_to_string(cwd.join("Cargo.toml")) else {
        bail!("[openshop] No Cargo.toml here. Run `openshop init-ui` from the root of an OpenShop project.");
    };
    let package = package_name(&manifest).unwrap_or_else(|| "openshop-app".to_string());

    let target_dir = cwd.join(target);
    if !is_empty_dir(&target_dir) {
        bail!("[openshop] Target directory is not empty: {}", target_dir.display());
    }

    // The UI crate lives next to the framework crate in the repository.
    let source = match openshop_path {
        Some(path) => {
            let ui = path
                .join("../openshop-ui")
                .canonicalize()
                .context("[openshop] --openshop-path has no sibling openshop-ui crate")?;
            format!("path = {}", toml_string(&ui.display().to_string()))
        }
        None => openshop_source(None),
    };
    let replacements = [("__PACKAGE_NAME__", package), ("__OPENSHOP_UI_SOURCE__", source)];

    for (path, template) in UI_TEMPLATE {
        let mut content = (*template).to_string();
        for (placeholder, value) in &replacements {
            content = content.replace(placeholder, value);
        }
        let file = target_dir.join(path);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&file, content)?;
    }

    println!("[openshop] Created the admin UI bundle in {}", target_dir.display());
    println!(
        "{}",
        UI_NEXT_STEPS
            .replace("ui/dist", &format!("{target}/dist"))
            .replace("cd ui", &format!("cd {target}"))
            .replace("`ui/`", &format!("`{target}/`"))
    );
    Ok(target_dir)
}

/// `args`: the target directory, optionally `--openshop-path=<dir>` to depend on
/// a local checkout of the framework crate instead of the git repository.
pub fn run_init(cwd: &Path, args: &[String]) -> Result<InitResult> {
    let openshop_path = args
        .iter()
        .find_map(|arg| arg.strip_prefix("--openshop-path="))
        .map(PathBuf::from);
    let Some(target) = args.iter().find(|arg| !arg.starts_with("--")) else {
        bail!("Usage: openshop init <dir>");
    };

    let target_dir = cwd.join(target);
    if !is_empty_dir(&target_dir) {
        bail!("[openshop] Target directory is not empty: {}", target_dir.display());
    }
    std::fs::create_dir_all(&target_dir)?;
    let target_dir = target_dir.canonicalize()?;

    let app_name = target_dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "openshop-app".to_string());
    let package_name = to_package_name(&app_name);
    let openshop_path = openshop_path
        .map(|path| path.canonicalize().context("[openshop] --openshop-path does not exist"))
        .transpose()?;
    let replacements = [
        ("__APP_NAME__", app_name.clone()),
        ("__PACKAGE_NAME__", package_name.clone()),
        ("__OPENSHOP_VERSION__", OPENSHOP_VERSION.to_string()),
        ("__OPENSHOP_SOURCE__", openshop_source(openshop_path.as_deref())),
    ];

    for (path, template) in TEMPLATE {
        let mut content = (*template).to_string();
        for (placeholder, value) in &replacements {
            content = content.replace(placeholder, value);
        }
        let file = target_dir.join(path);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&file, content)?;
    }

    // The framework schema becomes the project's first, client-owned migrations.
    generate_migrations(&target_dir, &[], false, &[])?;

    println!("[openshop] Created {app_name} in {}", target_dir.display());
    println!();
    println!("Next steps:");
    println!("  cd {target}");
    println!("  docker compose up -d");
    println!("  cp .env.example .env");
    println!("  openshop dev");

    Ok(InitResult {
        target_dir,
        package_name,
        app_name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "openshop-init-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn normalizes_package_names() {
        assert_eq!(to_package_name("My App"), "my-app");
        assert_eq!(to_package_name("  Acme/Storefront Sync!  "), "acme-storefront-sync");
        assert_eq!(to_package_name("snake_case_app"), "snake_case_app");
        assert_eq!(to_package_name("--weird--name--"), "weird-name");
        assert_eq!(to_package_name("2fast"), "app-2fast");
        assert_eq!(to_package_name("..."), "openshop-app");
        assert_eq!(to_package_name(""), "openshop-app");
    }

    #[test]
    fn scaffolds_a_project_with_placeholders_replaced() {
        let cwd = temp_dir("scaffold");
        let result = run_init(&cwd, &["My Shop App".to_string()]).unwrap();

        assert_eq!(result.app_name, "My Shop App");
        assert_eq!(result.package_name, "my-shop-app");

        let read = |path: &str| std::fs::read_to_string(result.target_dir.join(path)).unwrap();
        let manifest = read("Cargo.toml");
        assert!(manifest.contains("name = \"my-shop-app\""));
        let source = format!("git = \"https://github.com/0xtlt/openshop-rust\", tag = \"v{OPENSHOP_VERSION}\"");
        assert!(manifest.contains(&format!("openshop = {{ {source} }}")));
        assert!(manifest.contains(&format!("openshop = {{ {source}, features = [\"testing\"] }}")));
        assert!(read("shopify.web.toml").contains("dev = \"openshop dev\""));
        assert!(read("shopify.app.toml").contains("name = \"My Shop App\""));
        assert!(read("Dockerfile").contains("target/release/my-shop-app"));
        assert!(read(".gitignore").contains("target/"));
        assert!(read(".env.example").contains("DATABASE_URL="));
        assert!(read("src/flows/sync_orders.rs").contains("syncOrders"));
        let main = read("src/main.rs");
        assert!(main.contains(".mcp(McpConfig::new().enabled(false))"));
        assert!(main.contains(".page(AdminPageId::Mcp, AdminPageMode::Disabled)"));

        for path in TEMPLATE.iter().map(|(path, _)| path) {
            assert!(!read(path).contains("__"), "{path} still has a placeholder");
        }

        let mut migrations: Vec<_> = std::fs::read_dir(result.target_dir.join("migrations"))
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .collect();
        migrations.sort();
        assert_eq!(migrations.len(), openshop::db::migrate::FRAMEWORK_MIGRATIONS.len());
        let migration = std::fs::read_to_string(&migrations[0]).unwrap();
        assert!(migration.contains("CREATE TABLE IF NOT EXISTS installations"));
        std::fs::remove_dir_all(cwd).ok();
    }

    #[test]
    fn can_depend_on_a_local_framework_checkout() {
        let cwd = temp_dir("local");
        let framework = cwd.join("framework");
        std::fs::create_dir_all(&framework).unwrap();
        let result = run_init(
            &cwd,
            &[format!("--openshop-path={}", framework.display()), "app".to_string()],
        )
        .unwrap();

        let manifest = std::fs::read_to_string(result.target_dir.join("Cargo.toml")).unwrap();
        let expected = format!(
            "openshop = {{ path = \"{}\" }}",
            framework.canonicalize().unwrap().display()
        );
        assert!(manifest.contains(&expected), "{manifest}");
        std::fs::remove_dir_all(cwd).ok();
    }

    #[test]
    fn scaffolds_the_admin_ui_bundle_of_a_project() {
        let cwd = temp_dir("ui");
        let project = run_init(&cwd, &["my-app".to_string()]).unwrap().target_dir;

        let ui = run_init_ui(&project, &[]).unwrap();
        assert_eq!(ui, project.join("ui"));
        let read = |path: &str| std::fs::read_to_string(ui.join(path)).unwrap();
        let manifest = read("Cargo.toml");
        assert!(manifest.contains("name = \"my-app-ui\""));
        assert!(manifest.contains(&format!(
            "openshop-ui = {{ git = \"https://github.com/0xtlt/openshop-rust\", tag = \"v{OPENSHOP_VERSION}\" }}"
        )));
        assert!(manifest.contains("[workspace]"));
        assert!(read("index.html").contains("<meta name=\"shopify-api-key\" content=\"\" />"));
        assert!(read("Trunk.toml").contains("dist = \"dist\""));
        assert!(read("src/main.rs").contains("App::new().page(\"/hello\", HelloPage).mount();"));
        assert!(read(".gitignore").contains("dist/"));

        let error = run_init_ui(&project, &[]).unwrap_err().to_string();
        assert!(error.contains("Target directory is not empty"), "{error}");
        assert!(run_init_ui(&project, &["admin-ui".to_string()]).is_ok());

        let error = run_init_ui(&cwd, &[]).unwrap_err().to_string();
        assert!(error.contains("No Cargo.toml here"), "{error}");
        std::fs::remove_dir_all(cwd).ok();
    }

    #[test]
    fn reads_the_package_name_of_a_manifest() {
        assert_eq!(
            package_name("[workspace]\nname = \"no\"\n\n[package]\nname = \"my-app\"\nversion = \"0.1.0\"\n")
                .as_deref(),
            Some("my-app")
        );
        assert_eq!(package_name("[dependencies]\nname = \"x\"\n"), None);
    }

    #[test]
    fn refuses_missing_targets_and_non_empty_directories() {
        let cwd = temp_dir("refuse");
        assert_eq!(
            run_init(&cwd, &[]).unwrap_err().to_string(),
            "Usage: openshop init <dir>"
        );

        std::fs::create_dir_all(cwd.join("taken")).unwrap();
        std::fs::write(cwd.join("taken/file.txt"), "x").unwrap();
        let error = run_init(&cwd, &["taken".to_string()]).unwrap_err().to_string();
        assert!(error.contains("Target directory is not empty"), "{error}");

        std::fs::create_dir_all(cwd.join("empty")).unwrap();
        assert!(run_init(&cwd, &["empty".to_string()]).is_ok());
        std::fs::remove_dir_all(cwd).ok();
    }
}
