//! The admin UI bundle of a project whose admin pages have a view.
//!
//! The bundle is a crate of its own, written under the `target` directory of
//! the project and built with trunk. It is made of the modules of the project
//! that declare pages, compiled to WebAssembly on their own: the rest of the
//! project is the server, and is not part of it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use openshop::serde_json::Value;
use proc_macro2::{Spacing, TokenStream, TokenTree};
use syn::visit::{self, Visit};
use syn::{ImplItem, Item, ItemImpl, ItemUse, Meta, UseTree};
use tokio::process::{Child, Command};

use crate::metadata;

/// The shell of the admin UI, as the server expects it.
const INDEX_HTML: &str = include_str!("../templates/ui/index.html");

/// What the bundle is compiled with, whatever its pages name.
const ALWAYS: [&str; 2] = ["openshop", "leptos"];

/// What is said when the bundle does not compile.
const HINT: &str = "[openshop] The admin UI bundle is compiled from the modules that declare pages, on their own. \
Outside the functions the server runs, a page names nothing else of the application; \
what only those functions need is imported inside them.";

/// What a project gives its bundle.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Pages {
    /// The pages with a view, by their path in the crate: `admin::reviews::ReviewsPage`.
    views: Vec<String>,
    /// The modules of the crate root that declare them, each with its file.
    modules: Vec<(String, PathBuf)>,
    /// The crates those modules name, outside the functions of the server.
    crates: BTreeSet<String>,
}

fn marked(attributes: &[syn::Attribute], marker: &str) -> bool {
    attributes.iter().any(|attribute| {
        attribute
            .path()
            .segments
            .last()
            .is_some_and(|segment| segment.ident == marker)
    })
}

/// Whether an `impl` block is an `#[admin_page]`.
fn is_page(item: &ItemImpl) -> bool {
    item.trait_.is_none() && marked(&item.attrs, "admin_page")
}

fn is_view(implemented: &ImplItem) -> bool {
    matches!(implemented, ImplItem::Fn(function) if marked(&function.attrs, "view"))
}

/// The pages with a view among items, by their path from the crate root.
fn views(items: &[Item], module: &[String], found: &mut Vec<String>) {
    for item in items {
        match item {
            Item::Impl(item) if is_page(item) && item.items.iter().any(is_view) => {
                let syn::Type::Path(page) = &*item.self_ty else {
                    continue;
                };
                let page = page.path.segments.iter().map(|segment| segment.ident.to_string());
                found.push(module.iter().cloned().chain(page).collect::<Vec<_>>().join("::"));
            }
            Item::Mod(inline) => {
                if let Some((_, items)) = &inline.content {
                    let mut module = module.to_vec();
                    module.push(inline.ident.to_string());
                    views(items, &module, found);
                }
            }
            _ => {}
        }
    }
}

/// The names that start a path in what the bundle keeps of a file.
#[derive(Default)]
struct Names(BTreeSet<String>);

impl Names {
    /// `name::…` in the tokens of a macro or of an attribute.
    fn tokens(&mut self, tokens: TokenStream) {
        let mut previous: Option<String> = None;
        let mut after_separator = false;
        let mut tokens = tokens.into_iter().peekable();
        while let Some(token) = tokens.next() {
            match token {
                TokenTree::Group(group) => {
                    self.tokens(group.stream());
                    (previous, after_separator) = (None, false);
                }
                TokenTree::Ident(ident) => {
                    previous = (!after_separator).then(|| ident.to_string());
                    after_separator = false;
                }
                TokenTree::Punct(punct)
                    if punct.as_char() == ':'
                        && punct.spacing() == Spacing::Joint
                        && matches!(tokens.peek(), Some(TokenTree::Punct(next)) if next.as_char() == ':') =>
                {
                    tokens.next();
                    self.0.extend(previous.take());
                    after_separator = true;
                }
                _ => (previous, after_separator) = (None, false),
            }
        }
    }

    fn imported(&mut self, tree: &UseTree) {
        match tree {
            UseTree::Path(path) => {
                self.0.insert(path.ident.to_string());
            }
            UseTree::Name(name) => {
                self.0.insert(name.ident.to_string());
            }
            UseTree::Rename(rename) => {
                self.0.insert(rename.ident.to_string());
            }
            UseTree::Group(group) => group.items.iter().for_each(|tree| self.imported(tree)),
            UseTree::Glob(_) => {}
        }
    }
}

