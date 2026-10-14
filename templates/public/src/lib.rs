//! A public app, installed by many shops: a stock watch that tells each
//! merchant which variants run low.
//!
//! A merchant reaches the pages of the application, its home and its
//! settings, and nothing of the flows, the runs or the logs. Those are for
//! whoever runs the application, on the operator dashboard.

pub mod admin;
pub mod flows;
pub mod models;
pub mod webhooks;

use openshop::AdminPagesConfig;
use openshop::prelude::*;

use admin::home::HomePage;
use admin::settings::SettingsPage;
use flows::check_stock::check_stock;
use models::stock_settings::StockSettings;
use webhooks::app_uninstalled::app_uninstalled;

/// The application: what it is made of, and how it is set up.
pub static APP: App = app! {
    flows: [check_stock],
    models: [StockSettings],
    admin_pages: [HomePage, SettingsPage],
    webhooks: [app_uninstalled],
    crons: [
        Cron::new("0 * * * *", check_stock::NAME)
            .name("Hourly stock check")
            .all_shops(),
    ],
    // The admin of a merchant is the pages above: no built-in page, so no
    // flow, run, log or cron of the application.
    builtin_pages: AdminPagesConfig::none(),
    // Without its page, nobody creates an MCP token: the protocol is off too.
    mcp: McpConfig::disabled(),
    // The operator dashboard, under `/ops`, when `OPS_PASSWORD` is set: what
    // happens in every shop, read from outside Shopify admin.
    ops_password: std::env::var("OPS_PASSWORD").ok(),
    // Sentry, when `SENTRY_DSN` is set: the runs that fail, the handlers that
    // fail and the panics, each with its flow and its shop.
    sentry: std::env::var("SENTRY_DSN").ok(),
};
