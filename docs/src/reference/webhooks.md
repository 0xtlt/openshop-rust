# Webhooks

To set up event delivery, see [Handle a Shopify webhook](../guides/handle-webhooks.md).

A webhook handler is a `Webhook` built from an async function.

```rust,ignore
use openshop::{Value, Webhook};

pub fn orders_create() -> Webhook {
    Webhook::new(|ctx| async move {
        let name = ctx.payload.get("name").and_then(Value::as_str).unwrap_or_default();
        println!(
            "[webhook] {} on {} (app {}, API {}): {name}",
            ctx.topic, ctx.shop, ctx.shopify_app, ctx.api_version
        );
        Ok(())
    })
}
```

Register the definition on the configuration builder:

```rust,ignore
use openshop::{OpenShopConfig, Result};

fn config() -> Result<OpenShopConfig> {
    OpenShopConfig::builder()
        .webhook("orders/create", webhooks::orders_create::orders_create())
        .build()
}
```

Keys may also use Shopify's normalized form such as `ORDERS_CREATE`.
OpenShop first checks the exact topic (`orders/create`), then replaces `/` with
`_` and uppercases it.

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

Adding a handler to the configuration does not create a Shopify subscription.
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
| `200 { "ok": true }` | Handler returned an error or panicked; OpenShop logs the error. |
| `400 { "error": "Invalid JSON body" }` | Authenticated body is not valid JSON and a handler matched. |
| `401 { "error": "Invalid webhook HMAC" }` | HMAC did not match exactly one configured app. |
| `413` | Request body is larger than the request body limit (2 MB unless `.max_request_body_bytes(…)` is set). |
| `500 { "error": "SHOPIFY_API_SECRET is not configured" }` | No Shopify app secret is configured. A secret that is empty or only whitespace counts as not configured. |

Handler errors deliberately return 200, so Shopify will not retry them.
OpenShop does not provide automatic retries for webhook handlers.

## Idempotence and durable work

Shopify can deliver the same event more than once. `Webhook` does not
deduplicate deliveries and the context does not expose a webhook ID.
Make side effects idempotent using a stable identifier from the payload, or
dispatch a flow whose first step checks a durable app-owned record.

For work that must retry, keep the webhook handler short and dispatch a flow:

```rust,ignore
use openshop::{DispatchFlowParams, Webhook, json};

pub fn orders_create() -> Webhook {
    Webhook::new(|ctx| async move {
        ctx.app
            .dispatch_flow(
                DispatchFlowParams::new("syncOrders", ctx.shop.clone())
                    .shopify_app(ctx.shopify_app.clone())
                    .input(json!({ "limit": 1 })),
            )
            .await?;
        Ok(())
    })
}
```

Do not rely on returning an error from the handler: OpenShop acknowledges the
delivery. A dispatch rejected because the flow already has an active run for
the shop is such an error; see [Concurrency](flows.md#concurrency).
