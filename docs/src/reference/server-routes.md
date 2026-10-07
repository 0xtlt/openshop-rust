# Public server routes

To add an endpoint, see [Add a server route](../guides/add-server-route.md).

A server route is an async function declared with `#[route(METHOD, "/path")]`,
listed in the `routes` of `app!` and mounted under `/routes/*`. Route functions
receive the raw request and return an HTTP response. They do not require
Shopify authentication.

```rust,ignore
// src/routes/ping.rs
use openshop::response::{self, Response};
use openshop::{Result, ServerRouteContext, json, route};

#[route(GET, "/ping")]
pub async fn ping(_ctx: ServerRouteContext) -> Result<Response> {
    Ok(response::json(json!({ "ok": true })))
}
```

```rust,ignore
use openshop::{App, app};

use routes::ping::ping;

pub static APP: App = app! {
    routes: [ping],
};
```

The endpoint is available at `GET /routes/ping`. A route declared without
`auth` is public: leave `auth` out only when the endpoint is intentionally
unauthenticated.

The attribute leaves the function an ordinary function and adds a type of the
same name, which is what `routes` lists, with the constants `ping::METHOD` and
`ping::PATH`. Because of that type, do not re-export the function next to a
module of the same name (`pub use ping::ping;` beside `pub mod ping;` does not
compile): import it where the application is declared.

## Routing

| Declared path | Endpoint |
| --- | --- |
| `/` | `/routes` |
| `/callback` | `/routes/callback` |
| `/orders/:id` | `/routes/orders/:id` |

Dynamic segments are written `:name`; the file-style `[name]` is accepted too.
Static segments take precedence over dynamic ones. Two paths with the same
shape, such as `/orders/:id` and `/orders/:slug`, are rejected when the
configuration is built.

The method is `GET`, `HEAD`, `POST`, `PUT`, `PATCH`, `DELETE`, or `OPTIONS`. A
path can have several methods, each declared by its own function:
`#[route(GET, "/status")]` on one, `#[route(POST, "/status")]` on another, both
listed in `routes`. Two functions that declare the same method of a path are
rejected when the configuration is built
(`[openshop] Invalid config: routes./status handles GET twice`).
A request using a method that no function declares receives `405` with an
`Allow` header and the body `{ "error": "Method not allowed" }`.

`/routes` is only mounted when at least one server route is registered.

## Request and response

A route function takes a `ServerRouteContext` and returns `Result<T>`, where
`T` is anything that is a response (`openshop::response::IntoResponse`):
`openshop::response::Response`, `(StatusCode, String)`, and so on.

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

A function can return any type that implements `IntoResponse` as it is, for
example `Result<(StatusCode, String)>`. When its branches build different
types, it returns `Result<Response>` and calls `.into_response()` on each
value, for example
`(StatusCode::CREATED, [("x-request-id", "1")], response::json(body)).into_response()`.

OpenShop reads the whole request body before calling your code, does not parse
it, and does not add CORS headers for public routes. A body larger than the
[request body limit](configuration.md#settings) is answered with `413`.
Responses of 1 KB or more are compressed with gzip or Brotli when the client
accepts it.

## Authentication

A route function is public unless its attribute names an authentication
function: `#[route(POST, "/orders/:id", auth = partner)]`. Each route function
has its own authentication, so the methods of one path can differ, for example
a public `GET` next to an authenticated `POST`.

The authentication function is an
`async fn(ServerRouteRequestContext) -> Result<RouteAuthOutcome>`. It runs
before the route function and receives the same `request`, `params`, and
`app`, so both may read the body. It returns a `RouteAuthOutcome`:

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
use openshop::response::{self, Response, StatusCode};
use openshop::{
    DispatchFlowParams, Result, RouteAuthOutcome, ServerRouteContext, ServerRouteRequestContext, json, route,
};

use crate::integrations::warehouse::authenticate_warehouse;

async fn caller(ctx: ServerRouteRequestContext) -> Result<RouteAuthOutcome> {
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
}

#[route(POST, "/warehouse", auth = caller)]
pub async fn warehouse(ctx: ServerRouteContext) -> Result<Response> {
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
}
```

The flow name must be a registered flow key. The input is validated by the
worker when the run starts, not at dispatch; see [Input](flows.md#input).

## Without the attribute

`#[route]` is built on `ServerRoute`, which can be used directly for a route
that is only known at run time, or in a test:

```rust,ignore
use openshop::{ServerRoute, json, response};

pub fn ping() -> ServerRoute {
    ServerRoute::public().get(|_ctx| async move { Ok(response::json(json!({ "ok": true }))) })
}
```

Register it with `.route("/ping", ping())` on `OpenShopConfig::builder()`, or
on `APP.builder()?` to add it to a declared application.

`ServerRoute::public()` and `ServerRoute::authenticated(auth)` start a
definition. It has one method per supported HTTP method: `.get(…)`,
`.head(…)`, `.post(…)`, `.put(…)`, `.patch(…)`, `.delete(…)`, and
`.options(…)`. Each takes a handler that returns
`Result<openshop::response::Response>` and keeps the authentication of the
definition it is added to: `ServerRoute::handlers` maps a method to its
`ServerRouteMethod`, the handler with its authentication.

Calling `.route(path, …)` again for a path adds methods to it, each with the
authentication of its own definition; a method registered twice is rejected as
for route functions. A definition without any handler is rejected when the
configuration is built
(`[openshop] Invalid server route <path>: define at least one HTTP method`).
