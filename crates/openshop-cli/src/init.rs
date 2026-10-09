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

/// The page `openshop init-ui` creates, with its view.
const UI_PAGE: (&str, &str) = ("src/admin/hello.rs", include_str!("../templates/ui/src/admin/hello.rs"));

/// The version of Leptos the views are written with: the one of `openshop-ui`.
const LEPTOS_VERSION: &str = "0.8";

/// What to write in the application so that it serves the example page.
const UI_NEXT_STEPS: &str = r#"
In `src/main.rs`, declare the module and list the page:

    mod admin;

    use admin::hello::HelloPage;

    static APP: App = app! {
        admin_pages: [HelloPage],
        admin_ui: openshop::UiSource::Dir("dist".into()),
        // …
    };

Install the toolchain once: `rustup target add wasm32-unknown-unknown` and `cargo install trunk`.
`openshop dev` builds the admin UI bundle from the pages and rebuilds it when they change;
`openshop build` builds it for production, into `dist/`.
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

/// The manifest of an application whose pages have a view: `openshop` with its
/// `ui` feature, and `leptos`. `None` when `openshop` is not declared on one
/// line of `[dependencies]`, the way `openshop init` writes it.
fn manifest_with_views(manifest: &str) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut in_dependencies = false;
    let mut declared = false;
    let has_leptos = manifest.lines().any(|line| line.trim_start().starts_with("leptos"));
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_dependencies = trimmed == "[dependencies]";
        }
        let is_openshop = in_dependencies
            && trimmed
                .strip_prefix("openshop")
                .is_some_and(|rest| rest.trim_start().starts_with('='));
        if !is_openshop {
            lines.push(line.to_string());
            continue;
        }
        let table = trimmed.strip_suffix('}')?.trim_end();
        lines.push(if trimmed.contains("\"ui\"") {
            line.to_string()
        } else if let Some((before, after)) = table.split_once("features = [") {
            format!("{before}features = [\"ui\", {after} }}")
        } else {
            format!("{table}, features = [\"ui\"] }}")
        });
        if !has_leptos {
            lines.push("# The views of the admin pages.".to_string());
            lines.push(format!("leptos = \"{LEPTOS_VERSION}\""));
        }
        declared = true;
    }
    declared.then(|| lines.join("\n") + "\n")
}

