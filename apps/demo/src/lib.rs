//! OpenShop demo application: a warehouse provider, two flows, a Shopify
//! Function, webhooks, crons, app proxy routes, a public route, MCP
//! capabilities and a custom admin page.

pub mod admin;
pub mod flows;
pub mod functions;
pub mod mcp;
pub mod models;
pub mod providers;
pub mod proxy;
pub mod routes;
pub mod webhooks;

use std::sync::Arc;

use openshop::{
    App, CronEntry, CustomAdminNavigationItem, ErrorContext, McpConfig, McpPermissionDefinition, McpRiskLevel, app,
    json,
};

use admin::reviews::ReviewsPage;
use flows::count_variants::count_variants;
use flows::sync_orders::sync_orders;
use functions::volume_discount::VolumeDiscount;
use mcp::{guide, status};
use models::review::Review;
use providers::warehouse::Warehouse;
use proxy::api_reviews::{list_reviews, submit_review};
use proxy::reviews::reviews;
use routes::ping::ping;
use webhooks::app_uninstalled::app_uninstalled;
use webhooks::orders_create::orders_create;

/// Called when a flow run fails.
async fn report_failure(error: Arc<openshop::Error>, context: ErrorContext) {
    eprintln!(
        "[openshop:error] Flow \"{}\" failed: {error}",
        context.flow.unwrap_or_default()
    );
}

/// The application: what it is made of, and how it is set up.
pub static APP: App = app! {
    providers: [Warehouse],
    flows: [sync_orders, count_variants],
    functions: [VolumeDiscount],
    models: [Review],
    admin_pages: [ReviewsPage],
    custom_pages: [CustomAdminNavigationItem::new("Reviews", ReviewsPage::PATH)],
    webhooks: [orders_create, app_uninstalled],
    proxies: [reviews, list_reviews, submit_review],
    routes: [ping],
    mcp_tools: [status],
    mcp_resources: [guide],
    mcp: McpConfig::new().permission(
        mcp::READ_STATUS,
        McpPermissionDefinition::new("Read demo status")
            .description("Allows MCP tokens to read a small demo status payload.")
            .group("Demo")
            .risk_level(McpRiskLevel::Low),
    ),
    crons: [
        CronEntry::new("*/5 * * * *", sync_orders::NAME)
            .name("Quick sync")
            .input(json!({ "limit": 10 })),
        CronEntry::new("0 2 * * *", sync_orders::NAME)
            .name("Nightly full sync")
            .input(json!({ "limit": 1000 }))
            .shops("all"),
    ],
    on_error: report_failure,
};
