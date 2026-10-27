# Webhooks

To set up event delivery, see [Handle a Shopify webhook](../guides/handle-webhooks.md).

A webhook handler is an async function declared with `#[webhook("topic")]`.

```rust,ignore
// src/webhooks/orders_create.rs
use openshop::prelude::*;
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
use openshop::prelude::*;

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

The payload is a plain `#[derive(Deserialize)]` type, not an `#[input]`:
Shopify names its fields in snake case, and `#[input]` reads camel case.

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
| `shop` | `String` | `X-Shopify-Shop-Domain`, lowercased |
| `shopify_app` | `String` | App selected by matching the body HMAC against its secret |
| `payload` | `serde_json::Value` | Parsed JSON request body |
| `api_version` | `String` | `X-Shopify-Api-Version` |
| `db` | `PgPool` | The database of the application |
| `shopify` | `ShopifyClient` | The Admin API of the shop |
| `connectors` | `Connectors` | The providers of the shop |
| `app` | `OpenShop` | The running application |

A missing header yields an empty string, except `X-Shopify-Shop-Domain`: a
handler only runs for a shop written in full as `<name>.myshopify.com`; a bare
name or a value such as `__global__` is answered `400`. The signature covers
the body and not that header, and a delivery can be replayed, so the `shopify`
client of a webhook handler, like a flow the handler dispatches, is background
work: with the
[client credentials grant](authentication.md#client-credentials-grant), it may
get a token for the shop of the `client_credentials_shop` setting, and renew once,
after a `401`, the token of a shop whose installation row holds one and is not
marked uninstalled; it never gets a token for another shop, nor creates a row
for it.

Nothing is read when the context is built: the access token of the shop is
read at the first Shopify request, and the provider configuration at the first
provider call. A handler that makes neither costs no query.

```rust,ignore
#[webhook("orders/create")]
pub async fn orders_create(ctx: WebhookContext, order: CreatedOrder) -> Result<()> {
    // The Admin API of the shop that sent the event.
    let data = ctx.shopify.execute(graphql!("{ shop { name } }")).await?;

    // A provider, with the configuration the shop saved.
    let accepted = Warehouse::on(&ctx.connectors).push(&vec![order.name]).await?;

    // The rows of the shop.
    let reviews = Review::of(&ctx).count().await?;

    println!("[webhook] {}: {accepted} order sent, {reviews} reviews", data.shop.name);
    Ok(())
}
```

`WebhookContext` belongs to a shop, so what needs one takes the context
itself: `Review::of(&ctx)`, and `sync_orders::dispatch(&ctx, …)` as shown
under [Idempotence and durable work](#idempotence-and-durable-work).

OpenShop verifies `X-Shopify-Hmac-Sha256` against the raw bytes of the request
body before parsing JSON, with `shopify_api`'s `Webhook::verify`: a body that
is not UTF-8 is not converted first, so the signature is of what Shopify sent
and of nothing else. Only a JSON object verifies, a body whose first byte is
`{`: the secret also signs launches, app proxy requests and session tokens,
none of which starts so. The `X-Shopify-Topic` and `X-Shopify-Shop-Domain`
headers must be there, as Shopify always sends them. In multi-app mode,
exactly one configured app secret must match.

OpenShop also serves `POST /auth/webhooks/app-uninstalled`. It verifies the
same signature, marks the installation as uninstalled, and deletes its access
token. The shop is the one both the signed payload (`myshopify_domain`) and
the unsigned `X-Shopify-Shop-Domain` header name; a delivery where they differ
uninstalls nothing. Point
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
| `400 { "error": "Invalid shop domain" }` | A handler matched and `X-Shopify-Shop-Domain` is not a shop written as `<name>.myshopify.com`. |
| `401 { "error": "Invalid webhook HMAC" }` | The signature did not verify with exactly one configured app, the body is not a JSON object, or a header Shopify always sends (`X-Shopify-Topic`, a valid `X-Shopify-Shop-Domain`) is missing. |
| `413` | Request body is larger than the request body limit (2 MB unless the `max_request_body_bytes` setting of `app!` is set). |
| `500 { "error": "SHOPIFY_API_SECRET is not configured" }` | No Shopify app secret is configured. A secret that is empty or only whitespace counts as not configured. |

Handler errors deliberately return 200, so Shopify will not retry them.
OpenShop does not provide automatic retries for webhook handlers.

## Idempotence and durable work

Shopify can deliver the same event more than once, with the same
`X-Shopify-Webhook-Id` (or `X-Shopify-Event-Id`, when the former is missing).
After the signature is verified, OpenShop remembers a delivery whose handler
completed for an hour and answers `200` to a repeat without running the handler
again. A delivery is only remembered once its handler has completed: if the
handler returns an error or panics, or Shopify hangs up before it ends (the
request is cancelled), nothing is remembered and the delivery that Shopify
sends again runs. While a handler is still running, a repeat of the same
delivery is answered `503` with `Retry-After: 5`, not `200`, so that Shopify
comes back and finds either the handler completed or the delivery free to run. The memory is
that of the process: another replica, or a restart, does not know a delivery,
and a handler can still run twice for one event. The key is the app, the topic,
the id and the signature of the body, so a request that repeats an id with
another body is another delivery. A delivery without either header is never
taken for a repeat. The context does not expose the id.

Make side effects idempotent using a stable identifier from the payload, or
dispatch a flow whose first step checks a durable app-owned record.

For work that must retry, keep the webhook handler short and dispatch a flow:

```rust,ignore
use openshop::prelude::*;

use crate::flows::sync_orders::{SyncOrdersInput, sync_orders};

#[webhook("orders/create")]
pub async fn orders_create(ctx: WebhookContext) -> Result<()> {
    sync_orders::dispatch(&ctx, SyncOrdersInput { limit: 1 }).await?;
    Ok(())
}
```

`sync_orders::dispatch` is added by `#[flow]`. It queues a run for the shop
and the Shopify app of the context, and returns a `QueuedRun`. A flow without
input is dispatched with the context alone: `nightly::dispatch(&ctx).await?`.

Do not rely on returning an error from the handler: OpenShop acknowledges the
delivery. A dispatch rejected because the flow already has an active run for
the shop is such an error; see [Concurrency](flows.md#concurrency). So is a
dispatch refused because the input fails the input schema of the flow
(`InvalidFlowInput`): the error is logged, no run is queued, and Shopify still
gets its `200`, so it does not redeliver a payload that can never be accepted.

## Without the attribute

`#[webhook]` is built on `Webhook`, which can be used directly for a handler
that is only known at run time, or in a test:

```rust,ignore
use openshop::builder::Webhook;
use openshop::prelude::*;

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
