//! `dev`: everything in one process, with the schema pushed on startup.

use anyhow::{Context, Result};

use super::env::load_env_file;
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
    // Already done by `run`, which found nothing left to set; here for a caller of `start_dev`.
    load_env_file(&std::env::current_dir()?);
    let env = RuntimeEnv::from_env();
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
    let server = listen(&app, port, ServerOptions::development().ui(ui)).await?;

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
