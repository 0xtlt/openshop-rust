use openshop::{Flow, Value, json};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SyncOrdersInput {
    #[schemars(range(min = 1))]
    pub limit: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Order {
    id: String,
    name: String,
}

const RECENT_ORDERS: &str = "#graphql
  query GetRecentOrders($first: Int!) {
    orders(first: $first, sortKey: CREATED_AT, reverse: true) {
      edges {
        node {
          id
          name
        }
      }
    }
  }
";

pub fn sync_orders() -> Flow {
    Flow::new("syncOrders", |ctx| async move {
        let input: SyncOrdersInput = ctx.input()?;

        let orders: Vec<Order> = ctx
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
                let text = |edge: &Value, pointer: &str| {
                    edge.pointer(pointer).and_then(Value::as_str).unwrap_or_default().to_string()
                };
                Ok(edges
                    .into_iter()
                    .flatten()
                    .map(|edge| Order {
                        id: text(edge, "/node/id"),
                        name: text(edge, "/node/name"),
                    })
                    .collect())
            })
            .await?;

        ctx.step
            .run("send-to-warehouse", || async {
                ctx.connectors.call("warehouse", "push", &orders).await?;
                ctx.logger.info(json!({ "count": orders.len() }), "Orders synced");
                Ok(())
            })
            .await
    })
    .input::<SyncOrdersInput>()
}
