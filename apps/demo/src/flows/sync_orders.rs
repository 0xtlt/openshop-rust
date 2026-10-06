use openshop::{Flow, Value, json};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, JsonSchema)]
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

const RECENT_ORDERS: &str = "#graphql
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
";

fn text(node: &Value, pointer: &str) -> String {
    node.pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

pub fn sync_orders() -> Flow {
    Flow::new("syncOrders", |ctx| async move {
        let input: SyncOrdersInput = ctx.input()?;

        let orders: Vec<WarehouseOrder> = ctx
            .step
            .run("fetch-orders", || async {
                ctx.logger.info(
                    json!({ "limit": input.limit }),
                    "Fetching latest orders from Shopify...",
                );

                let data = ctx
                    .shopify
                    .graphql(RECENT_ORDERS, Some(json!({ "first": input.limit })))
                    .await?;

                let edges = data.pointer("/orders/edges").and_then(Value::as_array);
                Ok(edges
                    .into_iter()
                    .flatten()
                    .map(|edge| WarehouseOrder {
                        id: text(edge, "/node/id"),
                        name: text(edge, "/node/name"),
                        total: text(edge, "/node/totalPriceSet/shopMoney/currencyCode"),
                        items: edge
                            .pointer("/node/lineItems/edges")
                            .and_then(Value::as_array)
                            .map_or(0, Vec::len),
                    })
                    .collect())
            })
            .await?;

        ctx.logger.info(
            json!({ "count": orders.len() }),
            format!("Fetched {} orders", orders.len()),
        );

        let transformed: Vec<Value> = ctx
            .step
            .run("transform-data", || async {
                let exported_at = openshop::runtime::time::to_iso(&openshop::chrono::Utc::now());
                Ok(orders
                    .iter()
                    .map(|order| {
                        json!({
                            "ref": order.name,
                            "amount": order.total,
                            "lineCount": order.items,
                            "exportedAt": exported_at,
                        })
                    })
                    .collect())
            })
            .await?;

        ctx.step
            .run("send-to-warehouse", || async {
                ctx.logger
                    .info(json!({ "records": transformed.len() }), "Sending to warehouse API...");
                ctx.connectors.call("warehouse", "push", &transformed).await?;
                ctx.logger.info(json!({}), "Successfully sent to warehouse");
                Ok(())
            })
            .await
    })
    .input::<SyncOrdersInput>()
}
