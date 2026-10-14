//! `openshop init <dir>`: scaffold a project from one of the embedded templates.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use dialoguer::theme::ColorfulTheme;
use dialoguer::{Input, Select};

const OPENSHOP_VERSION: &str = env!("CARGO_PKG_VERSION");
const OPENSHOP_REPOSITORY: &str = "https://github.com/0xtlt/openshop-rust";

/// The manifest of the workspace the templates are members of: what their
/// `workspace = true` stands for.
const WORKSPACE_MANIFEST: &str = include_str!("../../../Cargo.toml");

/// The files of the templates in `templates/`, as `(path in the project,
/// content)`: `MINIMAL`, `CUSTOM` and `PUBLIC`, and `SHARED`, which every
/// project gets.
mod files {
    include!(concat!(env!("OUT_DIR"), "/templates.rs"));
}

/// A project `openshop init` starts from.
#[derive(Debug)]
pub struct Template {
    pub name: &'static str,
    /// What it is, in the list `openshop init` offers.
    pub summary: &'static str,
    /// Whether its admin pages have views, compiled to WebAssembly.
    pub views: bool,
    files: &'static [(&'static str, &'static str)],
}

impl Template {
    /// How the sources of the template name its crate, `openshop-template-<name>`.
    fn crate_name(&self) -> String {
        format!("openshop_template_{}", self.name)
    }
}

/// The templates, the first one offered by default.
pub const TEMPLATES: &[Template] = &[
    Template {
        name: "minimal",
        summary: "One provider and one flow, with the built-in admin",
        views: false,
        files: files::MINIMAL,
    },
    Template {
        name: "custom",
        summary: "A custom app: every building block, the built-in admin and a page of its own",
        views: true,
        files: files::CUSTOM,
    },
    Template {
        name: "public",
        summary: "A public app: an admin made of its own pages, the operator dashboard for you",
        views: true,
        files: files::PUBLIC,
    },
];

fn template_list() -> String {
    let lines: Vec<String> = TEMPLATES
        .iter()
        .map(|template| format!("\n  {:<8} {}", template.name, template.summary))
        .collect();
    format!("Templates:{}", lines.concat())
}

fn template_named(name: &str) -> Result<&'static Template> {
    match TEMPLATES.iter().find(|template| template.name == name) {
        Some(template) => Ok(template),
        None => bail!("[openshop] Unknown template: {name}\n\n{}", template_list()),
    }
}

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

/// The sections of a manifest as `(header, lines)`, the lines of a section
/// starting with the comments right above its header. What comes before the
/// first header has an empty one.
fn sections(manifest: &str) -> Vec<(&str, Vec<&str>)> {
    let mut sections = vec![("", Vec::new())];
    for line in manifest.lines() {
        let Some((_, previous)) = sections.last_mut() else {
            continue;
        };
        let header = line.trim();
        if !header.starts_with('[') {
            previous.push(line);
            continue;
        }
        let comments = previous
            .iter()
            .rev()
            .take_while(|line| line.trim_start().starts_with('#'))
            .count();
        let mut lines = previous.split_off(previous.len() - comments);
        lines.push(line);
        sections.push((header, lines));
    }
    sections
}

/// The value of `key` in the `[workspace.<table>]` of the workspace manifest.
fn workspace_value(table: &str, key: &str) -> Result<&'static str> {
    let header = format!("[workspace.{table}]");
    sections(WORKSPACE_MANIFEST)
        .into_iter()
        .filter(|(name, _)| *name == header)
        .flat_map(|(_, lines)| lines)
        .find_map(|line| {
            let (name, value) = line.split_once('=')?;
            (name.trim() == key).then(|| value.trim())
        })
        .with_context(|| format!("[openshop] The workspace has no `{key}` in {header}"))
}

