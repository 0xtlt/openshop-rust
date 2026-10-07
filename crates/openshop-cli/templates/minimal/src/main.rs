mod flows;
mod providers;

use openshop::{AdminPageId, AdminPageMode, CronEntry, McpConfig, OpenShopConfig, Result, json};

fn config() -> Result<OpenShopConfig> {
    // Single-app projects can keep Shopify credentials in env/TOML.
    // Add `.shopify(..)` only when one OpenShop instance must serve several Shopify apps.
    OpenShopConfig::builder()
        .provider(providers::warehouse::warehouse())
        .flow(flows::sync_orders::sync_orders())
        .cron(
            CronEntry::new("*/5 * * * *", "syncOrders")
                .name("Quick sync")
                .input(json!({ "limit": 10 })),
        )
        // MCP is off: `POST /mcp` is refused and the admin has no token page.
        // Remove both lines to turn it on.
        .mcp(McpConfig::new().enabled(false))
        .page(AdminPageId::Mcp, AdminPageMode::Disabled)
        .on_error(|error, context| async move {
            eprintln!(
                "[openshop:error] Flow \"{}\" failed: {error}",
                context.flow.unwrap_or_default()
            );
        })
        .build()
}

#[tokio::main]
async fn main() -> Result<()> {
    openshop::cli::run(config).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use openshop::engine::RunFlowResult;
    use openshop::json;
    use openshop::testing::{MockTransport, TestContext, TestOptions};

    #[test]
    fn config_is_valid() {
        let config = config().unwrap();
        assert!(config.flows.contains_key("syncOrders"));
        assert!(config.providers.contains_key("warehouse"));
    }

    // Needs PostgreSQL: `docker compose up -d`, then `cargo test -- --ignored`.
    #[tokio::test]
    #[ignore = "requires PostgreSQL"]
    async fn sync_orders_pushes_orders_to_the_warehouse() {
        let ctx = TestContext::with_options(
            config().unwrap(),
            TestOptions {
                access_token: Some("test-access-token".to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        ctx.transport.respond_with(|_request| {
            Ok(MockTransport::json(
                200,
                json!({ "data": { "orders": { "edges": [{ "node": { "id": "gid://shopify/Order/1", "name": "#1001" } }] } } }),
            ))
        });

        let result = ctx.run_flow("syncOrders", json!({ "limit": 5 })).await.unwrap();

        assert_eq!(result, RunFlowResult::Completed);
        let push = ctx.fakes.method("warehouse", "push");
        assert_eq!(push.call_count(), 1);
        assert_eq!(push.last_call().unwrap().args[0]["name"], "#1001");
        ctx.shutdown().await.unwrap();
    }
}
