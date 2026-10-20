//! `start`: the production web process.

use anyhow::Result;

use super::{database_url, port, shutdown_signal};
use crate::app::{OpenShop, OpenShopConfig};
use crate::db::migrate::assert_migrated;
use crate::engine::Scheduler;
use crate::runtime::RuntimeEnv;
use crate::server::{ServerOptions, UiSource, listen};

/// The admin UI to serve: `OPENSHOP_UI_DIR`, then the bundle set on the
/// configuration, then the one built into the framework.
pub(crate) fn ui_source(config: &OpenShopConfig) -> UiSource {
    match std::env::var_os("OPENSHOP_UI_DIR").filter(|directory| !directory.is_empty()) {
        Some(directory) => UiSource::Dir(directory.into()),
        None => config.admin_ui.clone().unwrap_or(UiSource::Embedded),
    }
}

/// Serves the API, the admin UI and the cron scheduler. Flows are executed by
/// separate `worker` processes.
pub async fn start_prod(config: OpenShopConfig) -> Result<()> {
    let port = port(3000);
    println!("[openshop] Starting production server...");
    println!("[openshop] Database migrations are manual. Run `migrate` before starting production processes.");

    let env = RuntimeEnv::from_env();
    let pool = crate::db::connect_to(&database_url()).await?;
    assert_migrated(&pool, &env.cwd).await?;
    let ui = ui_source(&config);
    let app = OpenShop::new(config, pool, env)?;
    println!(
        "[openshop] Loaded config: {} flows, {} providers",
        app.config().flows.len(),
        app.config().providers.len()
    );

    let server = listen(&app, port, ServerOptions::production().ui(ui)).await?;
    let mut scheduler = Scheduler::start(&app)?;
    println!("[openshop] Production server running on http://localhost:{port}");

    shutdown_signal().await;
    println!("[openshop] Shutting down...");
    let closed = server.shutdown().await;
    scheduler.stop();
    closed
}