/// A line of the dependencies of a template, as a project declares it:
/// `workspace = true` replaced by the source of the dependency, and `None`
/// for a path dependency, which only exists in the repository.
fn project_dependency(line: &str, openshop: &str) -> Result<Option<String>> {
    if line.trim_start().starts_with('#') {
        return Ok(Some(line.to_string()));
    }
    let Some((name, value)) = line.split_once('=') else {
        return Ok(Some(line.to_string()));
    };
    let (name, value) = (name.trim(), value.trim());
    if value.contains("path =") {
        return Ok(None);
    }
    // `serde.workspace = true`
    if let Some(name) = name.strip_suffix(".workspace") {
        let source = match name {
            "openshop" => format!("{{ {openshop} }}"),
            _ => workspace_value("dependencies", name)?.to_string(),
        };
        return Ok(Some(format!("{name} = {source}")));
    }
    // `openshop = { workspace = true, features = ["ui"] }`
    if let Some(rest) = value.strip_prefix("{ workspace = true") {
        let source = match name {
            "openshop" => openshop.to_string(),
            _ => match workspace_value("dependencies", name)? {
                version if version.starts_with('"') => format!("version = {version}"),
                _ => bail!("[openshop] A template adds to `{name}` of the workspace, a table: declare it in full"),
            },
        };
        return Ok(Some(format!("{name} = {{ {source}{rest}")));
    }
    Ok(Some(line.to_string()))
}

/// The manifest of a project made from a template: a package of its own, the
/// dependencies of the template with their source, and the profiles of the
/// workspace, which a member of it cannot declare.
fn project_manifest(template: &str, package_name: &str, openshop: &str) -> Result<String> {
    let mut manifest: Vec<String> = Vec::new();
    for (header, lines) in sections(template) {
        match header {
            "[package]" => manifest.extend([
                "[package]".to_string(),
                format!("name = {}", toml_string(package_name)),
                "version = \"0.1.0\"".to_string(),
                format!("edition = {}", workspace_value("package", "edition")?),
                "publish = false".to_string(),
                String::new(),
            ]),
            "[dependencies]" | "[dev-dependencies]" => {
                for line in lines {
                    match project_dependency(line, openshop)? {
                        Some(line) => manifest.push(line),
                        // The comments of a dependency go with it.
                        None => {
                            while manifest.last().is_some_and(|line| line.trim_start().starts_with('#')) {
                                manifest.pop();
                            }
                        }
                    }
                }
            }
            _ => manifest.extend(lines.into_iter().map(str::to_string)),
        }
    }
    for (header, lines) in sections(WORKSPACE_MANIFEST) {
        if header.starts_with("[profile.") {
            if manifest.last().is_some_and(|line| !line.trim().is_empty()) {
                manifest.push(String::new());
            }
            manifest.extend(lines.into_iter().map(str::to_string));
        }
    }
    Ok(manifest.join("\n").trim_end().to_string() + "\n")
}

/// What a project names itself, where a template names itself.
struct Project<'a> {
    app_name: &'a str,
    package_name: &'a str,
    openshop: &'a str,
}

/// A file of a template, as the project gets it.
fn project_file(path: &str, content: &str, template: &Template, project: &Project) -> Result<String> {
    let rename_lines = |prefix: &str, line: String| {
        let lines: Vec<String> = content
            .lines()
            .map(|current| match current.starts_with(prefix) {
                true => line.clone(),
                false => current.to_string(),
            })
            .collect();
        lines.join("\n") + "\n"
    };
    Ok(match path {
        "Cargo.toml" => project_manifest(content, project.package_name, project.openshop)?,
        "README.md" => rename_lines("# ", format!("# {}", project.app_name)),
        "shopify.app.toml" => rename_lines("name = ", format!("name = {}", toml_string(project.app_name))),
        _ if path.ends_with(".rs") => content.replace(&template.crate_name(), &project.package_name.replace('-', "_")),
        _ => content.replace("__PACKAGE_NAME__", project.package_name),
    })
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

const INIT_USAGE: &str = "Usage: openshop init [<dir>] [--template=<name>]";

/// What `openshop init` was given.
#[derive(Debug, Default, PartialEq, Eq)]
struct InitArgs {
    target: Option<String>,
    template: Option<String>,
    /// `--openshop-path=<dir>`: depend on a local checkout of the framework
    /// instead of its git repository.
    openshop_path: Option<PathBuf>,
}

fn parse_init_args(args: &[String]) -> Result<InitArgs> {
    let mut parsed = InitArgs::default();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if let Some(path) = arg.strip_prefix("--openshop-path=") {
            parsed.openshop_path = Some(path.into());
        } else if let Some(name) = arg.strip_prefix("--template=") {
            parsed.template = Some(name.to_string());
        } else if arg == "--template" || arg == "-t" {
            let name = args
                .next()
                .with_context(|| format!("[openshop] {arg} needs a template name"))?;
            parsed.template = Some(name.clone());
        } else if arg.starts_with('-') || parsed.target.is_some() {
            bail!(
                "[openshop] Unexpected argument: {arg}\n\n{INIT_USAGE}\n\n{}",
                template_list()
            );
        } else {
            parsed.target = Some(arg.clone());
        }
    }
    Ok(parsed)
}

