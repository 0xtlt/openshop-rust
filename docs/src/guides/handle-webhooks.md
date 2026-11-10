# Handle a Shopify webhook

Use this guide to receive `orders/create` events in an installed development app
with `read_orders` access. There are two registrations: the handler in OpenShop
and the subscription in Shopify.

## 1. Define the handler

Create `src/webhooks/orders_create.rs`:

```rust,ignore
use openshop::prelude::*;

#[webhook("orders/create")]
pub async fn orders_create(ctx: WebhookContext) -> Result<()> {
    println!(
        "Received Shopify event: topic={} shop={} app={}",
        ctx.topic, ctx.shop, ctx.shopify_app
    );
    Ok(())
}
```

Declare the module in a new `src/webhooks/mod.rs`:

```rust,ignore
pub mod orders_create;
```

The example logs identity and topic without logging the order payload
(`ctx.payload`). OpenShop verifies Shopify's signature before invoking it.

`#[webhook]` names the topic the function handles, and accepts several:
`#[webhook("orders/create", "orders/updated")]`. To read fields of the order,
give the function a second argument whose type implements `Deserialize`, such
as `order: CreatedOrder`; the [webhook reference](../reference/webhooks.md)
shows it. Derive `Deserialize` from `serde` on that type and do not mark it
`#[input]`: Shopify names the fields of a payload in snake case.

The context also carries what the handler needs to work for the shop:
`ctx.db`, the database pool, `ctx.shopify`, the Admin API of the shop, and
`ctx.connectors`, its providers. Nothing is read when the context is built:
the access token is read at the first Shopify request, and the provider
configuration at the first provider call.

## 2. Register it in OpenShop

Add `mod webhooks;` next to the other `mod` lines and list the function in the
`webhooks` of `app!`. Preserve your existing flows and other entries:

```rust,ignore
use webhooks::orders_create::orders_create;

pub static APP: App = app! {
    // ... providers, flows and other entries ...
    webhooks: [orders_create],
};
```

The attribute adds a type named `orders_create` next to the function, which is
what `webhooks` lists. Import the function where the application is declared,
as above: re-exporting it from `src/webhooks/mod.rs`
(`pub use orders_create::orders_create;`) does not compile, because the module
has the same name.

The topic can be written `orders/create` or `ORDERS_CREATE`.

## 3. Subscribe in Shopify

In the linked Shopify app TOML, retain its `[webhooks]` API version and add:

```toml
[[webhooks.subscriptions]]
topics = ["orders/create"]
uri = "/webhooks"
```

Use Shopify's [app configuration reference](https://shopify.dev/docs/apps/build/cli-for-apps/app-configuration)
for subscription fields and the API version used by your app. Configure any
required order-data access for the development app.

Use `shopify app dev` to test the development configuration. When releasing it,
deploy the matching Shopify app configuration as well as your OpenShop build:

```bash
shopify app deploy --config shopify.app.toml
```

For multiple apps, repeat this for each relevant TOML file.

Subscribe to `app/uninstalled` on `/webhooks` too, even without a handler of
your own: OpenShop marks the installation as uninstalled and deletes its access
token when it arrives. A handler of the topic, if you write one, runs after
that, for the cleanup of your app. See
[Uninstallation](../reference/webhooks.md#uninstallation).

```toml
[[webhooks.subscriptions]]
topics = ["app/uninstalled"]
uri = "/webhooks"
```

## 4. Verify delivery and failure handling

Create a test order on the development store. Check the output of the OpenShop
**web process** for **Received Shopify event** and the expected shop and app
handle. This handler runs in the HTTP process, so its message is not a flow-run log.

If no message arrives, verify the subscription, destination URL, app installation,
and requested scopes. An unsigned direct `curl` request should return 401:

```bash
curl -i -X POST http://localhost:3000/webhooks -d '{}'
```

Run `cargo check` to check the handler before shipping.

Request bodies are limited to 2 MB by default, and a larger delivery is refused
with HTTP 413. If you subscribe to a topic with larger payloads, raise the limit
with the `max_request_body_bytes` setting of `app!`.

> **Caution: handler errors are acknowledged.**
> OpenShop logs handler errors and panics and still returns HTTP 200. Shopify
> will not retry a delivery because your handler failed. For retryable work,
> enqueue a flow from the handler and monitor dispatch failures as well as flow
> failures.

To enqueue a flow, dispatch it for the shop and app that received the event:

```rust,ignore
use openshop::prelude::*;

use crate::flows::sync_orders::{SyncOrdersInput, sync_orders};

#[webhook("orders/create")]
pub async fn orders_create(ctx: WebhookContext) -> Result<()> {
    sync_orders::dispatch(&ctx, SyncOrdersInput { limit: 10 }).await?;
    Ok(())
}
```

`#[flow]` adds `dispatch` to the flow it declares. It takes the context, which
names the shop and the Shopify app, then the input of the run, and returns a
`QueuedRun` with the `run_id` of the queued run. A flow without input is
dispatched with the context alone.

To delay the run, to give it another retry policy, or to dispatch a flow only
known by name, describe the run with `DispatchOptions`:
`ctx.app.dispatch_flow(DispatchOptions::for_scope("syncOrders", &ctx).input(json!({ "limit": 10 })).delay(Duration::from_secs(60)))`.

Use a stable business identifier to make duplicate deliveries safe. See the
[webhook response contract](../reference/webhooks.md#response-behavior) and the
[flow dispatch API](../reference/sdk.md#appdispatch_flowoptions) before adding
side effects.
