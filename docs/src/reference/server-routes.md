# Public server routes

To add an endpoint, see [Add a server route](../guides/add-server-route.md).

A server route is an async function declared with `#[route(METHOD, "/path")]`,
listed in the `routes` of `app!` and mounted under `/routes/*`. Route functions
receive the raw request and return an HTTP response, or a value sent as JSON.
They do not require Shopify authentication.

```rust,ignore
// src/routes/ping.rs
use openshop::prelude::*;

#[route(GET, "/ping", json)]
pub async fn ping(_ctx: RouteContext) -> Result<Value> {
    Ok(json!({ "ok": true }))
}
```

```rust,ignore
use openshop::prelude::*;

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

A route function takes a `RouteContext` and returns `Result<T>`. What `T` is
depends on the attribute:

| Attribute | Successful result |
| --- | --- |
| `#[route(GET, "/path", json)]` | Anything that serializes, sent as a `200` response with a JSON body. |
| `#[route(GET, "/path")]` | Anything that is a response (`openshop::response::IntoResponse`): `openshop::response::Response`, `(StatusCode, String)`, and so on. |

`RouteContext` has these fields:

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
| `db` | `PgPool` | The database of the application. |
| `app` | `OpenShop` | The running application. |

A route does not belong to a shop. To reach the Admin API or the providers of
one, see [Shop-scoped access](#shop-scoped-access).

Without `json`, a route function builds its response.
`openshop::response` provides helpers and re-exports the `axum` types they
use:

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
it unless the function takes a [typed input](#typed-input), and does not add
CORS headers for public routes. A body larger than the
[request body limit](configuration.md#settings) is answered with `413`.
Responses of 1 KB or more are compressed with gzip or Brotli when the client
accepts it.

## Typed input

A route function can take a second argument: its input, read into a type that
implements `Deserialize`.

| Method | Input |
| --- | --- |
| `GET`, `HEAD`, `DELETE`, `OPTIONS` | The query string. |
| `POST`, `PUT`, `PATCH` | The JSON body. |

```rust,ignore
// src/routes/orders.rs
use openshop::prelude::*;
use serde::Deserialize;

/// The body of `POST /routes/orders`.
#[derive(Debug, Deserialize)]
pub struct NewOrder {
    pub reference: String,
    pub quantity: u32,
}

#[route(POST, "/orders", json)]
pub async fn create_order(_ctx: RouteContext, order: NewOrder) -> Result<Value> {
    Ok(json!({ "reference": order.reference, "quantity": order.quantity }))
}
```

A request that does not fit the type is answered with `422` and
`{ "error": "Invalid input: <reason>" }`. The function does not run. The
[authentication function](#authentication) of the route runs first, so an
unauthenticated request is refused before its input is read.

The values of a query string are text: a field that is a number or a boolean
is read from its text, and the first value wins for a repeated key. An empty
body is read as `null`, which fits only a type that accepts it, such as an
`Option`.

A function refuses an input the same way by returning `InvalidInput`:
`bail!(InvalidInput("rating is 1 to 5".into()))` is answered with `422` and
`{ "error": "Invalid input: rating is 1 to 5" }`.

`ctx.request` and `ctx.params` stay available next to the input.

## Authentication

A route function is public unless its attribute names an authentication
function: `#[route(POST, "/orders/:id", auth = partner)]`. Each route function
has its own authentication, so the methods of one path can differ, for example
a public `GET` next to an authenticated `POST`.

The authentication function is an
`async fn(RouteAuthContext) -> Result<RouteAuthOutcome<A>>`. It runs
before the route function and receives the same `request`, `params`, `db`,
and `app`, so both may read the body. It returns a `RouteAuthOutcome`:

| Outcome | Effect |
| --- | --- |
| `RouteAuthOutcome::Authenticated(value)` | Calls the handler and exposes `value` as `ctx.auth`. |
| `RouteAuthOutcome::Unauthorized` | Responds `401` with `{ "error": "Unauthorized" }`. |
| `RouteAuthOutcome::Response(response)` | Stops with a custom response. |

`A` is what an authenticated request is known by. It is any type, and the
handler takes the context of that type:

```rust,ignore
use openshop::prelude::*;

pub struct Partner {
    pub key: String,
}

async fn partner(ctx: RouteAuthContext) -> Result<RouteAuthOutcome<Partner>> {
    Ok(match ctx.request.header("x-partner-key") {
        Some(key) => RouteAuthOutcome::Authenticated(Partner { key: key.to_string() }),
        None => RouteAuthOutcome::Unauthorized,
    })
}

#[route(POST, "/orders/:id", json, auth = partner)]
pub async fn update_order(ctx: RouteContext<Partner>) -> Result<Value> {
    Ok(json!({ "order": ctx.params["id"], "by": ctx.auth.key }))
}
```

The compiler checks that the two agree: a handler that takes a
`RouteContext<Partner>` under an authentication that returns something else
does not compile. `RouteAuthOutcome` and `RouteContext` without a type carry a
`serde_json::Value`, which is `Null` in the handler of a public route.

An authentication function or handler that returns an error or panics is logged
and answered with a generic `500` response,
`{ "error": "Internal server route error" }`.

## Shop-scoped access

Authentication functions and route functions can call
`ctx.for_shop("demo.myshopify.com").await?`. It resolves an active Shopify
installation and returns a `RouteShopContext`, which gives what the other
contexts give for a shop:

| `RouteShopContext` field | Type | Contract |
| --- | --- | --- |
| `shop` | `String` | The normalized shop domain. |
| `shopify_app` | `String` | The handle of the resolved Shopify app. |
| `db` | `PgPool` | The database of the application. |
| `shopify` | `ShopifyClient` | The Admin API of the shop. |
| `connectors` | `Connectors` | The providers of the shop. |
| `app` | `OpenShop` | The running application. |

`for_shop` takes anything that names a shop: a domain, or
`RouteShopInput::new(domain).shopify_app(handle)` when the application serves
several Shopify apps.

```rust,ignore
let tenant = ctx.for_shop("demo.myshopify.com").await?;

let reviews = Review::of(&tenant).all().await?;
let accepted = Warehouse::on(&tenant.connectors).push(&records).await?;
```

`for_shop` only checks the installation. The access token of the shop is read
at the first Shopify request, and the provider configuration at the first
provider call. Provider configuration remains encrypted at rest and is never
included in the returned context.

`RouteShopContext` belongs to a shop, so what needs one takes it:
`Review::of(&tenant)` and `sync_orders::dispatch(&tenant, …)`.

`for_shop` fails when the shop is not a `*.myshopify.com` domain
(`[openshop] Invalid shop domain`), when the app handle is unknown or missing
while several apps are configured, or when the shop has no active installation
(`[openshop] Shop is not actively installed`).

Values used to select a shop are untrusted until the route authenticates the
request. Never treat a query parameter, path parameter, header, or decoded but
unverified payload as proof of identity.

## Links to the Shopify admin

A route sends a merchant to the admin of a shop that may not have the app
installed, or before anything is asked of it: the welcome link of a plan, a
link in an email. These read the configuration only, and take the shop as
`for_shop` does:

| Method | Returns |
| --- | --- |
| `ctx.app_handle(shop)` | `Result<&str>`: the handle of the app on Shopify, which the URLs of the admin name it by (see [Billing](billing.md#the-handle-of-the-app)). |
| `ctx.admin_url(shop, path)` | `Result<String>`: a page of the admin of the shop, `https://admin.shopify.com/store/{store}/{path}`, at the `admin_origin` of the [Shopify endpoints](../guides/custom-shopify-endpoints.md) when there is one. |
| `ctx.pricing_url(shop)` | `Result<String>`: the plan selection page of the app for the shop, as `billing::pricing_url` gives it. |

```rust,ignore
use openshop::prelude::*;
use openshop::response::{IntoResponse, Response, StatusCode, header};

/// The welcome link of the plans: back to the app, in the admin of the shop.
#[route(GET, "/welcome")]
pub async fn welcome(ctx: RouteContext) -> Result<Response> {
    let shop = ctx.request.query().get("shop").cloned().unwrap_or_default();
    let app = format!("apps/{}", ctx.app_handle(&shop)?);
    let location = ctx.admin_url(&shop, &app)?;
    Ok((StatusCode::FOUND, [(header::LOCATION, location)]).into_response())
}
```

An authentication function has them too. They fail as `for_shop` does when the
shop is not a `*.myshopify.com` domain, or the app handle is unknown or missing
while several apps are configured; whether the shop installed the app is not
asked. A route that wants a context of the shop, with `app_handle()` and
`admin_url(path)` of its own, calls `for_shop`.

## Queue durable work

Keep externally called routes short and move retryable work into a flow.
Dispatch the flow for the shop after the route has authenticated the shop
identity:

```rust,ignore
// src/routes/warehouse.rs
use openshop::prelude::*;
use openshop::response::{self, Response, StatusCode};

use crate::flows::sync_orders::{SyncOrdersInput, sync_orders};
use crate::integrations::warehouse::authenticate_warehouse;

/// The shop a verified caller acts for.
pub struct Caller {
    pub shop: String,
}

async fn caller(ctx: RouteAuthContext) -> Result<RouteAuthOutcome<Caller>> {
    // Your own verification. It returns the domain of the shop the verified
    // caller acts for, or `None`.
    let Some(shop) = authenticate_warehouse(&ctx.request).await? else {
        return Ok(RouteAuthOutcome::Unauthorized);
    };
    Ok(RouteAuthOutcome::Authenticated(Caller { shop }))
}

#[route(POST, "/warehouse", auth = caller)]
pub async fn warehouse(ctx: RouteContext<Caller>) -> Result<Response> {
    let tenant = ctx.for_shop(&ctx.auth.shop).await?;

    let queued = sync_orders::dispatch(&tenant, SyncOrdersInput { limit: 50 }).await?;

    Ok(response::json_with_status(StatusCode::ACCEPTED, queued))
}
```

`sync_orders::dispatch` is added by `#[flow]`. It queues a run for the shop
and the Shopify app of the context it is given, and returns a `QueuedRun`.
The input is validated at dispatch, which fails for input the flow refuses, and again by the worker; see
[Input](flows.md#input).

For a flow that is only known by its name, or to set a delay or a retry
policy, use
`ctx.app.dispatch_flow(DispatchOptions::for_scope("syncOrders", &tenant).input(json!({ "limit": 50 })))`.
The name must be a registered flow key.

## Without the attribute

`#[route]` is built on `Route`, which can be used directly for a route
that is only known at run time, or in a test:

```rust,ignore
use openshop::builder::Route;
use openshop::prelude::*;

pub fn ping() -> Route {
    Route::public().get(|_ctx| async move { Ok(response::json(json!({ "ok": true }))) })
}
```

Register it with `.route("/ping", ping())` on `OpenShopConfig::builder()`, or
on `APP.builder()?` to add it to a declared application.

`Route::public()` and `Route::authenticated(auth)` start a
definition. It has one method per supported HTTP method: `.get(…)`,
`.head(…)`, `.post(…)`, `.put(…)`, `.patch(…)`, `.delete(…)`, and
`.options(…)`. Each takes a handler that returns
`Result<openshop::response::Response>` and keeps the authentication of the
definition it is added to: `Route::handlers` maps a method to its
`RouteMethod`, the handler with its authentication. A handler has no typed
input: it reads `ctx.request`.

Calling `.route(path, …)` again for a path adds methods to it, each with the
authentication of its own definition; a method registered twice is rejected as
for route functions. A definition without any handler is rejected when the
configuration is built
(`[openshop] Invalid server route <path>: define at least one HTTP method`).
