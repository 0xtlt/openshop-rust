//! OpenShop: build Shopify integrations in Rust with background flows,
//! configurable service providers, and an embedded admin UI.
//!
//! An application is described once with [`OpenShopConfig::builder`] and run
//! through [`cli::run`], which provides the `dev`, `start`, `worker` and
//! `migrate` commands:
//!
//! ```no_run
//! use openshop::{Flow, OpenShopConfig};
//!
//! fn config() -> openshop::Result<OpenShopConfig> {
//!     OpenShopConfig::builder()
//!         .flow(Flow::new("hello", |ctx| async move {
//!             let greeting: String = ctx.step.run("greet", || async { Ok("hello".to_string()) }).await?;
//!             ctx.logger.info(serde_json::json!({ "greeting": greeting }), "Greeted");
//!             Ok(())
//!         }))
//!         .build()
//! }
//!
//! #[tokio::main]
//! async fn main() -> openshop::Result<()> {
//!     openshop::cli::run(config).await
//! }
//! ```
//!
//! A web process (`start`) accepts requests and queues work; workers (`worker`)
//! execute it. PostgreSQL stores installations, credentials, runs and checkpoints.

pub mod admin;
mod app;
pub mod cli;
pub mod config;
pub mod db;
pub mod engine;
pub mod response;
pub mod runtime;
pub mod server;
pub mod shopify;
#[cfg(feature = "testing")]
pub mod testing;
pub mod types;
pub mod validators;

pub use anyhow::{Context, Error, Result, anyhow, bail};
pub use chrono;
pub use indexmap::IndexMap;
#[cfg(feature = "schema")]
pub use schemars::{self, JsonSchema};
pub use serde_json::{self, Value, json};
pub use sqlx::{self, PgPool};
pub use tokio_util::sync::CancellationToken;

pub use admin::{
    AdminActor, AdminFunction, AdminPage, AdminPublicError, AdminServerContext, define_admin_action,
    define_admin_loader, define_admin_page_access,
};
#[cfg(feature = "schema")]
pub use admin::{define_typed_admin_action, define_typed_admin_loader};
pub use app::{OpenShop, OpenShopBuilder, OpenShopConfig};
pub use db::{Model, define_model};
pub use engine::{
    DispatchFlowParams, DispatchFlowResult, FlowCanceledError, FlowConcurrencyError, FlowTimeoutError, RunFlowOptions,
    RunFlowResult, Scheduler, SleepSignal, StepExecutor, StepTimeoutError, Worker, cancel_run, dispatch_flow, run_flow,
};
pub use runtime::{FlowLogger, RuntimeEnv, RuntimeLogger, get_runtime_logger, set_runtime_logger};
pub use server::connectors::{Connector, Connectors};
pub use server::static_ui::UiAssets;
pub use server::{ServerOptions, ShopSession, UiSource, create_server, start_api_server};
pub use shopify::{ShopifyClient, create_shopify_client};
pub use types::*;
