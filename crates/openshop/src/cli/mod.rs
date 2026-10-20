//! The command line of an application binary.
//!
//! ```text
//! dev                         Start API, admin UI, worker and scheduler in one process
//! start                       Start the production web process (API + UI + scheduler, no worker)
//! worker [--concurrency=N]    Start a worker (production, scalable)
//! migrate                     Apply client-owned migrations from ./migrations
//! migrate generate            Generate migrations for schema the project does not cover yet
//! migrate check               Check that ./migrations covers the framework schema
//! migrate status              Show migration status
//! ```

pub mod dev;
pub mod env;
pub mod migrate;
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
    migrate generate            Generate a new client-owned migration
    migrate check               Check generated migration history
    migrate status              Show client migration status
    start                       Start production server (API + UI, no worker)

  Usage:
    cargo run -- dev
    cargo run --release -- worker --concurrency=10
    cargo run -- migrate generate
    cargo run -- migrate
    cargo run -- migrate check
    cargo run -- migrate status
    cargo run --release -- start
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

/// Where the configuration comes from: a built [`OpenShopConfig`], or a
/// function that builds it.
///
/// Prefer passing the function (`openshop::cli::run(config)`): in `dev`, the
/// `.env` file is then loaded before the configuration reads the environment.
pub trait ConfigSource {
    fn load(self) -> Result<OpenShopConfig>;
}

impl ConfigSource for OpenShopConfig {
    fn load(self) -> Result<OpenShopConfig> {
        Ok(self)
    }
}

impl<F: FnOnce() -> Result<OpenShopConfig>> ConfigSource for F {
    fn load(self) -> Result<OpenShopConfig> {
        self()
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

/// Runs the command given on the command line.
pub async fn run(config: impl ConfigSource) -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    run_with_args(config, &args).await
}

pub async fn run_with_args(config: impl ConfigSource, args: &[String]) -> Result<()> {
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
            let config = config.load()?;
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
        Some("migrate") => migrate::run_migrate(&config.load()?, &args[1..]).await,
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
