# Sync orders with a flow

Use this guide to send recent orders to a warehouse in a background job.
Before starting, have an installed development app with `read_orders` access and
a registered, configured `warehouse` provider exposing `push(rows)`. Follow
[Connect an external service](define-provider.md) if you need that provider.

## 1. Create the flow

Create or replace `src/flows/sync_orders.rs`:

```rust,ignore
use std::time::Duration;

use openshop::{Flow, Value, json, serde_json};
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SyncOrdersInput {
    #[schemars(range(min = 1))]
    pub limit: u32,
}

const RECENT_ORDERS: &str = "#graphql
  query SyncOrders($first: Int!) {
    orders(first: $first, sortKey: CREATED_AT, reverse: true) {
      nodes { id name }
    }
  }
";

pub fn sync_orders() -> Flow {
    Flow::new("syncOrders", |ctx| async move {
        let input: SyncOrdersInput = ctx.input()?;

        let orders: Vec<Value> = ctx
            .step
            .run("fetch-orders", || async {
                let data = ctx
                    .shopify
                    .graphql(RECENT_ORDERS, Some(json!({ "first": input.limit })))
                    .await?;
                Ok(serde_json::from_value(data["orders"]["nodes"].clone())?)
            })
            .await?;

        ctx.step
            .run("push-orders", || async {
                ctx.connectors.call("warehouse", "push", &orders).await?;
                ctx.logger.info(json!({ "count": orders.len() }), "Orders synced");
                Ok(())
            })
            .await
    })
    .input::<SyncOrdersInput>()
    .timeout(Duration::from_secs(60))
    .step_timeout(Duration::from_secs(15))
}
```

`.input::<SyncOrdersInput>()` validates the run input against the JSON Schema
derived from the type, so `ctx.input()` only sees a positive integer `limit`.
`ctx.shopify.graphql()` returns the GraphQL data object, so the order list is
`data["orders"]["nodes"]`. The saved checkpoint contains only that JSON array.

Nothing checks that string when the project compiles. To have the operation
checked against Shopify's schema and its result typed, write it with
`graphql!`:

```rust,ignore
use openshop::graphql;

let data = ctx
    .shopify
    .execute(graphql!(
        "query($first: Int!) { orders(first: $first) { nodes { id name } } }",
        first = input.limit
    ))
    .await?;
let names: Vec<String> = data.orders.nodes.into_iter().map(|order| order.name).collect();
```

A misspelled field is then a compile error, and `data.orders.nodes` is a list
of structs with an `id` and a `name`. See
[Typed GraphQL](../reference/graphql.md).

This fetches one recent batch. For a complete historical sync, add pagination and
a durable cursor appropriate to your integration.

## 2. Register and run it

The generated `src/main.rs` already registers this flow. In another project,
declare the module in `src/flows/mod.rs` (`pub mod sync_orders;`) and add the
flow to the builder:

```rust,ignore
fn config() -> Result<OpenShopConfig> {
    OpenShopConfig::builder()
        .provider(providers::warehouse::warehouse())
        .flow(flows::sync_orders::sync_orders())
        // ... crons and other settings ...
        .build()
}
```

Preserve any other registered flows and app options. Check the app:

```bash
cargo check
```

With `shopify app dev` running, save the warehouse configuration in **Providers**,
then trigger `syncOrders` in **Flows** with `{ "limit": 10 }`. Confirm that both
steps complete and **Orders synced** appears in the logs. Development already
starts a worker. Production needs a [separate worker service](deploy-production.md).

## 3. Schedule the flow for installed shops

The generated configuration contains a sample cron entry named **Quick sync**
that has no shop target. Replace it with an entry that targets installed shops:

```rust,ignore
use openshop::{CronEntry, OpenShopConfig, Result, json};

fn config() -> Result<OpenShopConfig> {
    OpenShopConfig::builder()
        .provider(providers::warehouse::warehouse())
        .flow(flows::sync_orders::sync_orders())
        .cron(
            CronEntry::new("*/5 * * * *", "syncOrders")
                .name("Sync recent orders")
                .input(json!({ "limit": 10 }))
                .shops("all"),
        )
        // ... other settings ...
        .build()
}
```

`.shops("all")` dispatches for each installed app/shop pair. Omitting `.shops(..)`
uses the synthetic `__global__` shop, which has no ordinary Shopify installation:
a flow that calls Shopify, like this one, fails there. For a shop-specific
integration, set the target explicitly, for example
`.shops("example.myshopify.com")`. Configure credentials for every targeted shop
before enabling the schedule.

`build()` rejects an invalid schedule or a cron that names an unregistered flow.
Verify a new run appears after the next five-minute boundary. The default
concurrency policy rejects a second active run for the same app, shop, and flow.

## 4. Make repeated writes safe

Each scheduled run can include orders sent by an earlier run. Make the warehouse
write an upsert keyed by the Shopify order ID, or use the warehouse's idempotency
mechanism. Completed checkpoints are reused within one run; separate scheduled
runs do not share them.

If you need a different retry policy, set `.retry_policy(..)` on this flow and
consult the [flow reference](../reference/flows.md#retry-precedence-and-defaults).
For the failure model, read
[Checkpoints and retries](../concepts/checkpoints-and-retries.md).
