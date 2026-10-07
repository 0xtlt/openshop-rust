# Sync orders with a flow

Use this guide to send recent orders to a warehouse in a background job.
Before starting, have an installed development app with `read_orders` access and
a registered, configured `warehouse` provider exposing `push(rows)`. Follow
[Connect an external service](define-provider.md) if you need that provider.

## 1. Create the flow

Create or replace `src/flows/sync_orders.rs`:

```rust,ignore
use std::time::Duration;

use openshop::{FlowContext, Result, Value, flow, json, serde_json};
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

#[flow("syncOrders", timeout = Duration::from_secs(60), step_timeout = Duration::from_secs(15))]
pub async fn sync_orders(ctx: FlowContext, input: SyncOrdersInput) -> Result<()> {
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
}
```

`#[flow("syncOrders")]` makes the function the flow of that name. Its second
argument is the input of a run, validated against the JSON Schema derived from
`SyncOrdersInput`, so the function only sees a positive integer `limit`.
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

Declare the module in `src/flows/mod.rs` (`pub mod sync_orders;`, already
there in the generated project) and list the flow in the application, in
`src/main.rs`:

```rust,ignore
mod flows;
mod providers;

use openshop::{App, Result, app};

use flows::sync_orders::sync_orders;
use providers::warehouse::Warehouse;

static APP: App = app! {
    providers: [Warehouse],
    flows: [sync_orders],
    // ... crons and other settings ...
};

#[tokio::main]
async fn main() -> Result<()> {
    APP.run().await
}
```

`Warehouse` is the provider of
[Connect an external service](define-provider.md). Preserve any other listed
blocks and app settings.

The generated `src/main.rs` builds its configuration with
`OpenShopConfig::builder()`, the lower level of `app!`. To keep it as it is,
replace its line `.flow(flows::sync_orders::sync_orders())` by
`.add::<flows::sync_orders::sync_orders>()`, which adds a declared block to a
builder.

Check the app:

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
use openshop::{App, CronEntry, app, json};

static APP: App = app! {
    providers: [Warehouse],
    flows: [sync_orders],
    crons: [
        CronEntry::new("*/5 * * * *", sync_orders::NAME)
            .name("Sync recent orders")
            .input(json!({ "limit": 10 }))
            .shops("all"),
    ],
    // ... other settings ...
};
```

`sync_orders::NAME` is the name of the flow, `syncOrders`. On the builder of
the generated project, `.cron(…)` takes the same entry.

`.shops("all")` dispatches for each installed app/shop pair. Omitting `.shops(..)`
uses the synthetic `__global__` shop, which has no ordinary Shopify installation:
a flow that calls Shopify, like this one, fails there. For a shop-specific
integration, set the target explicitly, for example
`.shops("example.myshopify.com")`. Configure credentials for every targeted shop
before enabling the schedule.

The app refuses to start with an invalid schedule or a cron that names a flow
it does not list. Verify a new run appears after the next five-minute boundary.
The default concurrency policy rejects a second active run for the same app,
shop, and flow.

## 4. Make repeated writes safe

Each scheduled run can include orders sent by an earlier run. Make the warehouse
write an upsert keyed by the Shopify order ID, or use the warehouse's idempotency
mechanism. Completed checkpoints are reused within one run; separate scheduled
runs do not share them.

If you need a different retry policy, set `retry_policy = ..` in the `#[flow]`
attribute and consult the
[flow reference](../reference/flows.md#retry-precedence-and-defaults).
For the failure model, read
[Checkpoints and retries](../concepts/checkpoints-and-retries.md).
