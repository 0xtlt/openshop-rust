//! A custom app, built for the merchant who runs it: a warehouse provider, two
//! flows, a Shopify Function, webhooks, crons, app proxy routes, a public
//! route, MCP capabilities and a custom admin page.
//!
//! The built-in admin is open to the merchant: the flows, the runs, the logs,
//! the providers and the crons, next to the page of the application.

pub mod admin;
pub mod flows;
pub mod functions;
pub mod mcp;
pub mod models;
pub mod providers;
pub mod proxy;
pub mod routes;
pub mod webhooks;

use openshop::prelude::*;

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

/// The application: what it is made of, and how it is set up.
pub static APP: App = app! {
    providers: [Warehouse],
    flows: [sync_orders, count_variants],
    functions: [VolumeDiscount],
    models: [Review],
    admin_pages: [ReviewsPage],
    webhooks: [orders_create, app_uninstalled],
    proxies: [reviews, list_reviews, submit_review],
    routes: [ping],
    mcp_tools: [status],
    mcp_resources: [guide],
    crons: [
        Cron::new("*/5 * * * *", sync_orders::NAME)
            .name("Quick sync")
            .input(json!({ "limit": 10 })),
        Cron::new("0 2 * * *", sync_orders::NAME)
            .name("Nightly full sync")
            .input(json!({ "limit": 1000 }))
            .all_shops(),
    ],
    // The operator dashboard, under `/ops`, when `OPS_PASSWORD` is set: what
    // happens in every shop, read from outside Shopify admin.
    ops_password: std::env::var("OPS_PASSWORD").ok(),
    // Sentry, when `SENTRY_DSN` is set: the runs that fail, the handlers that
    // fail and the panics, each with its flow and its shop.
    sentry: std::env::var("SENTRY_DSN").ok(),
    // The development store whose token background work may get with the
    // client credentials grant, when `SHOPIFY_CLIENT_CREDENTIALS_SHOP` is set.
    client_credentials_shop: std::env::var("SHOPIFY_CLIENT_CREDENTIALS_SHOP").ok(),
};
