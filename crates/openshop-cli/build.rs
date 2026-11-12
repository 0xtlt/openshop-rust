//! Embeds the templates of `openshop init`: `$OUT_DIR/templates.rs` lists the
//! files a project gets from each, with their content, the Admin schema of
//! `graphql!` every project gets, and the manifest of the workspace, which the
//! `workspace = true` of a template stands for. It also sets
//! `OPENSHOP_GIT_COMMIT`, the commit of the repository the CLI is built from,
//! when there is one.
//!
//! Built in the repository (`cargo install --git`, `--path`, the workspace), it
//! reads them where they are: `templates/`, `Cargo.toml` and
//! `crates/openshop/graphql` at its root. Built from its package, as crates.io
//! builds it, it reads them in `bundle/`, the copy `bundle.sh` makes of them:
//! `cargo package` takes nothing outside `crates/openshop-cli`.

use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The templates, each a crate of the workspace.
const TEMPLATES: &[&str] = &["minimal", "custom", "public"];
/// What a project gets from its template. The rest of the directory, such as
/// `examples/` and `shopify.web.toml`, serves the repository.
const TEMPLATE_DIRS: &[&str] = &["src", "tests", "migrations"];
const TEMPLATE_FILES: &[&str] = &["Cargo.toml", "README.md", "shopify.app.toml"];
/// The name of the manifest of a template in `bundle/`: cargo leaves out of a
/// package every directory with a `Cargo.toml`, the root of another package.
const BUNDLED_MANIFEST: &str = "Cargo.toml.template";

/// Where the files the CLI embeds are read from.
struct Sources {
    /// `templates/` of the repository, or of `bundle/`.
    templates: PathBuf,
    /// The manifest of the workspace.
    workspace_manifest: PathBuf,
    /// The Admin schema of `graphql!`.
    schema: PathBuf,
    /// The root of the repository, `None` for the package.
    repository: Option<PathBuf>,
}

impl Sources {
    fn find(package: &Path) -> Sources {
        let root = package.join("../..");
        // In the repository, `crates/openshop-cli` of its root is this package.
        if let (Ok(this), Ok(member)) = (package.canonicalize(), root.join("crates/openshop-cli").canonicalize())
            && this == member
        {
            let root = root.canonicalize().unwrap();
            return Sources {
                templates: root.join("templates"),
                workspace_manifest: root.join("Cargo.toml"),
                schema: root.join("crates/openshop/graphql/admin.schema.json"),
                repository: Some(root),
            };
        }
        let bundle = package.join("bundle");
        assert!(
            bundle.is_dir(),
            "openshop-cli embeds the templates of `templates/` at the root of the OpenShop repository, \
             or the copy `crates/openshop-cli/bundle.sh` makes of them in {}, and neither exists: build it \
             from a checkout, as `cargo install --git https://github.com/0xtlt/openshop-rust openshop-cli` \
             does, or run `bundle.sh` before `cargo package`",
            bundle.display()
        );
        Sources {
            templates: bundle.join("templates"),
            workspace_manifest: bundle.join("workspace.toml"),
            schema: bundle.join("admin.schema.json"),
            repository: None,
        }
    }

    /// The file `name` of the template in `root`, renamed in the bundle if it
    /// is its manifest.
    fn template_file(&self, root: &Path, name: &str) -> PathBuf {
        match name {
            "Cargo.toml" if self.repository.is_none() => root.join(BUNDLED_MANIFEST),
            _ => root.join(name),
        }
    }
}

/// A file a project gets: its path in the project, and the file it is read from.
type File = (String, PathBuf);

/// The files of `directory` and below, by their path from `root`. A dotfile
/// is stored as `_name`, so that it does not apply to the repository.
fn files_in(root: &Path, directory: &Path, found: &mut Vec<File>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files_in(root, &path, found);
        } else if entry.file_name() != ".DS_Store" {
            let relative = path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            let relative = match relative.strip_prefix('_') {
                Some(dotfile) => format!(".{dotfile}"),
                None => relative,
            };
            found.push((relative, path));
        }
    }
}