fn check_target(target_dir: &Path) -> Result<()> {
    if !is_empty_dir(target_dir) {
        bail!("[openshop] Target directory is not empty: {}", target_dir.display());
    }
    Ok(())
}

fn ask_target() -> Result<String> {
    Ok(Input::<String>::with_theme(&ColorfulTheme::default())
        .with_prompt("Project directory")
        .default("my-app".to_string())
        .validate_with(|name: &String| match name.trim().is_empty() {
            true => Err("Name a directory"),
            false => Ok(()),
        })
        .interact_text()?
        .trim()
        .to_string())
}

fn ask_template() -> Result<&'static Template> {
    let items = TEMPLATES
        .iter()
        .map(|template| format!("{:<8} {}", template.name, template.summary));
    let chosen = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Template")
        .items(items)
        .default(0)
        .interact()?;
    Ok(&TEMPLATES[chosen])
}

/// `openshop init`: what it was not given, it asks for in a terminal.
pub fn run_init(cwd: &Path, args: &[String]) -> Result<InitResult> {
    let args = parse_init_args(args)?;
    let template = args.template.as_deref().map(template_named).transpose()?;
    let interactive = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
    let target = match args.target {
        Some(target) => target,
        None if interactive => ask_target()?,
        None => bail!("{INIT_USAGE}\n\n{}", template_list()),
    };
    check_target(&cwd.join(&target))?;
    let template = match template {
        Some(template) => template,
        None if interactive => ask_template()?,
        None => bail!(
            "[openshop] Name a template: openshop init {target} --template=<name>\n\n{}",
            template_list()
        ),
    };
    init(cwd, &target, template, args.openshop_path.as_deref())
}

