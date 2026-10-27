//! `dev`: everything in one process, with the schema pushed on startup.

use anyhow::{Context, Result};

use super::start::ui_source;
use super::{database_url, port, shutdown_signal};
use crate::app::{OpenShop, OpenShopConfig};
use crate::db::migrate::push_schema;
use crate::engine::{Scheduler, Worker};
use crate::runtime::RuntimeEnv;
use crate::server::{ServerOptions, listen};
use crate::types::WorkerConfig;

/// Starts the API, the admin UI, a worker and the scheduler together.
///
/// The `openshop dev` command of the CLI wraps this with a file watcher that
/// rebuilds and restarts the process when sources change.
pub async fn start_dev(config: OpenShopConfig) -> Result<()> {
    // The `dev` command is a development environment: secrets may be stored
    // in plaintext without an `ENCRYPTION_KEY`, unless `OPENSHOP_ENV` says production.
    let env = RuntimeEnv {
        development: true,
        ..RuntimeEnv::from_env()
    };
    let port = port(3000);

    println!("[openshop] Starting dev server...");

    // 1. Database setup — push the framework schema and application models
    let pool = crate::db::connect_to(&database_url()).await?;
    push_schema(&pool, &config.models, env.production)
        .await
        .context("[openshop] Schema push failed")?;
    println!("[openshop] Database initialized");

    // 2. API server + admin UI
    let ui = ui_source(&config);
    let app = OpenShop::new(config, pool, env)?;
    let server = listen(&app, port, ServerOptions { ui, dev: true }).await?;

    // 3. Worker and scheduler run in-process
    let worker = Worker::new(app.clone(), WorkerConfig::default());
    worker.start().await;
    let mut scheduler = Scheduler::start(&app)?;

    println!("[openshop] Admin UI running on http://localhost:{port}");
    println!("[openshop] Ready.");

    shutdown_signal().await;
    let closed = server.shutdown().await;
    scheduler.stop();
    worker.stop().await;
    closed
}