/// `files` as a Rust slice of `(path in the project, content)`.
fn slice(name: &str, files: &[File]) -> String {
    let mut code = format!("pub const {name}: &[(&str, &str)] = &[\n");
    for (path, file) in files {
        writeln!(code, "    ({path:?}, include_str!({:?})),", file.display().to_string()).unwrap();
    }
    code + "];\n"
}

/// The output of `git <args>` in `directory`, when it succeeds.
fn git(directory: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git").arg("-C").arg(directory).args(args).output().ok()?;
    let text = String::from_utf8(output.stdout).ok()?;
    (output.status.success() && !text.trim().is_empty()).then(|| text.trim().to_string())
}

/// The commit the CLI is built from: `HEAD` of the repository whose root is
/// `workspace`. `cargo install --git` builds from a checkout of its own, with
/// its `.git`, at the commit it installs. `None` for a source without git, or
/// inside another repository (a registry unpacked in a versioned home
/// directory, say).
fn commit(workspace: &Path) -> Option<String> {
    let top = PathBuf::from(git(workspace, &["rev-parse", "--show-toplevel"])?);
    if top.canonicalize().ok()? != workspace.canonicalize().ok()? {
        return None;
    }
    // A new commit, or another branch, is a new build: nothing else is. The
    // branches are watched as a directory, since the file of the current one
    // may not exist yet (it is packed until it gets a commit).
    for name in ["HEAD", "packed-refs", "refs/heads"] {
        if let Some(path) = git(workspace, &["rev-parse", "--git-path", name]) {
            let path = workspace.join(path);
            if path.exists() {
                println!("cargo::rerun-if-changed={}", path.display());
            }
        }
    }
    let commit = git(workspace, &["rev-parse", "HEAD"])?;
    (commit.len() == 40 && commit.chars().all(|character| character.is_ascii_hexdigit())).then_some(commit)
}

fn main() {
    let sources = Sources::find(Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()));
    let mut code = String::new();

    for template in TEMPLATES {
        let root = sources.templates.join(template);
        let mut files = Vec::new();
        for directory in TEMPLATE_DIRS {
            println!("cargo::rerun-if-changed={}", root.join(directory).display());
            files_in(&root, &root.join(directory), &mut files);
        }
        for file in TEMPLATE_FILES {
            let path = sources.template_file(&root, file);
            println!("cargo::rerun-if-changed={}", path.display());
            files.push((file.to_string(), path));
        }
        files.sort();
        code += &slice(&template.to_uppercase(), &files);
    }

    let shared = sources.templates.join("shared");
    println!("cargo::rerun-if-changed={}", shared.display());
    let mut files = Vec::new();
    files_in(&shared, &shared, &mut files);
    files.sort();
    // The schema `graphql!` checks the operations of a project against: the
    // one of the version the framework and every template pin, which the
    // templates link to.
    println!("cargo::rerun-if-changed={}", sources.schema.display());
    files.push(("graphql/admin.schema.json".to_string(), sources.schema.clone()));
    code += &slice("SHARED", &files);

    println!("cargo::rerun-if-changed={}", sources.workspace_manifest.display());
    writeln!(
        code,
        "pub const WORKSPACE_MANIFEST: &str = include_str!({:?});",
        sources.workspace_manifest.display().to_string()
    )
    .unwrap();
    // Built from its package, the CLI is the one of crates.io, which has
    // `openshop` of its version.
    writeln!(code, "pub const PACKAGED: bool = {};", sources.repository.is_none()).unwrap();

    if let Some(commit) = sources.repository.as_deref().and_then(commit) {
        println!("cargo::rustc-env=OPENSHOP_GIT_COMMIT={commit}");
    }

    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("templates.rs");
    std::fs::write(out, code).unwrap();
}