impl<'ast> Visit<'ast> for Names {
    fn visit_item_impl(&mut self, item: &'ast ItemImpl) {
        if !is_page(item) {
            return visit::visit_item_impl(self, item);
        }
        // In the bundle, a page keeps its view: its other functions are the server's.
        for implemented in &item.items {
            if is_view(implemented) || !matches!(implemented, ImplItem::Fn(_)) {
                self.visit_impl_item(implemented);
            }
        }
    }

    fn visit_item_use(&mut self, item: &'ast ItemUse) {
        self.imported(&item.tree);
    }

    fn visit_path(&mut self, path: &'ast syn::Path) {
        if (path.leading_colon.is_some() || path.segments.len() > 1)
            && let Some(first) = path.segments.first()
        {
            self.0.insert(first.ident.to_string());
        }
        visit::visit_path(self, path);
    }

    fn visit_macro(&mut self, invoked: &'ast syn::Macro) {
        self.tokens(invoked.tokens.clone());
        visit::visit_macro(self, invoked);
    }

    fn visit_attribute(&mut self, attribute: &'ast syn::Attribute) {
        if let Meta::List(list) = &attribute.meta {
            self.tokens(list.tokens.clone());
        }
        visit::visit_attribute(self, attribute);
    }
}

/// The Rust files of a directory and of the directories under it, in order.
fn rust_files(directory: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(directory)
        .with_context(|| format!("[openshop] Cannot read {}", directory.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect();
    entries.sort();
    for entry in entries {
        if entry.is_dir() {
            rust_files(&entry, files)?;
        } else if entry.extension().is_some_and(|extension| extension == "rs") {
            files.push(entry);
        }
    }
    Ok(())
}

/// The module a source file is, from the crate root: `src/admin/reviews.rs`
/// is `admin::reviews`, and the root itself is empty. `None` for the files of
/// other binaries.
fn module_of(src: &Path, file: &Path) -> Option<Vec<String>> {
    let relative = file.strip_prefix(src).ok()?.with_extension("");
    let mut module: Vec<String> = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect();
    if module.first().is_some_and(|first| first == "bin") {
        return None;
    }
    if module.last().is_some_and(|last| last == "mod")
        || (module.len() == 1 && matches!(module[0].as_str(), "main" | "lib"))
    {
        module.pop();
    }
    Some(module)
}

/// Reads the sources of a project for the pages that have a view.
pub fn scan(cwd: &Path) -> Result<Pages> {
    let src = cwd.join("src");
    let mut files = Vec::new();
    if src.is_dir() {
        rust_files(&src, &mut files)?;
    }
    let shown = |file: &Path| file.strip_prefix(cwd).unwrap_or(file).display().to_string();
    let parse = |file: &Path, text: &str| {
        syn::parse_file(text).map_err(|error| {
            let start = error.span().start();
            anyhow!(
                "[openshop] {}:{}:{}: {error}",
                shown(file),
                start.line,
                start.column + 1
            )
        })
    };

    // Only what the bundle is made of is parsed: the rest of the project may
    // be anything the compiler accepts.
    let mut sources = Vec::new();
    let mut pages = Pages::default();
    for file in files {
        let Some(module) = module_of(&src, &file) else {
            continue;
        };
        let text =
            std::fs::read_to_string(&file).with_context(|| format!("[openshop] Cannot read {}", shown(&file)))?;
        if text.contains("admin_page") {
            let mut found = Vec::new();
            views(&parse(&file, &text)?.items, &module, &mut found);
            if let (true, Some(page)) = (module.is_empty(), found.first()) {
                bail!(
                    "[openshop] {} declares `{page}` with a `#[view]`. A page with a view is declared in a module, such as `src/admin/`: the admin UI bundle is made of that module.",
                    shown(&file)
                );
            }
            pages.views.extend(found);
        }
        sources.push((module, file, text));
    }

    // The bundle is made of the modules of the crate root the pages are in.
    let roots: BTreeSet<String> = pages
        .views
        .iter()
        .filter_map(|view| view.split("::").next().map(str::to_string))
        .collect();
    for root in &roots {
        let directory = src.join(root).join("mod.rs");
        let file = if directory.is_file() {
            directory
        } else {
            src.join(format!("{root}.rs"))
        };
        pages.modules.push((root.clone(), file));
    }
    let mut names = Names::default();
    for (module, file, text) in &sources {
        if module.first().is_some_and(|first| roots.contains(first)) {
            names.visit_file(&parse(file, text)?);
        }
    }
    // `Self::LIST_REVIEWS`, `super::components`: not crates.
    names
        .0
        .retain(|name| !matches!(name.as_str(), "Self" | "self" | "super" | "crate"));
    pages.crates = names.0;
    Ok(pages)
}

/// What the manifest of a project says, as `cargo metadata` reports it.
#[derive(Debug, PartialEq, Eq)]
struct Project {
    name: String,
    edition: String,
    /// The dependencies, by the name the code knows them under, each as a line of a manifest.
    dependencies: Vec<(String, String)>,
    target_directory: PathBuf,
    workspace_root: PathBuf,
    /// The crates on disk the project is compiled with, by their directory.
    local_crates: Vec<PathBuf>,
}

fn toml_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

/// A dependency of the project as a line of the manifest of the bundle.
fn dependency(declared: &Value) -> Option<(String, String)> {
    // Dependencies of tests and of build scripts are not for the bundle, nor
    // are the ones of another target than WebAssembly.
    if !declared["kind"].is_null() {
        return None;
    }
    match declared["target"].as_str() {
        None => {}
        Some(target) if target.contains("wasm32") && !target.contains("not(") => {}
        Some(_) => return None,
    }
    let package = declared["name"].as_str()?;
    let key = declared["rename"].as_str().unwrap_or(package);

    let mut options = Vec::new();
    if let Some(path) = declared["path"].as_str() {
        options.push(format!("path = {}", toml_string(path)));
    } else if let Some(git) = declared["source"]
        .as_str()
        .and_then(|source| source.strip_prefix("git+"))
    {
        let git = git.split('#').next().unwrap_or(git);
        let (url, reference) = git.split_once('?').unwrap_or((git, ""));
        options.push(format!("git = {}", toml_string(url)));
        for pair in reference.split('&') {
            if let Some((kind @ ("tag" | "branch" | "rev"), value)) = pair.split_once('=') {
                options.push(format!("{kind} = {}", toml_string(value)));
            }
        }
    } else {
        options.push(format!(
            "version = {}",
            toml_string(declared["req"].as_str().unwrap_or("*"))
        ));
    }
    if key != package {
        options.push(format!("package = {}", toml_string(package)));
    }
    if declared["uses_default_features"].as_bool() == Some(false) {
        options.push("default-features = false".to_string());
    }
    let features: Vec<String> = declared["features"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|feature| feature.as_str().map(toml_string))
        .collect();
    if !features.is_empty() {
        options.push(format!("features = [{}]", features.join(", ")));
    }
    Some((key.replace('-', "_"), format!("{key} = {{ {} }}", options.join(", "))))
}

/// The package of `manifest` in what `cargo metadata` printed.
fn project(metadata: &Value, manifest: &Path) -> Result<Project> {
    let package = metadata::package(metadata, manifest)
        .with_context(|| format!("[openshop] {} is not the manifest of a package", manifest.display()))?;
    let text = |value: &Value, what: &str| {
        value
            .as_str()
            .map(str::to_string)
            .with_context(|| format!("[openshop] cargo metadata reports no {what}"))
    };
    Ok(Project {
        name: text(&package["name"], "package name")?,
        edition: text(&package["edition"], "edition")?,
        dependencies: package["dependencies"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(dependency)
            .collect(),
        target_directory: text(&metadata["target_directory"], "target directory")?.into(),
        workspace_root: text(&metadata["workspace_root"], "workspace root")?.into(),
        local_crates: metadata::local_crates(metadata, manifest, None),
    })
}

/// The files of the crate of the bundle, by their path in it.
fn files(project: &Project, pages: &Pages, cwd: &Path) -> Vec<(&'static str, String)> {
    let dependencies: Vec<&str> = project
        .dependencies
        .iter()
        .filter(|(name, _)| ALWAYS.contains(&name.as_str()) || pages.crates.contains(name))
        .map(|(_, line)| line.as_str())
        .collect();
    let manifest = format!(
        r#"# The admin UI bundle of {name}: its admin pages, compiled to WebAssembly.
# Written by `openshop` from the project. Do not edit.
[package]
name = "{name}-admin-ui"
version = "0.0.0"
edition = "{edition}"
publish = false

[workspace]

[dependencies]
{dependencies}

# A bundle is downloaded: it is compiled for size.
[profile.release]
opt-level = "z"
lto = true
codegen-units = 1
panic = "abort"
strip = true
"#,
        name = project.name,
        edition = project.edition,
        dependencies = dependencies.join("\n"),
    );

    let modules: String = pages
        .modules
        .iter()
        .map(|(module, file)| format!("#[path = {:?}]\nmod {module};\n", file.display().to_string()))
        .collect();
    let views: String = pages
        .views
        .iter()
        .map(|view| format!("        <{view} as openshop::ui::PageView>::page_view(),\n"))
        .collect();
    let main = format!(
        r#"//! The admin UI bundle of {name}: the modules that declare its admin pages,
//! compiled on their own. Written by `openshop` from the project. Do not edit.

// What the server runs is not compiled here, which leaves what only it uses unused.
#![allow(dead_code, unused_imports)]

{modules}
fn main() {{
    openshop::ui::mount([
{views}    ]);
}}
"#,
        name = project.name,
    );

    // The bundle is rebuilt when the pages change, not when cargo writes to `target/`.
    let mut watched = vec!["src".to_string(), "Cargo.toml".to_string()];
    for (module, file) in &pages.modules {
        let directory = cwd.join("src").join(module);
        if directory.is_dir() {
            watched.push(directory.display().to_string());
        }
        if file.file_name().is_some_and(|name| name != "mod.rs") {
            watched.push(file.display().to_string());
        }
    }
    // A crate on disk may be part of the bundle: the framework itself, when it is one.
    for directory in &project.local_crates {
        watched.push(directory.join("src").display().to_string());
    }
    let watched: Vec<String> = watched.iter().map(|path| toml_string(path)).collect();
    let trunk = format!(
        r#"# Written by `openshop` from the project. Do not edit.
[build]
target = "index.html"
dist = {dist}
public_url = "/"
# The server rewrites head tags of index.html at serve time: keep them byte-for-byte.
minify = "never"
no_sri = true

[watch]
watch = [{watched}]
"#,
        dist = toml_string(&dist(cwd).display().to_string()),
        watched = watched.join(", "),
    );

    vec![
        ("Cargo.toml", manifest),
        ("src/main.rs", main),
        ("index.html", INDEX_HTML.to_string()),
        ("Trunk.toml", trunk),
    ]
}

/// Where the bundle of the project is built.
pub fn dist(cwd: &Path) -> PathBuf {
    cwd.join("dist")
}

/// The crate of the bundle, written and ready to build.
pub struct Prepared {
    pub directory: PathBuf,
    /// Whether what trunk watches changed: a running `trunk watch` is started again.
    pub watch_changed: bool,
}

/// Writes the crate of the bundle of the project. `None` when no admin page
/// has a view: the admin UI is then the one built into the framework.
pub async fn prepare(cwd: &Path) -> Result<Option<Prepared>> {
    let pages = scan(cwd)?;
    if pages.views.is_empty() {
        return Ok(None);
    }

    let metadata = metadata::read(cwd).await?;
    let manifest = cwd.join("Cargo.toml");
    let project = project(&metadata, &manifest.canonicalize().unwrap_or(manifest))?;

    let directory = project.target_directory.join("openshop-ui").join(&project.name);
    let mut watch_changed = false;
    for (path, content) in files(&project, &pages, cwd) {
        let file = directory.join(path);
        if std::fs::read_to_string(&file).is_ok_and(|current| current == content) {
            continue;
        }
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&file, content).with_context(|| format!("[openshop] Cannot write {}", file.display()))?;
        watch_changed |= path == "Trunk.toml";
    }

    // The bundle is compiled with the versions the project is locked to.
    let locked = project.workspace_root.join("Cargo.lock");
    let lock = directory.join("Cargo.lock");
    let modified = |path: &Path| std::fs::metadata(path).and_then(|metadata| metadata.modified()).ok();
    if locked.is_file() && modified(&lock) < modified(&locked) {
        std::fs::copy(&locked, &lock)?;
    }

    Ok(Some(Prepared {
        directory,
        watch_changed,
    }))
}

/// Rebuilds the bundle whenever its pages change.
pub fn watch(prepared: &Prepared) -> Option<Child> {
    match Command::new("trunk")
        .arg("watch")
        .current_dir(&prepared.directory)
        .kill_on_drop(true)
        .spawn()
    {
        Ok(child) => {
            println!("[openshop] Building the admin UI bundle (trunk watch)");
            Some(child)
        }
        Err(error) => {
            eprintln!(
                "[openshop] Could not run trunk ({error}); the admin UI bundle is not rebuilt. Install it with `cargo install trunk`."
            );
            None
        }
    }
}

/// Builds the bundle of the project for production. `false` when the project
/// has none to build.
pub async fn build(cwd: &Path) -> Result<bool> {
    let Some(prepared) = prepare(cwd).await? else {
        return Ok(false);
    };
    println!("[openshop] Building the admin UI bundle (trunk build --release)");
    let status = Command::new("trunk")
        .args(["build", "--release"])
        .current_dir(&prepared.directory)
        .status()
        .await
        .context("[openshop] Could not run trunk. Install it with `cargo install trunk`.")?;
    if !status.success() {
        bail!("{HINT}\ntrunk build --release exited with {status}");
    }
    println!("[openshop] Admin UI bundle built in {}", dist(cwd).display());
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openshop::serde_json::json;

    /// A project in an empty directory, removed when the test ends.
    struct Sources(PathBuf);

    impl Sources {
        fn new(name: &str, files: &[(&str, &str)]) -> Self {
            let directory = std::env::temp_dir().join(format!("openshop-bundle-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&directory);
            for (path, content) in files {
                let file = directory.join(path);
                std::fs::create_dir_all(file.parent().unwrap()).unwrap();
                std::fs::write(file, content).unwrap();
            }
            Self(directory)
        }
    }

    impl Drop for Sources {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const REVIEWS: &str = r#"
        use leptos::prelude::*;
        use openshop::prelude::*;
        use serde::{Deserialize, Serialize};

        #[derive(Clone, Serialize, Deserialize)]
        pub struct Review { pub at: chrono::DateTime<chrono::Utc> }

        pub struct ReviewsPage;

        #[admin_page("/reviews", nav = "Reviews")]
        impl ReviewsPage {
            #[loader]
            async fn list_reviews(ctx: AdminContext) -> Result<Vec<Review>> {
                use crate::models::review::Review as Row;
                let rows = sqlx::query_as::<_, Row>("…").fetch_all(&ctx.db).await?;
                Ok(rows.into_iter().map(Into::into).collect())
            }

            fn helper() -> uuid::Uuid { uuid::Uuid::new_v4() }

            #[view]
            fn view() -> impl IntoView {
                let reviews = use_loader(Self::LIST_REVIEWS, || ());
                view! { <s-page>{move || reviews.data.get().map(|list| charts::bar(list.len()))}</s-page> }
            }
        }
    "#;

    #[test]
    fn a_bundle_is_made_of_the_modules_that_declare_views() {
        let project = Sources::new(
            "scan",
            &[
                ("src/main.rs", "mod admin;\nmod flows;\nfn main() {}\n"),
                ("src/admin/mod.rs", "pub mod reviews;\npub mod settings;\n"),
                ("src/admin/reviews.rs", REVIEWS),
                // A page without a view is the server's alone.
                (
                    "src/admin/settings.rs",
                    "pub struct SettingsPage;\n#[admin_page(\"/settings\")]\nimpl SettingsPage {\n    #[loader]\n    async fn settings(ctx: AdminContext) -> Result<Value> { reqwest::get(\"…\").await }\n}\n",
                ),
                // Not a module of pages: it may name anything.
                ("src/flows/mod.rs", "use tokio::time::sleep;\n"),
                // Another binary of the project.
                ("src/bin/tool.rs", "fn main() {}\n"),
                (
                    "src/pages.rs",
                    "pub mod nested {\n    pub struct InlinePage;\n    #[openshop::admin_page(\"/inline\")]\n    impl InlinePage {\n        #[view]\n        fn view() -> impl IntoView {}\n    }\n}\n",
                ),
            ],
        );

        let pages = scan(&project.0).unwrap();
        assert_eq!(
            pages.views,
            ["admin::reviews::ReviewsPage", "pages::nested::InlinePage"]
        );
        assert_eq!(
            pages.modules,
            [
                ("admin".to_string(), project.0.join("src/admin/mod.rs")),
                ("pages".to_string(), project.0.join("src/pages.rs")),
            ]
        );
        // What the view and the types of the page name, and nothing of what
        // only the server functions do: `sqlx`, `uuid`, `reqwest`, `tokio`.
        let crates: Vec<&str> = pages.crates.iter().map(String::as_str).collect();
        assert_eq!(crates, ["charts", "chrono", "leptos", "openshop", "serde"]);
    }

    #[test]
    fn a_project_without_a_view_has_no_bundle() {
        let project = Sources::new(
            "none",
            &[
                ("src/main.rs", "mod admin;\nfn main() {}\n"),
                (
                    "src/admin.rs",
                    "pub struct P;\n#[admin_page(\"/p\")]\nimpl P {\n    #[loader]\n    async fn list(ctx: AdminContext) -> Result<Value> { todo!() }\n}\n",
                ),
            ],
        );
        assert_eq!(scan(&project.0).unwrap(), Pages::default());
        assert_eq!(scan(&project.0.join("missing")).unwrap(), Pages::default());
    }

    #[test]
    fn refuses_what_the_bundle_cannot_be_made_of() {
        let page =
            "pub struct P;\n#[admin_page(\"/p\")]\nimpl P {\n    #[view]\n    fn view() -> impl IntoView {}\n}\n";
        let root = Sources::new("root", &[("src/main.rs", page)]);
        let error = scan(&root.0).unwrap_err().to_string();
        assert!(
            error.starts_with(
                "[openshop] src/main.rs declares `P` with a `#[view]`. A page with a view is declared in a module"
            ),
            "{error}"
        );

        let broken = Sources::new(
            "broken",
            &[
                ("src/admin.rs", "pub struct P;\n#[admin_page(\"/p\")]\nimpl P {\n"),
                // What is not a module of pages is left to the compiler.
                ("src/flows.rs", "fn broken( {\n"),
            ],
        );
        let error = scan(&broken.0).unwrap_err().to_string();
        assert!(error.starts_with("[openshop] src/admin.rs:"), "{error}");
    }

    fn metadata() -> Value {
        json!({
            "packages": [
                { "name": "other", "edition": "2021", "manifest_path": "/work/other/Cargo.toml", "dependencies": [] },
                { "name": "my-charts", "id": "my-charts", "source": null, "manifest_path": "/work/charts/Cargo.toml" },
                {
                    "name": "my-app",
                    "id": "my-app",
                    "edition": "2024",
                    "manifest_path": "/work/my-app/Cargo.toml",
                    "dependencies": [
                        { "name": "openshop", "source": "git+https://github.com/0xtlt/openshop-rust?tag=v1.2.3", "req": "*", "kind": null, "rename": null, "optional": false, "uses_default_features": true, "features": ["ui"], "target": null },
                        { "name": "leptos", "source": "registry+https://github.com/rust-lang/crates.io-index", "req": "^0.8", "kind": null, "rename": null, "optional": false, "uses_default_features": true, "features": [], "target": null },
                        { "name": "serde", "source": "registry+https://github.com/rust-lang/crates.io-index", "req": "^1", "kind": null, "rename": null, "optional": false, "uses_default_features": false, "features": ["derive", "rc"], "target": null },
                        { "name": "my-charts", "source": null, "req": "*", "kind": null, "rename": "charts", "optional": false, "uses_default_features": true, "features": [], "target": null, "path": "/work/charts" },
                        { "name": "sqlx", "source": "registry+https://github.com/rust-lang/crates.io-index", "req": "^0.8", "kind": null, "rename": null, "optional": false, "uses_default_features": true, "features": [], "target": null },
                        { "name": "gloo-timers", "source": "registry+https://github.com/rust-lang/crates.io-index", "req": "^0.3", "kind": null, "rename": null, "optional": false, "uses_default_features": true, "features": [], "target": "cfg(target_arch = \"wasm32\")" },
                        { "name": "tokio", "source": "registry+https://github.com/rust-lang/crates.io-index", "req": "^1", "kind": null, "rename": null, "optional": false, "uses_default_features": true, "features": [], "target": "cfg(not(target_arch = \"wasm32\"))" },
                        { "name": "openshop", "source": "git+https://github.com/0xtlt/openshop-rust?tag=v1.2.3", "req": "*", "kind": "dev", "rename": null, "optional": false, "uses_default_features": true, "features": ["testing"], "target": null }
                    ]
                }
            ],
            "resolve": { "nodes": [
                { "id": "my-app", "deps": [{ "name": "charts", "pkg": "my-charts", "dep_kinds": [{ "kind": null, "target": null }] }] },
                { "id": "my-charts", "deps": [] }
            ] },
            "target_directory": "/work/target",
            "workspace_root": "/work"
        })
    }

    #[test]
    fn reads_the_project_from_cargo_metadata() {
        let project = project(&metadata(), Path::new("/work/my-app/Cargo.toml")).unwrap();
        assert_eq!((project.name.as_str(), project.edition.as_str()), ("my-app", "2024"));
        assert_eq!(project.target_directory, Path::new("/work/target"));
        assert_eq!(project.workspace_root, Path::new("/work"));
        assert_eq!(project.local_crates, [Path::new("/work/charts")]);
        let lines: Vec<&str> = project.dependencies.iter().map(|(_, line)| line.as_str()).collect();
        // Not the dependencies of tests, nor the ones of the server alone.
        assert_eq!(
            lines,
            [
                r#"openshop = { git = "https://github.com/0xtlt/openshop-rust", tag = "v1.2.3", features = ["ui"] }"#,
                r#"leptos = { version = "^0.8" }"#,
                r#"serde = { version = "^1", default-features = false, features = ["derive", "rc"] }"#,
                r#"charts = { path = "/work/charts", package = "my-charts" }"#,
                r#"sqlx = { version = "^0.8" }"#,
                r#"gloo-timers = { version = "^0.3" }"#,
            ]
        );
        assert_eq!(project.dependencies[5].0, "gloo_timers");

        let error = project_error(Path::new("/work/elsewhere/Cargo.toml"));
        assert_eq!(
            error,
            "[openshop] /work/elsewhere/Cargo.toml is not the manifest of a package"
        );
    }

    fn project_error(manifest: &Path) -> String {
        project(&metadata(), manifest).unwrap_err().to_string()
    }

    #[test]
    fn writes_the_crate_of_the_bundle() {
        let cwd = Path::new("/work/my-app");
        let project = project(&metadata(), &cwd.join("Cargo.toml")).unwrap();
        let pages = Pages {
            views: vec!["admin::reviews::ReviewsPage".to_string()],
            modules: vec![("admin".to_string(), cwd.join("src/admin/mod.rs"))],
            crates: ["charts", "serde", "std"].map(str::to_string).into(),
        };
        let files = files(&project, &pages, cwd);
        let file = |name: &str| {
            files
                .iter()
                .find(|(path, _)| *path == name)
                .map(|(_, content)| content.as_str())
                .unwrap()
        };

        // `openshop` and `leptos`, and the crates the pages name: not `sqlx`.
        let manifest = file("Cargo.toml");
        assert!(manifest.contains("name = \"my-app-admin-ui\"\nversion = \"0.0.0\"\nedition = \"2024\""));
        assert!(manifest.contains(
            "[dependencies]\nopenshop = { git = \"https://github.com/0xtlt/openshop-rust\", tag = \"v1.2.3\", features = [\"ui\"] }\nleptos = { version = \"^0.8\" }\nserde = { version = \"^1\", default-features = false, features = [\"derive\", \"rc\"] }\ncharts = { path = \"/work/charts\", package = \"my-charts\" }\n\n"
        ));
        assert!(manifest.contains("[workspace]\n") && manifest.contains("opt-level = \"z\""));

        let main = file("src/main.rs");
        assert!(main.contains("#[path = \"/work/my-app/src/admin/mod.rs\"]\nmod admin;\n"));
        assert!(main.contains(
            "    openshop::ui::mount([\n        <admin::reviews::ReviewsPage as openshop::ui::PageView>::page_view(),\n    ]);"
        ));

        let trunk = file("Trunk.toml");
        assert!(trunk.contains("dist = \"/work/my-app/dist\""));
        // The bundle is built again when a crate on disk it may be compiled with changes.
        assert!(
            trunk.contains("watch = [\"src\", \"Cargo.toml\", \"/work/charts/src\"]"),
            "{trunk}"
        );
        assert!(file("index.html").contains("<meta name=\"shopify-api-key\" content=\"\" />"));
    }

    #[test]
    fn the_shell_is_the_one_of_the_framework() {
        let framework = Path::new(env!("CARGO_MANIFEST_DIR")).join("../openshop-ui/index.html");
        assert_eq!(std::fs::read_to_string(framework).unwrap(), INDEX_HTML);
    }
}
