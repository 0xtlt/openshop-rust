//! The Admin API consumed by the embedded admin UI. Every route is scoped to
//! the shop and Shopify app of the session token.

pub mod crons;
pub mod flows;
pub mod mcp;
pub mod pages;
pub mod providers;
pub mod run_logs;
pub mod runs;

use axum::Router;

use crate::app::OpenShop;

pub fn create_api_routes() -> Router<OpenShop> {
    Router::new()
        .merge(pages::routes())
        .merge(flows::routes())
        .merge(crons::routes())
        .merge(runs::routes())
        .merge(providers::routes())
        .merge(mcp::routes())
}
