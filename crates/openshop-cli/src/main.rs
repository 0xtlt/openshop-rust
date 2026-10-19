//! `openshop`: scaffold and develop OpenShop applications.
#![forbid(unsafe_code)]

mod bundle;
mod codegen;
mod dev;
mod init;
mod metadata;
mod restart;

use std::process::ExitCode;

use anyhow::{Result, bail};
use tokio::process::Command;

const USAGE: &str = "
  openshop - Shopify integration framework

  Commands:
    init [<dir>]                Scaffold a new OpenShop project from a template; asks for what
                                it is not given (--template=minimal|custom|public)
    init-ui                     Scaffold a custom admin page with its view
    dev                         Start the app in dev mode and restart it when sources change
    codegen                     Download the Shopify Admin GraphQL schema of another API version
                                for graphql! (--api-version=YYYY-MM, --out=<file>)
    worker [--concurrency=N]    Start worker only (production, scalable)
    migrate                     Apply client-owned migrations from ./migrations
    migrate generate            Generate a new client-owned migration
    migrate check               Check generated migration history
    migrate status              Show client migration status
    build                       Build for production: the admin UI bundle, if any, then the app
    build-ui                    Build the admin UI bundle alone, into dist/
    start                       Start production server (API + UI, no worker)
    test [args]                 Run tests

  Usage:
    openshop init
    openshop init my-app --template=public
    openshop dev
    openshop worker --concurrency=10
    openshop migrate generate
    openshop migrate
    openshop migrate check
    openshop migrate status
    openshop test
    openshop build && openshop start

  Everything except `init`, `init-ui`, `dev`, `build-ui` and `codegen` runs the
  application binary through cargo in the current project.
";

/// Runs cargo in the current directory and forwards its exit status.
async fn cargo(args: &[&str], passthrough: &[String]) -> Result<()> {
    let status = Command::new("cargo").args(args).args(passthrough).status().await?;
    if !status.success() {
        bail!("cargo {} exited with {status}", args.join(" "));
    }
    Ok(())
}

async fn run(args: &[String]) -> Result<()> {
    let rest = args.get(1..).unwrap_or_default();
    match args.first().map(String::as_str) {
        Some("init") => {
            init::run_init(&std::env::current_dir()?, rest)?;
            Ok(())
        }
        Some("init-ui") => init::run_init_ui(&std::env::current_dir()?),
        Some("dev") => dev::start_dev(rest).await,
        Some("codegen") => codegen::run_codegen(&std::env::current_dir()?, rest).await,
        Some("build") => {
            bundle::build(&std::env::current_dir()?).await?;
            cargo(&["build", "--release"], rest).await
        }
        Some("build-ui") => {
            if !bundle::build(&std::env::current_dir()?).await? {
                println!("[openshop] No admin page has a view: the admin UI is the one built into openshop.");
            }
            Ok(())
        }
        Some("test") => cargo(&["test"], rest).await,
        Some("migrate") => cargo(&["run", "--", "migrate"], rest).await,
        Some(command @ ("start" | "worker")) => cargo(&["run", "--release", "--", command], rest).await,
        _ => {
            println!("{USAGE}");
            Ok(())
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::FAILURE
        }
    }
}
