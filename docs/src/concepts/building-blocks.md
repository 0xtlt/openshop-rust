# Building blocks

OpenShop separates the code that knows **how to call a service** from the code
that decides **what work to do**. This keeps a warehouse client reusable across
order syncs, inventory updates, and reconciliation jobs.

## Providers become configured connectors

A **provider definition** describes an external service. It is a struct: its
fields are the editable configuration, and its public methods, such as `push`,
are what flows can do with a saved configuration, which they read as `&self`.
OpenShop uses the fields to render a form in the embedded admin.

A merchant saves a different configuration for each shop. At the first call
to a provider, OpenShop loads that shop's configuration and binds it to the
provider methods.
The resulting **connector** is called with the arguments only, as in
`Warehouse::on(&ctx.connectors).push(&orders)`: application code does not have
to retrieve or pass credentials on every call.

The provider definition belongs to the codebase. The saved provider
configuration belongs to an app/shop pair. The connector belongs to the current
execution context. Registering a provider does not configure its credentials:
until a configuration is saved for a shop, its methods receive an empty one.

## Flows describe work; runs record executions

A **flow definition** is a function: it describes a background job and the
type of its input. A **flow run** is one execution of that definition, with
its own ID, input, status, step results, and logs.

Dispatching a flow stores a pending run. It does not synchronously execute the
job. A worker later claims the run and calls its function with the shop's
connectors and Shopify client.

A **step** is a named checkpoint inside that function. A completed checkpoint
stores a JSON result for reuse within the same run. See
[Checkpoints and retries](checkpoints-and-retries.md) for the guarantees.

## One declaration lists the application

A block is declared where it is written, with an attribute on the function or
the struct itself, and the application lists its blocks in one place:

```text
src/providers/warehouse.rs    #[provider] on the impl block of Warehouse
src/flows/sync_orders.rs      #[flow("syncOrders")] on the function sync_orders
        ↓ both listed by
src/main.rs                   app! { providers: [Warehouse], flows: [sync_orders], … }
                              with the schedules and the runtime settings
```

`app!` declares an `App`: a value that names the blocks and holds the settings.
`APP.config()` validates them as a whole and returns the configuration, and
`APP.main()` turns it into the commands of your binary. Underneath, the
declaration fills a builder, which is also used directly in tests and for
blocks built at run time.

A flow names the provider struct it calls, and the compiler checks the method,
its arguments and its result. The call still goes through the connectors of
the shop rather than to the struct directly: that is what binds the saved
configuration, and what lets a test replace the provider with a fake. Whether
the provider is registered and configured for the shop is therefore a run-time
question: an unregistered provider fails with `Unknown provider "warehouse"`,
and a shop without configuration with
`Invalid configuration for provider "warehouse": missing field …`. A test that
runs the flow catches both; see [Test an app](../guides/test-app.md).

A method can also be reached by name, with
`ctx.connectors.call("warehouse", "push", &orders)`, when the provider or the
method is only known at run time. Nothing is checked at compile time then.

## Every context of a shop gives the same accesses

A handler receives a context. Every context that belongs to a shop carries
`ctx.db`, the database pool, `ctx.shopify`, the Admin API of the shop, and
`ctx.connectors`, its providers: the context of a flow, of a webhook, of a
proxy request, of a loader or an action of an admin page, of an MCP tool or
resource, and what `ctx.for_shop(..)` returns in a server route. Nothing is
read when a context is built: the access token is read at the first Shopify
request, and the provider configuration at the first provider call.

Such a context also names the shop and the Shopify app it runs for, so what
needs them takes the context itself. A webhook queues a run of a flow this way:

```rust
use openshop::prelude::*;

use crate::flows::sync_orders::{SyncOrdersInput, sync_orders};

#[webhook("orders/create")]
pub async fn orders_create(ctx: WebhookContext) -> Result<()> {
    sync_orders::dispatch(&ctx, SyncOrdersInput { limit: 10 }).await?;
    Ok(())
}
```

`#[flow]` adds `dispatch` to the flow it declares: it queues a run for the
shop and the Shopify app of the context, with an input of the type the flow
takes. With the default concurrency policy, it fails with a
`FlowConcurrencyError` while another run of the same flow is active for that
shop. The rows of a model are reached the same way, with `Review::of(&ctx)`.

Every context also carries `ctx.app`, the `OpenShop` handle of the running
application, for what the context does not cover:
`ctx.app.dispatch_flow(DispatchOptions::new(name, shop))` queues a flow only
known by name, or for another shop, and `ctx.app.shopify_client(shop, app)`
and `ctx.app.connectors(shop, app)` reach any shop. A declared flow exposes
its name as a constant, `sync_orders::NAME`.

## Different entry points serve different callers

| Entry point | Typical caller | Role | Declared with |
| --- | --- | --- | --- |
| Embedded admin | Merchant or shop staff | Configure providers and inspect or trigger work | Built in |
| Cron | OpenShop scheduler | Dispatch work on a schedule | A `Cron` in `crons` |
| Webhook | Shopify | Handle an event or enqueue a flow | `#[webhook("topic")]`, listed in `webhooks` |
| Proxy route | Storefront or Customer Account extension | Serve a Shopify-authenticated request | `#[proxy(GET, "/path")]`, listed in `proxies` |
| Server route | Another service | Handle an HTTP request with app-defined authentication | `#[route(GET, "/path")]`, listed in `routes` |
| Custom admin page (experimental) | Shop staff | Use app-owned screens and authenticated loaders/actions | `#[admin_page("/path", nav = "Label")]`, listed in `admin_pages` |

Apart from the embedded admin, each of these is an entry of `app!`. Nothing is
discovered from the file system.

An HTTP handler can enqueue durable work and return promptly. It does not gain
flow checkpoints or retries merely by calling a provider.

Continue with [Architecture](architecture.md) for the process model, or
use the [configuration reference](../reference/configuration.md) for exact options.
