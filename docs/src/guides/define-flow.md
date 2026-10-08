# Sync orders with a flow

Use this guide to send recent orders to a warehouse in a background job.
Before starting, have an installed development app with `read_orders` access and
a registered, configured `warehouse` provider exposing `push(rows)`. Follow
[Connect an external service](define-provider.md) if you need that provider.

## 1. Create the flow

Create or replace `src/flows/sync_orders.rs`:

```rust,ignore
use std::time::Duration;

use openshop::prelude::*;

use crate::providers::warehouse::{Order, Warehouse};

#[input]
#[derive(Debug)]
pub struct SyncOrdersInput {
    #[schemars(range(min = 1))]
    pub limit: u32,
}

#[flow("syncOrders", timeout = "60s", step_timeout = "15s")]
pub async fn sync_orders(ctx: FlowContext, input: SyncOrdersInput) -> Result<()> {
    let orders: Vec<Order> = ctx
        .step
        .run("fetch-orders", async || {
            let data = ctx
                .shopify
                .execute(graphql!(
                    "query SyncOrders($first: Int!) {
                       orders(first: $first, sortKey: CREATED_AT, reverse: true) {
                         nodes { id name }
                       }
                     }",
                    first = input.limit
                ))
                .await?;
            let orders = data.orders.nodes.into_iter();
            Ok(orders.map(|order| Order { id: order.id, name: order.name }).collect())
        })
        .await?;

    ctx.step
        .run("push-orders", async || {
            let accepted = Warehouse::on(&ctx.connectors).push(&orders).await?;
            ctx.logger.info_with("Orders synced", json!({ "count": accepted }));
            Ok(())
        })
        .await
}
```

`#[flow("syncOrders")]` makes the function the flow of that name. Its second
argument is the input of a run. `#[input]` makes `SyncOrdersInput` such an
input: a run is validated against the JSON Schema derived from it, so the
function only sees a positive integer `limit`.

`graphql!` checks the operation against Shopify's schema when the project
compiles: a misspelled field is a compile error, and `data.orders.nodes` is a
list of structs with an `id` and a `name`. See
[Typed GraphQL](../reference/graphql.md). The first step returns the orders
as the `Order` of the provider, and the saved checkpoint contains that list.

`Warehouse::on(&ctx.connectors).push(&orders)` calls the provider with the
configuration the shop saved. The compiler checks the method, its argument and
its result. A provider or an operation only known at run time goes through
`ctx.connectors.call("warehouse", "push", &orders)` and
`ctx.shopify.graphql(query, variables)`, which take and return JSON.

This fetches one recent batch. For a complete historical sync, add pagination and
a durable cursor appropriate to your integration.

## 2. Register and run it

Declare the module in `src/flows/mod.rs` (`pub mod sync_orders;`, already
there in the generated project) and list the flow in the application, in
`src/main.rs`:

```rust,ignore
mod flows;
mod providers;

use openshop::prelude::*;

use flows::sync_orders::sync_orders;
use providers::warehouse::Warehouse;

static APP: App = app! {
    providers: [Warehouse],
    flows: [sync_orders],
    // ... crons and other settings ...
};

fn main() -> Result<()> {
    APP.main()
}
```

`Warehouse` is the provider of
[Connect an external service](define-provider.md). Preserve any other listed
blocks and app settings. The generated `src/main.rs` already lists both.

Check the app:

```bash
cargo check
```

With `shopify app dev` running, save the warehouse configuration in **Providers**,
then trigger `syncOrders` in **Flows** with `{ "limit": 10 }`. Confirm that both
steps complete and **Orders synced** appears in the logs. Development already
starts a worker. Production needs a [separate worker service](deploy-production.md).

## 3. Schedule the flow for installed shops

The generated application contains a sample cron entry named **Quick sync**
that has no shop target. Replace it with an entry that targets installed shops:

```rust,ignore
static APP: App = app! {
    providers: [Warehouse],
    flows: [sync_orders],
    crons: [
        Cron::new("*/5 * * * *", sync_orders::NAME)
            .name("Sync recent orders")
            .input(json!({ "limit": 10 }))
            .all_shops(),
    ],
    // ... other settings ...
};
```

`sync_orders::NAME` is the name of the flow, `syncOrders`.

`.all_shops()` dispatches for each installed app/shop pair. Without
`.all_shops()`, `.shop(..)` or `.shops(..)`, the cron runs once, for the
synthetic `__global__` shop, which has no ordinary Shopify installation:
a flow that calls Shopify, like this one, fails there. For a shop-specific
integration, set the target explicitly, for example
`.shop("example.myshopify.com")`. Configure credentials for every targeted shop
before enabling the schedule.

The app refuses to start with an invalid schedule, a cron that names a flow
it does not list, or an `input` its flow does not accept: `{ "limit": 0 }`
here stops it with
`[openshop] Invalid config: crons[0].input is not an input of flow "syncOrders": …`.
Verify a new run appears after the next five-minute boundary.
The default concurrency policy rejects a second active run for the same app,
shop, and flow.

## 4. Make repeated writes safe

Each scheduled run can include orders sent by an earlier run. Make the warehouse
write an upsert keyed by the Shopify order ID, or use the warehouse's idempotency
mechanism. Completed checkpoints are reused within one run; separate scheduled
runs do not share them.

If you need a different retry policy, set it in the `#[flow]` attribute, for
example
`retry_policy = RetryPolicy::new().max_attempts(5).initial_interval(Duration::from_secs(2))`,
and consult the
[flow reference](../reference/flows.md#retry-precedence-and-defaults).
For the failure model, read
[Checkpoints and retries](../concepts/checkpoints-and-retries.md).