/// Creates `target`, a project made from `template`.
pub fn init(cwd: &Path, target: &str, template: &Template, openshop_path: Option<&Path>) -> Result<InitResult> {
    let target_dir = cwd.join(target);
    check_target(&target_dir)?;
    let openshop_path = openshop_path
        .map(|path| path.canonicalize().context("[openshop] --openshop-path does not exist"))
        .transpose()?;
    std::fs::create_dir_all(&target_dir)?;
    let target_dir = target_dir.canonicalize()?;

    let app_name = target_dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "openshop-app".to_string());
    let package_name = to_package_name(&app_name);
    let openshop = openshop_source(openshop_path.as_deref());
    let project = Project {
        app_name: &app_name,
        package_name: &package_name,
        openshop: &openshop,
    };

    for (path, content) in template.files.iter().chain(files::SHARED) {
        let file = target_dir.join(path);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&file, project_file(path, content, template, &project)?)?;
    }

    println!(
        "[openshop] Created {app_name} in {}, from the `{}` template",
        target_dir.display(),
        template.name
    );
    println!();
    println!("Next steps:");
    println!("  cd {target}");
    println!("  docker compose up -d");
    println!("  cp .env.example .env");
    if template.views {
        println!("  rustup target add wasm32-unknown-unknown && cargo install trunk    # once, for the admin pages");
    }
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

    fn minimal() -> &'static Template {
        template_named("minimal").unwrap()
    }

    fn args(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| arg.to_string()).collect()
    }

    #[test]
    fn reads_what_it_is_given() {
        assert_eq!(parse_init_args(&[]).unwrap(), InitArgs::default());
        let expected = InitArgs {
            target: Some("my-app".to_string()),
            template: Some("public".to_string()),
            openshop_path: None,
        };
        assert_eq!(
            parse_init_args(&args(&["my-app", "--template=public"])).unwrap(),
            expected
        );
        assert_eq!(
            parse_init_args(&args(&["--template", "public", "my-app"])).unwrap(),
            expected
        );
        assert_eq!(parse_init_args(&args(&["-t", "public", "my-app"])).unwrap(), expected);
        assert_eq!(
            parse_init_args(&args(&["--openshop-path=../openshop", "my-app"])).unwrap(),
            InitArgs {
                target: Some("my-app".to_string()),
                template: None,
                openshop_path: Some("../openshop".into()),
            }
        );

        let error = parse_init_args(&args(&["my-app", "--template"]))
            .unwrap_err()
            .to_string();
        assert_eq!(error, "[openshop] --template needs a template name");
        let error = parse_init_args(&args(&["my-app", "other"])).unwrap_err().to_string();
        assert!(error.starts_with("[openshop] Unexpected argument: other"), "{error}");
        let error = parse_init_args(&args(&["--force", "my-app"])).unwrap_err().to_string();
        assert!(error.contains("Templates:\n  minimal "), "{error}");
        let error = template_named("shop").unwrap_err().to_string();
        assert!(
            error.starts_with("[openshop] Unknown template: shop\n\nTemplates:"),
            "{error}"
        );
    }

    #[test]
    fn scaffolds_the_minimal_template() {
        let cwd = temp_dir("scaffold");
        let result = init(&cwd, "My Shop App", minimal(), None).unwrap();

        assert_eq!(result.app_name, "My Shop App");
        assert_eq!(result.package_name, "my-shop-app");

        let read = |path: &str| std::fs::read_to_string(result.target_dir.join(path)).unwrap();
        let manifest = read("Cargo.toml");
        assert!(manifest.starts_with(
            "[package]\nname = \"my-shop-app\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n"
        ));
        let source = format!("git = \"https://github.com/0xtlt/openshop-rust\", tag = \"v{OPENSHOP_VERSION}\"");
        assert!(manifest.contains(&format!("[dependencies]\nopenshop = {{ {source} }}\n")));
        assert!(manifest.contains("serde = { version = \"1\", features = [\"derive\"] }"));
        assert!(manifest.contains(&format!("openshop = {{ {source}, features = [\"testing\"] }}")));
        // The profiles of the workspace, which a template cannot declare.
        assert!(manifest.contains("[profile.dev.package.scrypt]\nopt-level = 3"));
        assert!(manifest.contains("[profile.release]\nlto = \"thin\""));
        assert!(read("shopify.web.toml").contains("dev = \"openshop dev\""));
        assert!(read("shopify.app.toml").contains("name = \"My Shop App\""));
        assert!(read("README.md").starts_with("# My Shop App\n"));
        assert!(read("Dockerfile").contains("target/release/my-shop-app"));
        assert_eq!(read(".gitignore"), "target/\n.env\n.shopify/\ndist/\n");
        assert!(read(".env.example").contains("DATABASE_URL="));
        assert!(read("src/flows/sync_orders.rs").contains("syncOrders"));
        // The application is declared, and starts with MCP off.
        let main = read("src/main.rs");
        assert!(main.contains("static APP: App = app! {"));
        assert!(main.contains("mcp: McpConfig::disabled(),"));
        assert!(main.contains("APP.main()"));
        assert!(read("src/providers/warehouse.rs").contains("#[provider]"));
        assert!(!manifest.contains("tokio") && !manifest.contains("schemars"));
        // What only serves the repository stays there.
        assert!(!result.target_dir.join("examples").exists());

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
    fn every_template_makes_a_project_of_its_own() {
        let cwd = temp_dir("templates");
        for template in TEMPLATES {
            let result = init(&cwd, &format!("shop-{}", template.name), template, None).unwrap();
            let paths: Vec<_> = template.files.iter().map(|(path, _)| *path).collect();
            assert!(
                files::SHARED.iter().all(|(path, _)| !paths.contains(path)),
                "{} overrides a shared file",
                template.name
            );
            for path in paths.iter().chain(files::SHARED.iter().map(|(path, _)| path)) {
                let content = std::fs::read_to_string(result.target_dir.join(path)).unwrap();
                for repository_only in [
                    "openshop_template_",
                    "openshop-template-",
                    "workspace",
                    "__PACKAGE_NAME__",
                ] {
                    assert!(
                        !content.contains(repository_only),
                        "{}: {path} still has {repository_only}",
                        template.name
                    );
                }
            }
            let manifest = std::fs::read_to_string(result.target_dir.join("Cargo.toml")).unwrap();
            assert!(!manifest.contains("path ="), "{manifest}");
            assert_eq!(manifest.contains("features = [\"ui\"]"), template.views, "{manifest}");
            assert_eq!(manifest.contains("leptos = \"0.8\""), template.views, "{manifest}");
            assert!(result.target_dir.join("migrations").is_dir());
            assert!(result.target_dir.join(".env.example").is_file());
        }
        let main = std::fs::read_to_string(cwd.join("shop-public/src/main.rs")).unwrap();
        assert!(main.contains("shop_public::APP.main()"), "{main}");
        std::fs::remove_dir_all(cwd).ok();
    }

    #[test]
    fn a_template_manifest_becomes_the_manifest_of_a_project() {
        let template = "[package]\nname = \"openshop-template-x\"\nversion.workspace = true\n\n[dependencies]\nopenshop = { workspace = true, features = [\"ui\"] }\nserde.workspace = true\nregex = { workspace = true, features = [\"unicode\"] }\n# The views.\nleptos = \"0.8\"\n\n[dev-dependencies]\nopenshop = { workspace = true, features = [\"testing\"] }\n# Only in the repository.\npreview = { path = \"../preview\" }\n";
        let manifest = project_manifest(template, "shop", "path = \"/openshop\"").unwrap();
        let (manifest, profiles) = manifest.split_once("\n\n# MCP token hashing").unwrap();
        assert_eq!(
            manifest,
            "[package]\nname = \"shop\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n[dependencies]\nopenshop = { path = \"/openshop\", features = [\"ui\"] }\nserde = { version = \"1\", features = [\"derive\"] }\nregex = { version = \"1\", features = [\"unicode\"] }\n# The views.\nleptos = \"0.8\"\n\n[dev-dependencies]\nopenshop = { path = \"/openshop\", features = [\"testing\"] }"
        );
        assert!(profiles.contains("[profile.release]"));

        let error = project_manifest(
            "[dependencies]\nsqlx = { workspace = true, features = [\"uuid\"] }\n",
            "shop",
            "",
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("adds to `sqlx` of the workspace"), "{error}");
    }

    #[test]
    fn can_depend_on_a_local_framework_checkout() {
        let cwd = temp_dir("local");
        let framework = cwd.join("framework");
        std::fs::create_dir_all(&framework).unwrap();
        let result = init(&cwd, "app", minimal(), Some(&framework)).unwrap();

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
        let project = init(&cwd, "my-app", minimal(), None).unwrap().target_dir;

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
    fn refuses_non_empty_directories() {
        let cwd = temp_dir("refuse");
        std::fs::create_dir_all(cwd.join("taken")).unwrap();
        std::fs::write(cwd.join("taken/file.txt"), "x").unwrap();
        let error = init(&cwd, "taken", minimal(), None).unwrap_err().to_string();
        assert!(error.contains("Target directory is not empty"), "{error}");

        std::fs::create_dir_all(cwd.join("empty")).unwrap();
        assert!(init(&cwd, "empty", minimal(), None).is_ok());
        std::fs::remove_dir_all(cwd).ok();
    }
}
