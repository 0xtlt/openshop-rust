use openshop::prelude::*;

use crate::providers::warehouse::{Order, Warehouse};

/// What a run receives. The admin builds its form from it, and a run with
/// another input is refused.
#[input]
#[derive(Debug)]
pub struct SyncOrdersInput {
    #[schemars(range(min = 1))]
    pub limit: u32,
}

#[flow("syncOrders")]
pub async fn sync_orders(ctx: FlowContext, input: SyncOrdersInput) -> Result<()> {
    // Each step is checkpointed: a retried run reuses its stored output.
    let orders: Vec<Order> = ctx
        .step
        .run("fetch-orders", async || {
            ctx.logger.info_with(
                "Fetching latest orders from Shopify...",
                json!({ "limit": input.limit }),
            );

            // Checked against Shopify's schema when the crate compiles; `data` is typed.
            let data = ctx
                .shopify
                .execute(graphql!(
                    "query GetRecentOrders($first: Int!) {
                       orders(first: $first, sortKey: CREATED_AT, reverse: true) {
                         nodes { id name }
                       }
                     }",
                    first = input.limit
                ))
                .await?;

            let orders = data.orders.nodes.into_iter();
            Ok(orders
                .map(|order| Order {
                    id: order.id,
                    name: order.name,
                })
                .collect())
        })
        .await?;

    ctx.step
        .run("send-to-warehouse", async || {
            // Checked by the compiler: a `Vec<Order>` in, a `usize` out.
            let accepted = Warehouse::on(&ctx.connectors).push(&orders).await?;
            ctx.logger.info_with("Orders synced", json!({ "count": accepted }));
            Ok(())
        })
        .await
}
