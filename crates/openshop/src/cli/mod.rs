//! The command line of an application binary: the commands of [`USAGE`].

pub mod dev;
pub mod env;
pub mod migrate;
pub mod run;
pub mod start;
pub mod worker;

use anyhow::Result;

use crate::app::OpenShopConfig;
use crate::runtime::env::DEFAULT_DATABASE_URL;
use crate::runtime::report;

pub const USAGE: &str = "
  openshop - Shopify integration framework

  Commands:
    dev                         Start dev server (API + UI + worker + scheduler)
    worker [--concurrency=N]    Start worker only (production, scalable)
    migrate                     Apply client-owned migrations from ./migrations
    migrate generate            Generate migrations for the schema ./migrations does not cover yet
    migrate check               Check that ./migrations covers the current schema
    migrate status              Show client migration status
    start                       Start production server (API + UI + scheduler, no worker)
    run <flow> [--shop=<domain> | --all-shops] [--input=<json>]
                                Run a flow once, through the queue and a worker, and wait for it
    cron run <name>             Run the flow of a cron once, for the shops of the cron

  Usage:
    cargo run -- dev
    cargo run --release -- worker --concurrency=10
    cargo run -- migrate generate
    cargo run -- migrate
    cargo run -- migrate check
    cargo run -- migrate status
    cargo run --release -- start
    cargo run -- run syncOrders --shop=my-store.myshopify.com --input='{\"limit\":10}'
    cargo run -- cron run \"Nightly sync\"
";

/// Logs to stdout. `RUST_LOG` overrides the default filter (`info`, with SQL
/// statement and notice logging limited to warnings).
pub fn init_logging() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,sqlx=warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}

/// `DATABASE_URL`, or the local development database when unset.
pub(crate) fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .ok()
        .filter(|url| !url.is_empty())
        .unwrap_or_else(|| DEFAULT_DATABASE_URL.to_string())
}

pub(crate) fn port(default: u16) -> u16 {
    std::env::var("PORT")
        .ok()
        .and_then(|port| port.parse().ok())
        .filter(|port| *port != 0)
        .unwrap_or(default)
}

/// Resolves when the process receives SIGINT or SIGTERM.
pub async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = terminate.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// `--concurrency=N` of the `worker` command. Absent, `worker.concurrency`
/// from the configuration applies.
fn parse_concurrency(args: &[String]) -> Result<Option<usize>> {
    let Some(value) = args.iter().find_map(|arg| arg.strip_prefix("--concurrency=")) else {
        return Ok(None);
    };
    match value.parse::<usize>() {
        Ok(concurrency) if concurrency > 0 => Ok(Some(concurrency)),
        _ => anyhow::bail!("[openshop] --concurrency must be a positive integer, got \"{value}\""),
    }
}

/// The usage a command prints when its arguments ask for it with `--help`
/// or `-h`, wherever they are: `run --help`, `cron run -h`. It is printed
/// before the configuration is built or the database opened, and the
/// command does not run.
fn help(args: &[String]) -> Option<&'static str> {
    let (command, rest) = args.split_first()?;
    if !rest.iter().any(|arg| arg == "--help" || arg == "-h") {
        return None;
    }
    Some(match command.as_str() {
        "run" => run::RUN_USAGE,
        "cron" => run::CRON_USAGE,
        "migrate" => migrate::MIGRATE_USAGE,
        _ => USAGE,
    })
}

/// Runs the command given on the command line. `config` builds the
/// configuration once the command is known: in `dev`, after the `.env` file
/// is loaded, so that it can read the values of the file.
pub async fn run(config: impl FnOnce() -> Result<OpenShopConfig>) -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    run_with_args(config, &args).await
}

pub async fn run_with_args(config: impl FnOnce() -> Result<OpenShopConfig>, args: &[String]) -> Result<()> {
    if let Some(usage) = help(args) {
        println!("{usage}");
        return Ok(());
    }
    let command = args.first().map(String::as_str);
    // Before logging and the configuration, so that both can read `.env` values.
    // `App::main` has done it before starting the runtime: this finds nothing
    // left to set. It is a late `set_var` for a `main` that starts its own runtime.
    let late = env::load_dev_env(args, &std::env::current_dir()?);
    init_logging();
    if late > 0 {
        tracing::warn!(
            "[openshop] .env was loaded after the async runtime started, which is not sound while other threads read the environment. Run the app with `APP.main()` to load it before."
        );
    }

    match command {
        Some(process @ ("dev" | "start" | "worker")) => {
            let concurrency = match process {
                "worker" => parse_concurrency(args)?,
                _ => None,
            };
            let config = config()?;
            // Reports to Sentry until the process stops, when the application gives a DSN.
            let _reporting = report::start(&config, process);
            let outcome = match process {
                "dev" => dev::start_dev(config).await,
                "start" => start::start_prod(config).await,
                _ => worker::start_worker(config, concurrency).await,
            };
            // What stopped the process is reported before it exits.
            if let Err(error) = &outcome {
                report::error(error);
            }
            outcome
        }
        Some("migrate") => migrate::run_migrate(&config()?, &args[1..]).await,
        Some(command @ ("run" | "cron")) => {
            let config = config()?;
            let _reporting = report::start(&config, command);
            let outcome = match command {
                "run" => run::run_flow_command(config, &args[1..]).await,
                _ => run::run_cron_command(config, &args[1..]).await,
            };
            if let Err(error) = &outcome {
                report::error(error);
            }
            outcome
        }
        _ => {
            println!("{USAGE}");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| arg.to_string()).collect()
    }

    #[test]
    fn help_after_a_command_is_its_usage() {
        for (command, usage) in [
            (&["run", "--help"][..], run::RUN_USAGE),
            (&["run", "syncOrders", "--shop=a.myshopify.com", "-h"], run::RUN_USAGE),
            (&["cron", "--help"], run::CRON_USAGE),
            (&["cron", "run", "--help"], run::CRON_USAGE),
            (&["cron", "run", "Nightly sync", "-h"], run::CRON_USAGE),
            (&["migrate", "-h"], migrate::MIGRATE_USAGE),
            (&["migrate", "generate", "--help"], migrate::MIGRATE_USAGE),
            (&["worker", "--help"], USAGE),
            (&["dev", "-h"], USAGE),
            (&["start", "--help"], USAGE),
        ] {
            assert_eq!(help(&args(command)), Some(usage), "{command:?}");
        }
        for command in [
            &[][..],
            &["--help"],
            &["run", "syncOrders"],
            &["cron", "run", "--helpful"],
            &["migrate", "status"],
        ] {
            assert_eq!(help(&args(command)), None, "{command:?}");
        }
    }

    #[tokio::test]
    async fn help_succeeds_without_running_the_command() {
        for command in [&["run", "--help"][..], &["cron", "run", "--help"], &["migrate", "generate", "-h"]] {
            run_with_args(|| panic!("the configuration is built"), &args(command))
                .await
                .unwrap();
        }
    }

    #[test]
    fn worker_concurrency_is_optional_and_positive() {
        assert_eq!(parse_concurrency(&args(&["worker"])).unwrap(), None);
        assert_eq!(
            parse_concurrency(&args(&["worker", "--concurrency=10"])).unwrap(),
            Some(10)
        );
        for invalid in ["0", "-1", "ten", ""] {
            let error = parse_concurrency(&args(&["worker", &format!("--concurrency={invalid}")])).unwrap_err();
            assert_eq!(
                error.to_string(),
                format!("[openshop] --concurrency must be a positive integer, got \"{invalid}\"")
            );
        }
    }
}
