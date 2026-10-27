//! Embeds the templates of `openshop init`, from `templates/` at the root of
//! the repository: `$OUT_DIR/templates.rs` lists the files a project gets from
//! each, with their content, and the Admin schema of `graphql!` every project
//! gets. It also sets `OPENSHOP_GIT_COMMIT`, the commit of
//! the repository the CLI is built from, when there is one.

use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The templates, each a crate of the workspace.
const TEMPLATES: &[&str] = &["minimal", "custom", "public"];
/// What a project gets from its template. The rest of the directory, such as
/// `examples/`, `.gitignore` and `shopify.web.toml`, serves the repository.
const TEMPLATE_DIRS: &[&str] = &["src", "tests", "migrations"];
const TEMPLATE_FILES: &[&str] = &["Cargo.toml", "README.md", "shopify.app.toml"];

/// The files of `directory` and below, sorted.
fn files_in(directory: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files_in(&path, found);
        } else if entry.file_name() != ".DS_Store" {
            found.push(path);
        }
    }
}

/// `files` as a Rust slice of `(path in the project, content)`.
fn slice(name: &str, root: &Path, files: &[PathBuf]) -> String {
    let mut code = format!("pub const {name}: &[(&str, &str)] = &[\n");
    for file in files {
        let relative = file.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
        // A dotfile is stored as `_name`, so that it does not apply to the repository.
        let relative = match relative.strip_prefix('_') {
            Some(dotfile) => format!(".{dotfile}"),
            None => relative,
        };
        writeln!(
            code,
            "    ({relative:?}, include_str!({:?})),",
            file.display().to_string()
        )
        .unwrap();
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
    let templates = Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../templates");
    // The templates are crates of the workspace: the CLI is built from a checkout
    // of the repository, as `cargo install --git` does, never from its package alone.
    let Ok(templates) = templates.canonicalize() else {
        panic!(
            "openshop-cli embeds the templates of `templates/` at the root of the OpenShop repository, \
             and {} does not exist: build it from a checkout, as \
             `cargo install --git https://github.com/0xtlt/openshop-rust openshop-cli` does",
            templates.display()
        );
    };
    let mut code = String::new();

    for template in TEMPLATES {
        let root = templates.join(template);
        let mut files = Vec::new();
        for directory in TEMPLATE_DIRS {
            println!("cargo::rerun-if-changed={}", root.join(directory).display());
            files_in(&root.join(directory), &mut files);
        }
        for file in TEMPLATE_FILES {
            println!("cargo::rerun-if-changed={}", root.join(file).display());
            files.push(root.join(file));
        }
        files.sort();
        code += &slice(&template.to_uppercase(), &root, &files);
    }

    let shared = templates.join("shared");
    println!("cargo::rerun-if-changed={}", shared.display());
    let mut files = Vec::new();
    files_in(&shared, &mut files);
    files.sort();
    // The schema `graphql!` checks the operations of a project against: the
    // one of the version every template pins, written once.
    let schema = templates.join("minimal/graphql/admin.schema.json");
    println!("cargo::rerun-if-changed={}", schema.display());
    let entry = format!(
        "    (\"graphql/admin.schema.json\", include_str!({:?})),\n];\n",
        schema.display().to_string()
    );
    code += &slice("SHARED", &shared, &files).replace("];\n", &entry);

    if let Some(commit) = templates.parent().and_then(commit) {
        println!("cargo::rustc-env=OPENSHOP_GIT_COMMIT={commit}");
    }

    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("templates.rs");
    std::fs::write(out, code).unwrap();
}
