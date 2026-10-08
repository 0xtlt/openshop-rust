mod flows;
mod providers;

use std::sync::Arc;

use openshop::prelude::*;

use flows::sync_orders::sync_orders;
use providers::warehouse::Warehouse;

/// Called when a flow run fails.
async fn report_failure(error: Arc<openshop::Error>, context: ErrorContext) {
    eprintln!(
        "[openshop:error] Flow \"{}\" failed: {error}",
        context.flow.unwrap_or_default()
    );
}

/// The application: what it is made of, and how it is set up.
///
/// Shopify credentials come from the environment and `shopify.app.toml`. Add
/// `shopify: …` only when one OpenShop instance must serve several Shopify apps.
static APP: App = app! {
    providers: [Warehouse],
    flows: [sync_orders],
    crons: [
        Cron::new("*/5 * * * *", sync_orders::NAME)
            .name("Quick sync")
            .input(json!({ "limit": 10 })),
    ],
    // MCP is off: `POST /mcp` is refused and the admin has no MCP page.
    // Remove this line to turn it on.
    mcp: McpConfig::disabled(),
    on_error: report_failure,
};

fn main() -> Result<()> {
    APP.main()
}

#[cfg(test)]
mod tests {
    use super::*;
    use openshop::testing::{MockTransport, TestContext};

    #[test]
    fn the_application_is_valid() {
        let config = APP.config().unwrap();
        assert!(config.flows.contains_key(sync_orders::NAME));
        assert!(config.providers.contains_key(Warehouse::NAME));
    }

    // Needs PostgreSQL: `docker compose up -d`, then `cargo test -- --ignored`.
    #[openshop::test(APP)]
    #[ignore = "requires PostgreSQL"]
    async fn sync_orders_pushes_orders_to_the_warehouse(ctx: &TestContext) {
        ctx.transport.respond_with(|_request| {
            Ok(MockTransport::json(
                200,
                json!({ "data": { "orders": { "nodes": [{ "id": "gid://shopify/Order/1", "name": "#1001" }] } } }),
            ))
        });
        // The fake of a method takes what the method returns, and gives back what it received.
        let push = ctx.fakes.for_call(Warehouse::PUSH);
        push.returns(1);

        let outcome = ctx.run_flow(sync_orders::NAME, json!({ "limit": 5 })).await.unwrap();

        assert_eq!(outcome, RunOutcome::Completed);
        assert_eq!(push.call_count(), 1);
        assert_eq!(push.last_call().unwrap().args[0].name, "#1001");
    }
}
