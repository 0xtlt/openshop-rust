//! Embeds the templates of `openshop init`, from `templates/` at the root of
//! the repository: `$OUT_DIR/templates.rs` lists the files a project gets from
//! each, with their content.

use std::fmt::Write;
use std::path::{Path, PathBuf};

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

fn main() {
    let templates = Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../templates");
    let templates = templates
        .canonicalize()
        .expect("templates/ at the root of the repository");
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
    code += &slice("SHARED", &shared, &files);

    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("templates.rs");
    std::fs::write(out, code).unwrap();
}
