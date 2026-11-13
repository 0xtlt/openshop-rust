//! `openshop`: scaffold and develop OpenShop applications.
#![forbid(unsafe_code)]

mod dev;
mod init;
mod legacy;
mod metadata;

use std::process::ExitCode;

use anyhow::{Result, bail};
use tokio::process::Command;

const USAGE: &str = "
  openshop - Shopify integration framework

  Commands:
    init [<dir>]                Scaffold a new OpenShop project from a template; asks for what
                                it is not given (--template=minimal|custom|public). The project
                                depends on the commit this CLI was built from; name another
                                source with --openshop-rev=<commit>, --openshop-tag=<tag>,
                                --openshop-branch=<name> or --openshop-path=<dir>
    init-ui                     Scaffold a custom admin page with its view, which the server renders
    dev                         Start the app in dev mode and restart it when sources change
    worker [--concurrency=N]    Start worker only (production, scalable)
    migrate                     Apply client-owned migrations from ./migrations
    migrate generate            Generate migrations for the schema ./migrations does not cover yet
    migrate check               Check that ./migrations covers the current schema
    migrate status              Show client migration status
    build                       Build the app for production
    start                       Start production server (API + UI + scheduler, no worker)
    run <flow> [--shop=<domain> | --all-shops] [--input=<json>]
                                Run a flow once, through the queue and a worker, against the
                                database of the app, and wait for it
    cron run <name>             Run the flow of a cron once, for the shops of the cron
    test [args]                 Run tests
    --version, -V               Print the version of this CLI, and the commit it was built from

  Usage:
    openshop init
    openshop init my-app --template=public
    openshop dev
    openshop worker --concurrency=10
    openshop migrate generate
    openshop migrate
    openshop migrate check
    openshop migrate status
    openshop run syncOrders --shop=my-store.myshopify.com --input='{\"limit\":10}'
    openshop cron run \"Nightly sync\"
    openshop test
    openshop build && openshop start

  `worker`, `migrate`, `start`, `run` and `cron` run the application binary
  through cargo in the current project; `test` and `build` run `cargo test`
  and `cargo build --release` there.
";

/// Runs cargo in the current directory and forwards its exit status.
async fn cargo(args: &[&str], passthrough: &[String]) -> Result<()> {
    let status = Command::new("cargo").args(args).args(passthrough).status().await?;
    if !status.success() {
        bail!("cargo {} exited with {status}", args.join(" "));
    }
    Ok(())
}

/// `openshop 0.1.0-rc.2 (1b8a53a4cac9)`: the version of the CLI, and the commit
/// of the repository it was built from, when it was built from a checkout
/// (`cargo install --git`, `cargo install --path`).
fn version() -> String {
    let version = env!("CARGO_PKG_VERSION");
    match option_env!("OPENSHOP_GIT_COMMIT") {
        Some(commit) => format!("openshop {version} ({})", &commit[..12]),
        None => format!("openshop {version}"),
    }
}

/// Whether the arguments of a command ask for its usage.
fn asks_for_help(args: &[String]) -> bool {
    args.iter().any(|arg| arg == "--help" || arg == "-h")
}

async fn run(args: &[String]) -> Result<()> {
    let rest = args.get(1..).unwrap_or_default();
    match args.first().map(String::as_str) {
        // `--help` after a command the CLI runs itself is the usage. The
        // others pass their arguments on: to the application binary, which
        // prints the usage of its commands (`run --help`), or to cargo.
        Some("init" | "init-ui") if asks_for_help(rest) => {
            println!("{USAGE}");
            Ok(())
        }
        Some("init") => {
            init::run_init(&std::env::current_dir()?, rest)?;
            Ok(())
        }
        Some("init-ui") => init::run_init_ui(&std::env::current_dir()?),
        Some("dev") => dev::start_dev(rest).await,
        Some("build") => cargo(&["build", "--release"], rest).await,
        Some("test") => cargo(&["test"], rest).await,
        Some(command @ ("migrate" | "run" | "cron")) => cargo(&["run", "--", command], rest).await,
        Some(command @ ("start" | "worker")) => cargo(&["run", "--release", "--", command], rest).await,
        Some("--version" | "-V") => {
            println!("{}", version());
            Ok(())
        }
        None | Some("help" | "--help" | "-h") => {
            println!("{USAGE}");
            Ok(())
        }
        Some(unknown) => Err(UnknownCommand(unknown.to_string()).into()),
    }
}

/// A command that does not exist: the usage goes to stderr and the exit code is
/// 2, as for a command line that cannot be parsed, so that a script that
/// mistypes a command stops.
#[derive(Debug)]
struct UnknownCommand(String);

impl std::fmt::Display for UnknownCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[openshop] Unknown command: {}\n{USAGE}", self.0)
    }
}

impl std::error::Error for UnknownCommand {}

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error:#}");
            match error.downcast_ref::<UnknownCommand>() {
                Some(_) => ExitCode::from(2),
                None => ExitCode::FAILURE,
            }
        }
    }
}
