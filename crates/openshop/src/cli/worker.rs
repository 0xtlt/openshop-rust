//! `worker`: executes queued flow runs.

use anyhow::Result;

use super::{database_url, shutdown_signal};
use crate::app::{OpenShop, OpenShopConfig};
use crate::db::migrate::assert_migrated;
use crate::engine::Worker;
use crate::runtime::RuntimeEnv;
use crate::types::WorkerConfig;

pub async fn start_worker(config: OpenShopConfig, concurrency: Option<usize>) -> Result<()> {
    println!("[openshop] Starting worker...");
    println!("[openshop] Database migrations are manual. Run `migrate` before starting production processes.");

    let env = RuntimeEnv::from_env();
    let pool = crate::db::connect_to(&database_url()).await?;
    assert_migrated(&pool, &env.cwd).await?;
    let app = OpenShop::new(config, pool, env)?;
    println!("[openshop] Loaded config: {} flows", app.config().flows.len());

    let worker = Worker::new(
        app,
        WorkerConfig {
            concurrency,
            ..Default::default()
        },
    );
    worker.start().await;

    shutdown_signal().await;
    worker.stop().await;
    Ok(())
}
