//! OpenShop demo application: a warehouse provider, two flows, a Shopify
//! Function, webhooks, crons, app proxy routes, a public route, MCP
//! capabilities and a custom admin page.

pub mod admin;
pub mod flows;
pub mod functions;
pub mod models;
pub mod providers;
pub mod proxy;
pub mod routes;
pub mod webhooks;

use openshop::{
    CronEntry, CustomAdminNavigationItem, McpConfig, McpPermissionDefinition, McpResourceContent,
    McpResourceDefinition, McpRiskLevel, McpToolDefinition, OpenShopConfig, Result, json,
};

fn mcp() -> McpConfig {
    McpConfig::new()
        .permission(
            "demo:read_status",
            McpPermissionDefinition::new("Read demo status")
                .description("Allows MCP tokens to read a small demo status payload.")
                .group("Demo")
                .risk_level(McpRiskLevel::Low),
        )
        .tool(
            "demo.status",
            McpToolDefinition::new("Read a small OpenShop demo status payload.", |ctx, _input| async move {
                Ok(json!({
                    "content": [{
                        "type": "text",
                        "text": format!("Demo app is reachable for {} ({}).", ctx.shop, ctx.app_handle),
                    }],
                    "structuredContent": { "ok": true, "shop": ctx.shop, "appHandle": ctx.app_handle },
                }))
            })
            .required_permissions(["demo:read_status"])
            .risk_level(McpRiskLevel::Low)
            .input_schema(json!({ "type": "object", "additionalProperties": false, "properties": {} })),
        )
        .resource(
            "openshop://demo/mcp",
            McpResourceDefinition::new("Demo MCP guide", |_ctx| async move {
                Ok(McpResourceContent::from(
                    [
                        "# Demo MCP",
                        "",
                        "Create an MCP token from the OpenShop admin, grant `demo:read_status`, then call `demo.status`.",
                        "Core tools such as `openshop.logs.search` are available through OpenShop itself.",
                    ]
                    .join("\n"),
                ))
            })
            .description("Example resource exposed by the demo app.")
            .mime_type("text/markdown"),
        )
}

pub fn config() -> Result<OpenShopConfig> {
    OpenShopConfig::builder()
        .provider(providers::warehouse::warehouse())
        .flow(flows::sync_orders::sync_orders())
        .flow(flows::count_variants::count_variants())
        .function("volumeDiscount", functions::volume_discount::volume_discount())
        .model_of::<models::review::Review>()
        .custom_pages([CustomAdminNavigationItem {
            label: "Reviews".to_string(),
            path: "/reviews".to_string(),
        }])
        .admin_page(admin::reviews::page())
        .mcp(mcp())
        .webhook("orders/create", webhooks::orders_create::orders_create())
        .webhook("app/uninstalled", webhooks::app_uninstalled::app_uninstalled())
        .proxy("/reviews", proxy::reviews::reviews())
        .proxy("/api/reviews", proxy::api_reviews::api_reviews())
        .route("/ping", routes::ping::ping())
        .cron(
            CronEntry::new("*/5 * * * *", "syncOrders")
                .name("Quick sync")
                .input(json!({ "limit": 10 })),
        )
        .cron(
            CronEntry::new("0 2 * * *", "syncOrders")
                .name("Nightly full sync")
                .input(json!({ "limit": 1000 }))
                .shops("all"),
        )
        .on_error(|error, context| async move {
            eprintln!(
                "[openshop:error] Flow \"{}\" failed: {error}",
                context.flow.unwrap_or_default()
            );
        })
        .build()
}
