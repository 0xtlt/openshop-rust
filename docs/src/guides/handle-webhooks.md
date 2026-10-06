# Handle a Shopify webhook

Use this guide to receive `orders/create` events in an installed development app
with `read_orders` access. There are two registrations: the handler in OpenShop
and the subscription in Shopify.

## 1. Define the handler

Create `src/webhooks/orders_create.rs`:

```rust,ignore
use openshop::Webhook;

pub fn orders_create() -> Webhook {
    Webhook::new(|ctx| async move {
        println!(
            "Received Shopify event: topic={} shop={} app={}",
            ctx.topic, ctx.shop, ctx.shopify_app
        );
        Ok(())
    })
}
```

Declare it in a new `src/webhooks/mod.rs`:

```rust,ignore
pub mod orders_create;
```

The example logs identity and topic without logging the order payload
(`ctx.payload`). OpenShop verifies Shopify's signature before invoking it.

## 2. Register it in OpenShop

Add `mod webhooks;` next to the other `mod` lines of `src/main.rs` and register
the handler on the builder. Preserve your existing flows and other settings:

```rust,ignore
fn config() -> Result<OpenShopConfig> {
    OpenShopConfig::builder()
        // ... providers, flows and other settings ...
        .webhook("orders/create", webhooks::orders_create::orders_create())
        .build()
}
```

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
with `.max_request_body_bytes(..)` on the builder.

> **Caution: handler errors are acknowledged.**
> OpenShop logs handler errors and panics and still returns HTTP 200. Shopify
> will not retry a delivery because your handler failed. For retryable work,
> enqueue a flow from the handler and monitor dispatch failures as well as flow
> failures.

To enqueue a flow, dispatch it for the shop and app that received the event:

```rust,ignore
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

Use a stable business identifier to make duplicate deliveries safe. See the
[webhook response contract](../reference/webhooks.md#response-behavior) and the
[flow dispatch API](../reference/sdk.md#appdispatch_flowparams) before adding
side effects.
