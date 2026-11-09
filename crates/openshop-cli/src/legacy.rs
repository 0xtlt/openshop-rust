//! A project still on the Leptos admin, which the framework no longer has:
//! `openshop dev` says what is left of it and where the migration guide is.

use std::path::Path;

/// What the source of a project names of the Leptos admin: the `UiSource`
/// variants that served its bundle, the module of its views, and Leptos.
const SOURCE_MARKERS: &[&str] = &[
    "UiSource::Dir",
    "UiSource::Assets",
    "UiSource::Embedded",
    "openshop::ui",
    "leptos::",
];

/// What is left of the Leptos admin in the project at `cwd`, one line each:
/// in `Cargo.toml`, `openshop` with the feature `ui` and the dependency
/// `leptos`; in `src/`, the first line of each file that names one of
/// [`SOURCE_MARKERS`]. Empty for a project on the server-rendered admin.
pub fn leftovers(cwd: &Path) -> Vec<String> {
    let mut found = Vec::new();
    if let Ok(manifest) = std::fs::read_to_string(cwd.join("Cargo.toml")) {
        manifest_leftovers(&manifest, &mut found);
    }
    let mut files = Vec::new();
    rust_files(&cwd.join("src"), &mut files);
    files.sort();
    for file in files {
        let Ok(source) = std::fs::read_to_string(&file) else {
            continue;
        };
        let line = source.lines().enumerate().find_map(|(index, line)| {
            let marker = SOURCE_MARKERS.iter().find(|marker| line.contains(*marker))?;
            Some((index + 1, marker))
        });
        if let Some((number, marker)) = line {
            let relative = file.strip_prefix(cwd).unwrap_or(&file);
            let relative = relative.to_string_lossy().replace('\\', "/");
            found.push(format!("{relative}:{number}: {marker}"));
        }
    }
    found
}

/// What `manifest` declares of the Leptos admin, in its dependencies and in
/// those of its workspace.
fn manifest_leftovers(manifest: &str, found: &mut Vec<String>) {
    let Ok(document) = manifest.parse::<toml_edit::DocumentMut>() else {
        return;
    };
    let tables = [
        ("[dependencies]", document.get("dependencies")),
        (
            "[workspace.dependencies]",
            document
                .get("workspace")
                .and_then(|workspace| workspace.get("dependencies")),
        ),
    ];
    for (name, dependencies) in tables {
        let Some(dependencies) = dependencies.and_then(toml_edit::Item::as_table_like) else {
            continue;
        };
        let has_ui = dependencies
            .get("openshop")
            .and_then(|openshop| openshop.get("features"))
            .and_then(toml_edit::Item::as_array)
            .is_some_and(|features| features.iter().any(|feature| feature.as_str() == Some("ui")));
        if has_ui {
            found.push(format!("Cargo.toml: `openshop` with the feature `ui`, in {name}"));
        }
        if dependencies.contains_key("leptos") {
            found.push(format!("Cargo.toml: the dependency `leptos`, in {name}"));
        }
    }
}

/// The `.rs` files of `directory` and below.
fn rust_files(directory: &Path, found: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, found);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            found.push(path);
        }
    }
}

/// What `openshop dev` prints for a project with `leftovers`, before it runs
/// it: cargo, or the admin, fails next.
pub fn warning(leftovers: &[String]) -> String {
    let mut text = String::from(
        "[openshop] This project is still on the Leptos admin, which this version of OpenShop no longer has:\n",
    );
    for leftover in leftovers {
        text.push_str(&format!("[openshop]   {leftover}\n"));
    }
    text.push_str(
        "[openshop] The admin is now rendered by the server, without trunk or a bundle. Move the project with the\n\
         [openshop] guide \"Migrate to the server-rendered admin\" (docs/src/guides/migrate-to-server-rendered-ui.md\n\
         [openshop] of the OpenShop repository), or keep the release it depends on and the CLI of that release.",
    );
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_what_a_project_keeps_of_the_leptos_admin() {
        let dir = std::env::temp_dir().join(format!("openshop-legacy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/admin")).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            r#"[package]
name = "shop-test"

[dependencies]
openshop = { git = "https://github.com/0xtlt/openshop-rust", tag = "v0.0.11", features = ["ui"] }
leptos = "0.8"
"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("src/main.rs"),
            "use openshop::prelude::*;\n\nfn ui() -> UiSource {\n    UiSource::Dir(\"dist\".into())\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("src/admin/home.rs"),
            "use leptos::prelude::*;\nuse openshop::ui::use_loader;\n",
        )
        .unwrap();
        std::fs::write(dir.join("src/admin/mod.rs"), "pub mod home;\n").unwrap();

        assert_eq!(
            leftovers(&dir),
            [
                "Cargo.toml: `openshop` with the feature `ui`, in [dependencies]",
                "Cargo.toml: the dependency `leptos`, in [dependencies]",
                "src/admin/home.rs:1: leptos::",
                "src/main.rs:4: UiSource::Dir",
            ]
        );
        let warning = warning(&leftovers(&dir));
        assert!(
            warning.contains("[openshop]   src/main.rs:4: UiSource::Dir\n"),
            "{warning}"
        );
        assert!(warning.contains("migrate-to-server-rendered-ui.md"), "{warning}");

        // Once migrated: nothing.
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"shop-test\"\n\n[dependencies]\nopenshop = { path = \"../openshop\" }\nmaud = \"0.27\"\n",
        )
        .unwrap();
        std::fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(dir.join("src/admin/home.rs"), "use maud::html;\n").unwrap();
        assert!(leftovers(&dir).is_empty(), "{:?}", leftovers(&dir));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn reads_the_dependencies_of_a_workspace() {
        let mut found = Vec::new();
        manifest_leftovers(
            "[workspace.dependencies]\nopenshop = { version = \"0.0.11\", features = [\"testing\", \"ui\"] }\n",
            &mut found,
        );
        assert_eq!(
            found,
            ["Cargo.toml: `openshop` with the feature `ui`, in [workspace.dependencies]"]
        );
        // A dotted table, and a `ui` that is not the feature of `openshop`.
        found.clear();
        manifest_leftovers(
            "[dependencies.openshop]\nversion = \"0.0.11\"\nfeatures = [\"ui\"]\n\n[dependencies]\nother = { version = \"1\", features = [\"ui\"] }\n",
            &mut found,
        );
        assert_eq!(
            found,
            ["Cargo.toml: `openshop` with the feature `ui`, in [dependencies]"]
        );
    }
}
