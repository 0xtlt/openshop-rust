# Webhooks

To set up event delivery, see [Handle a Shopify webhook](../guides/handle-webhooks.md).

A webhook handler is an async function declared with `#[webhook("topic")]`.

```rust,ignore
// src/webhooks/orders_create.rs
use openshop::{Result, WebhookContext, webhook};
use serde::Deserialize;

/// What the handler reads from the order Shopify sends.
#[derive(Deserialize)]
pub struct CreatedOrder {
    #[serde(default)]
    pub name: String,
}

#[webhook("orders/create")]
pub async fn orders_create(ctx: WebhookContext, order: CreatedOrder) -> Result<()> {
    println!(
        "[webhook] {} on {} (app {}, API {}): {}",
        ctx.topic, ctx.shop, ctx.shopify_app, ctx.api_version, order.name
    );
    Ok(())
}
```

List the function in the `webhooks` of `app!`:

```rust,ignore
use openshop::{App, app};

use webhooks::orders_create::orders_create;

pub static APP: App = app! {
    webhooks: [orders_create],
};
```

The function takes the `WebhookContext` and, optionally, the payload as a
second argument: a type that implements `Deserialize`, read from the JSON
body. Fields the type does not name are ignored. A payload that cannot be read
into the type is a handler error, and the function is not called; see
[Response behavior](#response-behavior). With or without the second argument,
the payload is available as JSON in `ctx.payload`.

An attribute can name several topics,
`#[webhook("orders/create", "orders/updated")]`; `ctx.topic` is the one that
was delivered. A topic named by two listed functions is rejected when the
configuration is built
(`[openshop] Invalid config: webhooks.orders/create is registered twice`).

Topics may also use Shopify's normalized form such as `ORDERS_CREATE`.
OpenShop first checks the exact topic (`orders/create`), then replaces `/` with
`_` and uppercases it.

The attribute leaves the function an ordinary function and adds a type of the
same name, which is what `webhooks` lists, with the constant
`orders_create::TOPICS`. Because of that type, do not re-export the function
next to a module of the same name (`pub use orders_create::orders_create;`
beside `pub mod orders_create;` does not compile): import it where the
application is declared.

## HTTP endpoint and context

Shopify sends all configured topics to `POST /webhooks`.

| `WebhookContext` field | Type | Shopify source |
| --- | --- | --- |
| `topic` | `String` | `X-Shopify-Topic` |
| `shop` | `String` | `X-Shopify-Shop-Domain` |
| `shopify_app` | `String` | App selected by matching the body HMAC against its secret |
| `payload` | `serde_json::Value` | Parsed JSON request body |
| `api_version` | `String` | `X-Shopify-Api-Version` |
| `app` | `OpenShop` | The running application, for dispatch, database, and connector access |

A missing header yields an empty string.

OpenShop verifies `X-Shopify-Hmac-Sha256` against the raw request body before
parsing JSON. In multi-app mode, exactly one configured app secret must match.

OpenShop also serves `POST /auth/webhooks/app-uninstalled`. It verifies the
same HMAC, marks the installation as uninstalled, and deletes its access token.
The shop is taken from the signed payload (`myshopify_domain`); the
`X-Shopify-Shop-Domain` header is only used when the payload has none. Point
Shopify's `app/uninstalled` subscription at it, or register your own
`app/uninstalled` handler on `/webhooks` when you need custom cleanup.

## Register with Shopify

Listing a webhook in `app!` does not create a Shopify subscription.
Declare or manage the corresponding subscription in your Shopify app
configuration and point it to `/webhooks`, then deploy that configuration using
Shopify CLI. Topics, API versions, protected-customer-data requirements, and
delivery retry policies are Shopify-owned contracts; verify them against the
Shopify version used by your app.

## Response behavior

| Status | Condition |
| --- | --- |
| `200 { "ok": true }` | Handler completed. |
| `200 { "ok": true }` | No handler matched; OpenShop logs a warning. |
| `200 { "ok": true }` | Handler returned an error or panicked, or the payload could not be read into the type of its second argument; OpenShop logs the error. |
| `400 { "error": "Invalid JSON body" }` | Authenticated body is not valid JSON and a handler matched. |
| `401 { "error": "Invalid webhook HMAC" }` | HMAC did not match exactly one configured app. |
| `413` | Request body is larger than the request body limit (2 MB unless the `max_request_body_bytes` setting of `app!` is set). |
| `500 { "error": "SHOPIFY_API_SECRET is not configured" }` | No Shopify app secret is configured. A secret that is empty or only whitespace counts as not configured. |

Handler errors deliberately return 200, so Shopify will not retry them.
OpenShop does not provide automatic retries for webhook handlers.

## Idempotence and durable work

Shopify can deliver the same event more than once. OpenShop does not
deduplicate deliveries and the context does not expose a webhook ID.
Make side effects idempotent using a stable identifier from the payload, or
dispatch a flow whose first step checks a durable app-owned record.

For work that must retry, keep the webhook handler short and dispatch a flow:

```rust,ignore
use openshop::{DispatchFlowParams, Result, WebhookContext, json, webhook};

#[webhook("orders/create")]
pub async fn orders_create(ctx: WebhookContext) -> Result<()> {
    ctx.app
        .dispatch_flow(
            DispatchFlowParams::new("syncOrders", ctx.shop.clone())
                .shopify_app(ctx.shopify_app.clone())
                .input(json!({ "limit": 1 })),
        )
        .await?;
    Ok(())
}
```

Do not rely on returning an error from the handler: OpenShop acknowledges the
delivery. A dispatch rejected because the flow already has an active run for
the shop is such an error; see [Concurrency](flows.md#concurrency).

## Without the attribute

`#[webhook]` is built on `Webhook`, which can be used directly for a handler
that is only known at run time, or in a test:

```rust,ignore
use openshop::{Value, Webhook};

pub fn orders_create() -> Webhook {
    Webhook::new(|ctx| async move {
        let name = ctx.payload.get("name").and_then(Value::as_str).unwrap_or_default();
        println!("[webhook] {} on {}: {name}", ctx.topic, ctx.shop);
        Ok(())
    })
}
```

Register it with `.webhook("orders/create", orders_create())` on
`OpenShopConfig::builder()`, or on `APP.builder()?` to add it to a declared
application. The handler takes the `WebhookContext` and reads the payload
from `ctx.payload`. A topic registered twice is rejected as for webhook
functions.
