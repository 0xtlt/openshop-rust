# Building blocks

OpenShop separates the code that knows **how to call a service** from the code
that decides **what work to do**. This keeps a warehouse client reusable across
order syncs, inventory updates, and reconciliation jobs.

## Providers become configured connectors

A **provider definition** describes an external service. It declares editable
configuration fields and methods such as `push`. A method is a closure that
receives the saved configuration and the call arguments: `|config, orders| …`.
OpenShop uses the fields to render a form in the embedded admin.

A merchant saves a different configuration for each shop. When work runs,
OpenShop loads that shop's configuration and binds it to the provider methods.
The resulting **connector** is called with the arguments only, as in
`ctx.connectors.call("warehouse", "push", &orders)`: application code does not
have to retrieve or pass credentials on every call.

The provider definition belongs to the codebase. The saved provider
configuration belongs to an app/shop pair. The connector belongs to the current
execution context. Registering a provider does not configure its credentials:
until a configuration is saved for a shop, its methods receive an empty one.

## Flows describe work; runs record executions

A **flow definition** describes a background job, its input type, and its run
function. A **flow run** is one execution of that definition, with its own
ID, input, status, step results, and logs.

Dispatching a flow stores a pending run. It does not synchronously execute the
job. A worker later claims the run and calls its function with the shop's
connectors and Shopify client.

A **step** is a named checkpoint inside that function. A completed checkpoint
stores a JSON result for reuse within the same run. See
[Checkpoints and retries](checkpoints-and-retries.md) for the guarantees.

## One builder assembles the application

The generated project keeps each definition in its own module and assembles
them in one place:

```text
src/providers/warehouse.rs    warehouse() returns a Provider
src/flows/sync_orders.rs      sync_orders() returns a Flow
        ↓ both called by
src/main.rs                   OpenShopConfig::builder() registers providers,
                              flows, schedules, and runtime settings
```

`OpenShopConfig::builder()` collects the definitions. `build()` validates them
as a whole and returns the configuration, and `openshop::cli::run(config)`
turns it into the commands of your binary.

A flow does not import the provider it uses. It reaches the connector by
provider and method name, so definitions depend neither on each other nor on
the final configuration. Those names are resolved when the call happens, not
when the project compiles: calling a provider that is not registered fails
with `Unknown provider "warehouse"`, and a method registered with
`typed_method` rejects arguments it cannot deserialize. A test that runs the
flow catches both; see [Test an app](../guides/test-app.md).

Code that needs the running application receives it as a value. Every handler
context carries `ctx.app`, an `OpenShop` handle that exposes `dispatch_flow()`,
the database pool, and the connectors and Shopify client of any shop:

```rust
use openshop::{DispatchFlowParams, Webhook, json};

pub fn orders_create() -> Webhook {
    Webhook::new(|ctx| async move {
        ctx.app
            .dispatch_flow(
                DispatchFlowParams::new("syncOrders", ctx.shop.clone())
                    .shopify_app(ctx.shopify_app.clone())
                    .input(json!({ "limit": 10 })),
            )
            .await?;
        Ok(())
    })
}
```

The flow is named by its registered key. With the default concurrency policy,
`dispatch_flow()` returns a `FlowConcurrencyError` while another run of the same
flow is active for that shop.

## Different entry points serve different callers

| Entry point | Typical caller | Role | Registered with |
| --- | --- | --- | --- |
| Embedded admin | Merchant or shop staff | Configure providers and inspect or trigger work | Built in |
| Cron | OpenShop scheduler | Dispatch work on a schedule | `.cron(…)` |
| Webhook | Shopify | Handle an event or enqueue a flow | `.webhook(topic, …)` |
| Proxy route | Storefront or Customer Account extension | Serve a Shopify-authenticated request | `.proxy(path, …)` |
| Server route | Another service | Handle an HTTP request with app-defined authentication | `.route(path, …)` |
| Custom admin page (experimental) | Shop staff | Use app-owned screens and authenticated loaders/actions | `.custom_pages(…)` and `.admin_page(…)` |

Each of these is a method of the configuration builder. Nothing is discovered
from the file system.

An HTTP handler can enqueue durable work and return promptly. It does not gain
flow checkpoints or retries merely by calling a provider.

Continue with [Architecture](architecture.md) for the process model, or
use the [configuration reference](../reference/configuration.md) for exact options.
