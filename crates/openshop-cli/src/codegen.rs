//! `openshop codegen`: downloads the Shopify Admin GraphQL schema that
//! `graphql!` checks operations against.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use openshop::Value;
use openshop::shopify::SHOPIFY_API_VERSION;
use openshop_graphql::{INTROSPECTION_QUERY, PROJECT_SCHEMA_PATH, Schema};

/// Shopify's public copy of the Admin schema, as used by its own codegen preset.
const SCHEMA_ENDPOINT: &str = "https://shopify.dev/admin-graphql-direct-proxy";

#[derive(Debug, PartialEq, Eq)]
pub struct CodegenOptions {
    pub api_version: String,
    pub out: PathBuf,
}

/// `YYYY-MM`, or `unstable`.
fn is_api_version(version: &str) -> bool {
    let bytes = version.as_bytes();
    let dated = bytes.len() == 7 && bytes[4] == b'-' && bytes[..4].iter().chain(&bytes[5..]).all(u8::is_ascii_digit);
    dated || version == "unstable"
}

pub fn parse_options(args: &[String]) -> Result<CodegenOptions> {
    let mut options = CodegenOptions {
        api_version: SHOPIFY_API_VERSION.to_string(),
        out: PathBuf::from(PROJECT_SCHEMA_PATH),
    };
    for arg in args {
        if let Some(version) = arg.strip_prefix("--api-version=") {
            if !is_api_version(version) {
                bail!("[openshop] --api-version must look like 2026-04, got \"{version}\"");
            }
            options.api_version = version.to_string();
        } else if let Some(out) = arg.strip_prefix("--out=").filter(|out| !out.is_empty()) {
            options.out = PathBuf::from(out);
        } else {
            bail!(
                "[openshop] Unknown codegen option \"{arg}\". Usage: openshop codegen [--api-version=YYYY-MM] [--out=<file>]"
            );
        }
    }
    Ok(options)
}

async fn download(api_version: &str) -> Result<Value> {
    let url = format!("{SCHEMA_ENDPOINT}/{api_version}");
    let response = reqwest::Client::new()
        .post(&url)
        .json(&openshop::json!({ "query": INTROSPECTION_QUERY }))
        .send()
        .await
        .with_context(|| format!("[openshop] Could not reach {url}"))?;
    let status = response.status();
    if !status.is_success() {
        bail!("[openshop] {url} answered {status}. Is {api_version} a Shopify API version?");
    }
    response
        .json()
        .await
        .with_context(|| format!("[openshop] {url} did not answer with JSON"))
}

/// Stores the schema of an introspection result. Returns the file and the number of types.
pub fn write_schema(cwd: &Path, options: &CodegenOptions, introspection: &Value) -> Result<(PathBuf, usize)> {
    let schema = Schema::from_introspection(introspection, &options.api_version)
        .map_err(|error| anyhow::anyhow!("[openshop] {error}"))?;
    let path = cwd.join(&options.out);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, schema.to_json())
        .with_context(|| format!("[openshop] Could not write {}", path.display()))?;
    Ok((path, schema.types.len()))
}

pub async fn run_codegen(cwd: &Path, args: &[String]) -> Result<()> {
    let options = parse_options(args)?;
    println!(
        "[openshop] Downloading the Shopify Admin GraphQL schema {}...",
        options.api_version
    );
    let introspection = download(&options.api_version).await?;
    let (path, types) = write_schema(cwd, &options, &introspection)?;
    println!("[openshop] Wrote {} ({types} types)", options.out.display());

    if options.out == Path::new(PROJECT_SCHEMA_PATH) {
        println!(
            "[openshop] `graphql!` now checks this project's operations against the Admin API {}.",
            options.api_version
        );
        if options.api_version == SHOPIFY_API_VERSION {
            println!("[openshop] This is the version bundled with openshop: the file is only needed for another one.");
        } else {
            println!(
                "[openshop] Commit the file, and create the Shopify clients that run those operations with the same version."
            );
        }
    } else {
        println!(
            "[openshop] Set OPENSHOP_GRAPHQL_SCHEMA={} to check operations against it.",
            path.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use openshop::json;

    fn args(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| arg.to_string()).collect()
    }

    #[test]
    fn defaults_to_the_framework_version_and_the_project_file() {
        assert_eq!(
            parse_options(&[]).unwrap(),
            CodegenOptions {
                api_version: SHOPIFY_API_VERSION.to_string(),
                out: PathBuf::from("graphql/admin.schema.json"),
            }
        );
        assert_eq!(
            parse_options(&args(&["--api-version=2025-10", "--out=schemas/admin.json"])).unwrap(),
            CodegenOptions {
                api_version: "2025-10".to_string(),
                out: PathBuf::from("schemas/admin.json"),
            }
        );
        assert_eq!(
            parse_options(&args(&["--api-version=unstable"])).unwrap().api_version,
            "unstable"
        );
    }

    #[test]
    fn rejects_invalid_options() {
        for version in ["2026", "2026-4", "latest", "2026-04/../x", ""] {
            let error = parse_options(&args(&[&format!("--api-version={version}")])).unwrap_err();
            assert_eq!(
                error.to_string(),
                format!("[openshop] --api-version must look like 2026-04, got \"{version}\"")
            );
        }
        let error = parse_options(&args(&["--watch"])).unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with("[openshop] Unknown codegen option \"--watch\".")
        );
    }

    #[test]
    fn writes_the_schema_of_an_introspection_result() {
        let dir = std::env::temp_dir().join(format!("openshop-codegen-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let options = parse_options(&args(&["--api-version=2025-10"])).unwrap();
        let introspection = json!({ "data": { "__schema": {
            "queryType": { "name": "QueryRoot" },
            "mutationType": null,
            "types": [
                { "kind": "OBJECT", "name": "QueryRoot", "interfaces": [], "fields": [{
                    "name": "ping",
                    "args": [],
                    "type": { "kind": "SCALAR", "name": "String", "ofType": null },
                }] },
                { "kind": "SCALAR", "name": "String" },
            ],
        } } });

        let (path, types) = write_schema(&dir, &options, &introspection).unwrap();
        assert_eq!(path, dir.join("graphql/admin.schema.json"));
        assert_eq!(types, 2);
        let schema = Schema::from_json(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(schema.api_version, "2025-10");
        assert_eq!(schema.mutation, None);

        let error = write_schema(&dir, &options, &json!({ "errors": [{ "message": "Not Found" }] })).unwrap_err();
        assert_eq!(error.to_string(), "[openshop] Introspection failed: Not Found");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
