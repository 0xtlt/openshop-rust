//! What `cargo metadata` says of the project.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::Value;
use tokio::process::Command;

/// Runs `cargo metadata` in the project.
pub async fn read(cwd: &Path) -> Result<Value> {
    let output = Command::new("cargo")
        .args(["metadata", "--format-version", "1"])
        .current_dir(cwd)
        .output()
        .await
        .context("[openshop] Failed to run cargo")?;
    if !output.status.success() {
        bail!(
            "[openshop] cargo metadata failed:\n{}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    serde_json::from_slice(&output.stdout).context("[openshop] Cannot read cargo metadata")
}

fn packages(metadata: &Value) -> impl Iterator<Item = &Value> {
    metadata["packages"].as_array().into_iter().flatten()
}

fn id(package: &Value) -> Option<&str> {
    package["id"].as_str()
}

/// The package of `manifest`.
pub fn package<'a>(metadata: &'a Value, manifest: &Path) -> Option<&'a Value> {
    let same =
        |path: &str| Path::new(path).canonicalize().is_ok_and(|path| path == manifest) || Path::new(path) == manifest;
    packages(metadata).find(|package| package["manifest_path"].as_str().is_some_and(same))
}

/// The directories of the crates on disk the project is compiled with, beside
/// the one of `manifest`: path dependencies and theirs, members of the
/// workspace or not.
///
/// The project is the package `cargo run` is asked for with `-p`, the one of
/// `manifest` otherwise, and the default members of a workspace whose root has
/// none.
pub fn local_crates(metadata: &Value, manifest: &Path, requested: Option<&str>) -> Vec<PathBuf> {
    let here = package(metadata, manifest).and_then(id);
    let run: Vec<&str> = match requested {
        Some(name) => packages(metadata)
            .filter(|package| package["source"].is_null() && package["name"] == name)
            .filter_map(id)
            .collect(),
        None if here.is_some() => here.into_iter().collect(),
        None => metadata["workspace_default_members"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect(),
    };
    let nodes: HashMap<&str, &Value> = metadata["resolve"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|node| Some((node["id"].as_str()?, node)))
        .collect();

    let mut compiled: HashSet<&str> = run.iter().copied().collect();
    let mut next = run;
    while let Some(id) = next.pop() {
        let dependencies = nodes.get(id).and_then(|node| node["deps"].as_array());
        for dependency in dependencies.into_iter().flatten() {
            // What only the tests of a crate need is not compiled to run the project.
            let kinds = dependency["dep_kinds"].as_array();
            if kinds.is_some_and(|kinds| kinds.iter().all(|kind| kind["kind"] == "dev")) {
                continue;
            }
            if let Some(package) = dependency["pkg"].as_str()
                && compiled.insert(package)
            {
                next.push(package);
            }
        }
    }

    packages(metadata)
        .filter(|package| package["source"].is_null())
        .filter(|package| id(package).is_some_and(|id| Some(id) != here && compiled.contains(id)))
        .filter_map(|package| Some(Path::new(package["manifest_path"].as_str()?).parent()?.to_path_buf()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn package(name: &str, local: bool) -> Value {
        json!({
            "name": name,
            "id": name,
            "source": if local { Value::Null } else { json!("registry+https://github.com/rust-lang/crates.io-index") },
            "manifest_path": format!("/work/{name}/Cargo.toml"),
        })
    }

    fn node(id: &str, dependencies: &[(&str, &[Option<&str>])]) -> Value {
        let dependencies: Vec<Value> = dependencies
            .iter()
            .map(|(package, kinds)| {
                let kinds: Vec<Value> = kinds
                    .iter()
                    .map(|kind| json!({ "kind": kind, "target": null }))
                    .collect();
                json!({ "name": package, "pkg": package, "dep_kinds": kinds })
            })
            .collect();
        json!({ "id": id, "deps": dependencies })
    }

    #[test]
    fn finds_the_crates_on_disk_a_project_is_compiled_with() {
        let metadata = json!({
            "packages": [
                package("my-app", true),
                package("charts", true),
                package("palette", true),
                package("fixtures", true),
                package("neighbour", true),
                package("serde", false),
            ],
            "workspace_default_members": ["neighbour"],
            "resolve": {
                "nodes": [
                    node("my-app", &[("charts", &[None, Some("dev")]), ("serde", &[None]), ("fixtures", &[Some("dev")])]),
                    // Through a crate of the registry, as a `[patch]` does.
                    node("charts", &[("serde", &[None])]),
                    node("serde", &[("palette", &[Some("build")])]),
                    node("palette", &[("charts", &[None])]),
                    node("fixtures", &[]),
                    node("neighbour", &[("my-app", &[None])]),
                ],
            },
        });

        let crates = |manifest: &str, requested: Option<&str>| {
            let crates = local_crates(&metadata, Path::new(manifest), requested);
            let names = crates
                .iter()
                .map(|directory| directory.strip_prefix("/work").unwrap().display().to_string());
            names.collect::<Vec<_>>().join(" ")
        };

        // Not what the tests alone need, nor a member of the workspace that is not a dependency.
        assert_eq!(crates("/work/my-app/Cargo.toml", None), "charts palette");
        assert_eq!(crates("/work/fixtures/Cargo.toml", None), "");
        // `cargo run -p my-app`, from the crate of another package or from the root of a workspace.
        assert_eq!(
            crates("/work/fixtures/Cargo.toml", Some("my-app")),
            "my-app charts palette"
        );
        assert_eq!(crates("/work/Cargo.toml", Some("my-app")), "my-app charts palette");
        assert_eq!(crates("/work/my-app/Cargo.toml", Some("my-app")), "charts palette");
        assert_eq!(crates("/work/Cargo.toml", Some("unknown")), "");
        // The root of a workspace is not a package: `cargo run` is for its default members.
        assert_eq!(crates("/work/Cargo.toml", None), "my-app charts palette neighbour");
    }
}
