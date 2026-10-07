# Public server routes

To add an endpoint, see [Add a server route](../guides/add-server-route.md).

Server routes are registered with `.route(path, …)` on the configuration
builder and mounted under `/routes/*`. Handlers receive the raw request and
return an HTTP response. They do not require Shopify authentication.

```rust,ignore
// src/routes/ping.rs
use openshop::{ServerRoute, json, response};

pub fn ping() -> ServerRoute {
    ServerRoute::public().get(|_ctx| async move { Ok(response::json(json!({ "ok": true }))) })
}
```

```rust,ignore
use openshop::{OpenShopConfig, Result};

fn config() -> Result<OpenShopConfig> {
    OpenShopConfig::builder()
        .route("/ping", routes::ping::ping())
        .build()
}
```

The endpoint is available at `GET /routes/ping`. Use `ServerRoute::public()`
only when the endpoint is intentionally unauthenticated.

## Routing

| Registered path | Endpoint |
| --- | --- |
| `/` | `/routes` |
| `/callback` | `/routes/callback` |
| `/orders/:id` | `/routes/orders/:id` |

Dynamic segments are written `:name`; the file-style `[name]` is accepted too.
Static segments take precedence over dynamic ones. Two paths with the same
shape, such as `/orders/:id` and `/orders/:slug`, are rejected when the
configuration is built.

`ServerRoute` has one builder method per supported HTTP method: `.get(…)`,
`.head(…)`, `.post(…)`, `.put(…)`, `.patch(…)`, `.delete(…)`, and
`.options(…)`. A route without any handler is rejected when the configuration
is built
(`[openshop] Invalid server route <path>: define at least one HTTP method`).
A request using a method without a handler receives `405` with an `Allow`
header and the body `{ "error": "Method not allowed" }`.

`/routes` is only mounted when at least one server route is registered.

## Request and response

Handlers receive a `ServerRouteContext` and return
`Result<openshop::response::Response>`.

| Field | Type | Contract |
| --- | --- | --- |
| `request.method` | `Method` | HTTP method. |
| `request.uri` | `Uri` | Request path and query. |
| `request.headers` | `HeaderMap` | Request headers. |
| `request.body` | `Bytes` | The complete request body. |
| `request.header(name)` | `Option<&str>` | A header value, when it is valid text. |
| `request.query()` | `IndexMap<String, String>` | Query parameters; the first value wins for a repeated key. |
| `request.json::<T>()` | `Result<T>` | The body deserialized from JSON. |
| `request.text()` | `String` | The body as text. |
| `params` | `IndexMap<String, String>` | Dynamic route parameters. |
| `auth` | `serde_json::Value` | Value produced by the authentication function; `null` on public routes. |
| `app` | `OpenShop` | The running application. |

`openshop::response` provides helpers to build responses and re-exports the
`axum` types they use:

| Helper | Response |
| --- | --- |
| `response::json(body)` | `200` with a JSON body. |
| `response::json_with_status(status, body)` | The given `StatusCode` with a JSON body. |
| `response::text(body)` | `200` with a `text/plain; charset=utf-8` body. |
| `response::status(status)` | An empty response with the given status. |

Anything that implements `IntoResponse` can be returned after calling
`.into_response()`, for example
`(StatusCode::CREATED, [("x-request-id", "1")], response::json(body)).into_response()`.

OpenShop reads the whole request body before calling your code, does not parse
it, and does not add CORS headers for public routes. A body larger than the
[request body limit](configuration.md#builder-methods) is answered with `413`.
Responses of 1 KB or more are compressed with gzip or Brotli when the client
accepts it.

## Authentication

Every route is created with either `ServerRoute::public()` or
`ServerRoute::authenticated(auth)`; a route cannot be built without choosing.
The authentication function receives a `ServerRouteRequestContext` with the
same `request`, `params`, and `app` as the handler, so both may read the body.
It returns a `RouteAuthOutcome`:

| Outcome | Effect |
| --- | --- |
| `RouteAuthOutcome::Authenticated(value)` | Calls the handler and exposes `value` as `ctx.auth`. |
| `RouteAuthOutcome::Unauthorized` | Responds `401` with `{ "error": "Unauthorized" }`. |
| `RouteAuthOutcome::Response(response)` | Stops with a custom response. |

An authentication function or handler that returns an error or panics is logged
and answered with a generic `500` response,
`{ "error": "Internal server route error" }`.

## Shop-scoped connectors

Authentication functions and handlers can call
`ctx.for_shop(RouteShopInput::new(shop))`, optionally with
`.shopify_app(handle)`. It resolves an active Shopify installation and returns
a `RouteShopContext` with the normalized `shop`, the resolved `shopify_app`,
and the provider `connectors` for that tenant. Provider configuration remains
encrypted at rest and is never included in the returned context.

`for_shop` fails when the shop is not a `*.myshopify.com` domain
(`[openshop] Invalid shop domain`), when the app handle is unknown or missing
while several apps are configured, or when the shop has no active installation
(`[openshop] Shop is not actively installed`).

Values used to select a shop are untrusted until the route authenticates the
request. Never treat a query parameter, path parameter, header, or decoded but
unverified payload as proof of identity.

## Queue durable work

Keep externally called routes short and move retryable work into a flow.
Dispatch a registered flow through `ctx.app` after the route has authenticated
the shop identity:

```rust,ignore
// src/routes/warehouse.rs
use openshop::response::{self, StatusCode};
use openshop::{DispatchFlowParams, RouteAuthOutcome, ServerRoute, json};

use crate::integrations::warehouse::authenticate_warehouse;

pub fn warehouse() -> ServerRoute {
    ServerRoute::authenticated(|ctx| async move {
        // Your own verification. It returns the `RouteShopInput` of the shop
        // the verified caller acts for, or `None`.
        let Some(identity) = authenticate_warehouse(&ctx.request).await? else {
            return Ok(RouteAuthOutcome::Unauthorized);
        };
        let tenant = ctx.for_shop(identity).await?;
        Ok(RouteAuthOutcome::Authenticated(json!({
            "shop": tenant.shop,
            "shopifyApp": tenant.shopify_app,
        })))
    })
    .post(|ctx| async move {
        let shop = ctx.auth["shop"].as_str().unwrap_or_default();
        let shopify_app = ctx.auth["shopifyApp"].as_str().unwrap_or_default();

        let queued = ctx
            .app
            .dispatch_flow(
                DispatchFlowParams::new("syncOrders", shop)
                    .shopify_app(shopify_app)
                    .input(json!({ "limit": 50 })),
            )
            .await?;

        Ok(response::json_with_status(StatusCode::ACCEPTED, queued))
    })
}
```

The flow name must be a registered flow key. The input is validated by the
worker when the run starts, not at dispatch; see [Input](flows.md#input).