/// `openshop init-ui`: gives a project a first page with a view,
/// `src/admin/hello.rs`, and what views are compiled with.
pub fn run_init_ui(cwd: &Path) -> Result<()> {
    let manifest_path = cwd.join("Cargo.toml");
    let Ok(manifest) = std::fs::read_to_string(&manifest_path) else {
        bail!("[openshop] No Cargo.toml here. Run `openshop init-ui` from the root of an OpenShop project.");
    };
    let (path, content) = UI_PAGE;
    let page = cwd.join(path);
    if page.exists() {
        bail!("[openshop] {path} already exists.");
    }
    if let Some(parent) = page.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&page, content)?;

    // The page is a module of `admin`, next to the pages the project already has.
    let pages = cwd.join("src/admin/mod.rs");
    let declared = std::fs::read_to_string(&pages).unwrap_or_default();
    if !declared.lines().any(|line| line.trim().ends_with("mod hello;")) {
        let separator = if declared.is_empty() || declared.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        std::fs::write(&pages, format!("{declared}{separator}pub mod hello;\n"))?;
    }

    let ignored_path = cwd.join(".gitignore");
    let ignored = std::fs::read_to_string(&ignored_path).unwrap_or_default();
    if !ignored.lines().any(|line| line.trim().trim_end_matches('/') == "dist") {
        let separator = if ignored.is_empty() || ignored.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        std::fs::write(&ignored_path, format!("{ignored}{separator}dist/\n"))?;
    }

    println!("[openshop] Created {path}");
    match manifest_with_views(&manifest) {
        Some(manifest) => {
            std::fs::write(&manifest_path, manifest)?;
            println!("[openshop] Cargo.toml: enabled the `ui` feature of openshop, added leptos");
        }
        None => {
            println!();
            println!("In `Cargo.toml`, enable the `ui` feature of `openshop` and add `leptos`:");
            println!();
            println!("    openshop = {{ …, features = [\"ui\"] }}");
            println!("    leptos = \"{LEPTOS_VERSION}\"");
        }
    }
    println!("{UI_NEXT_STEPS}");
    Ok(())
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
        // The application is declared, and starts with MCP off.
        let main = read("src/main.rs");
        assert!(main.contains("static APP: App = app! {"));
        assert!(main.contains("mcp: McpConfig::disabled(),"));
        assert!(main.contains("APP.main()"));
        assert!(read("src/providers/warehouse.rs").contains("#[provider]"));
        assert!(!manifest.contains("tokio") && !manifest.contains("schemars"));

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

        run_init_ui(&project).unwrap();
        let read = |path: &str| std::fs::read_to_string(project.join(path)).unwrap();
        // The page is in the application, with its view: no other crate, and
        // nothing of the bundle, which `openshop` builds from the pages.
        assert!(!project.join("ui").exists());
        assert!(!project.join("Trunk.toml").exists() && !project.join("index.html").exists());
        let page = read("src/admin/hello.rs");
        assert!(page.contains("#[admin_page(\"/hello\", nav = \"Hello\")]"));
        assert!(page.contains("#[view]") && page.contains("use_loader(Self::GREETING, || ())"));
        assert_eq!(read("src/admin/mod.rs"), "pub mod hello;\n");
        assert_eq!(read(".gitignore"), "target/\n.env\n.shopify/\ndist/\n");

        let manifest = read("Cargo.toml");
        let source = format!("git = \"https://github.com/0xtlt/openshop-rust\", tag = \"v{OPENSHOP_VERSION}\"");
        assert!(manifest.contains(&format!(
            "[dependencies]\nopenshop = {{ {source}, features = [\"ui\"] }}\n# The views of the admin pages.\nleptos = \"0.8\"\n"
        )));
        // The tests keep their own features.
        assert!(manifest.contains(&format!("openshop = {{ {source}, features = [\"testing\"] }}")));

        // The bundle of the project is made of that page.
        let pages = crate::bundle::scan(&project).unwrap();
        assert_eq!(format!("{pages:?}").matches("admin::hello::HelloPage").count(), 1);

        let error = run_init_ui(&project).unwrap_err().to_string();
        assert_eq!(error, "[openshop] src/admin/hello.rs already exists.");

        let error = run_init_ui(&cwd).unwrap_err().to_string();
        assert!(error.contains("No Cargo.toml here"), "{error}");
        std::fs::remove_dir_all(cwd).ok();
    }

    #[test]
    fn a_manifest_gains_what_a_view_needs() {
        assert_eq!(
            manifest_with_views("[dependencies]\nopenshop = { path = \"../openshop\" }\nserde = \"1\"\n").as_deref(),
            Some(
                "[dependencies]\nopenshop = { path = \"../openshop\", features = [\"ui\"] }\n# The views of the admin pages.\nleptos = \"0.8\"\nserde = \"1\"\n"
            )
        );
        // Features and a `leptos` the manifest already has are kept.
        assert_eq!(
            manifest_with_views(
                "[dependencies]\nleptos = \"0.8\"\nopenshop = { version = \"1\", features = [\"testing\"] }\n"
            )
            .as_deref(),
            Some(
                "[dependencies]\nleptos = \"0.8\"\nopenshop = { version = \"1\", features = [\"ui\", \"testing\"] }\n"
            )
        );
        let ready = "[dependencies]\nleptos = \"0.8\"\nopenshop = { version = \"1\", features = [\"ui\"] }\n";
        assert_eq!(manifest_with_views(ready).as_deref(), Some(ready));
        // Another shape is left to its author.
        assert_eq!(manifest_with_views("[dependencies]\nopenshop.workspace = true\n"), None);
        assert_eq!(
            manifest_with_views("[dev-dependencies]\nopenshop = { version = \"1\" }\n"),
            None
        );
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
