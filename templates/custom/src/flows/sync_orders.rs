use openshop::prelude::*;
use serde::{Deserialize, Serialize};

use crate::providers::warehouse::{Warehouse, WarehouseRecord};

#[input]
#[derive(Debug)]
pub struct SyncOrdersInput {
    #[schemars(range(min = 1))]
    pub limit: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WarehouseOrder {
    id: String,
    name: String,
    total: String,
    items: usize,
}

#[flow("syncOrders")]
pub async fn sync_orders(ctx: FlowContext, input: SyncOrdersInput) -> Result<()> {
    let orders: Vec<WarehouseOrder> = ctx
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
                    "
                      query GetRecentOrders($first: Int!) {
                        orders(first: $first, sortKey: CREATED_AT, reverse: true) {
                          edges {
                            node {
                              id
                              customer {
                                displayName
                              }
                              name
                              totalPriceSet { shopMoney { amount currencyCode } }
                              lineItems(first: 5) { edges { node { title quantity } } }
                            }
                          }
                        }
                      }
                    ",
                    first = input.limit
                ))
                .await?;

            Ok(data
                .orders
                .edges
                .into_iter()
                .map(|edge| WarehouseOrder {
                    id: edge.node.id,
                    name: edge.node.name,
                    total: edge.node.total_price_set.shop_money.amount,
                    items: edge.node.line_items.edges.len(),
                })
                .collect())
        })
        .await?;

    ctx.logger.info_with(
        format!("Fetched {} orders", orders.len()),
        json!({ "count": orders.len() }),
    );

    let transformed: Vec<WarehouseRecord> = ctx
        .step
        .run("transform-data", async || {
            let exported_at = openshop::runtime::time::to_iso(&openshop::chrono::Utc::now());
            Ok(orders
                .iter()
                .map(|order| WarehouseRecord {
                    reference: order.name.clone(),
                    amount: order.total.clone(),
                    line_count: order.items,
                    exported_at: exported_at.clone(),
                })
                .collect())
        })
        .await?;

    ctx.step
        .run("send-to-warehouse", async || {
            ctx.logger
                .info_with("Sending to warehouse API...", json!({ "records": transformed.len() }));
            Warehouse::on(&ctx.connectors).push(&transformed).await?;
            ctx.logger.info("Successfully sent to warehouse");
            Ok(())
        })
        .await
}
