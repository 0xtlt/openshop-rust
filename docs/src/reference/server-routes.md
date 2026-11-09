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
| `logger` | `ContextLogger` | Logs to the runtime logger with the `route`; see [Logging](#logging). |
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
`logger` and `app`, so both may read the body. It returns a `RouteAuthOutcome`:

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
| `logger` | `ContextLogger` | Logs with the `route`, the `shop` and the `shopifyApp`; see [Logging](#logging). |
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
installed, or before anything is asked of it: a link in an email, a welcome
link opened outside the admin. These read the configuration only, and take the
shop as `for_shop` does:

| Method | Returns |
| --- | --- |
| `ctx.app_handle(shop)` | `Result<&str>`: the handle of the app on Shopify, which the URLs of the admin name it by (see [Billing](billing.md#the-handle-of-the-app)). An error when `shopify.app.toml` has no `handle`. |
| `ctx.admin_url(shop, path)` | `Result<String>`: a page of the admin of the shop, `https://admin.shopify.com/store/{store}/{path}`, at the `admin_origin` of the [Shopify endpoints](../guides/custom-shopify-endpoints.md) when there is one. |
| `ctx.pricing_url(shop)` | `Result<String>`: the plan selection page of the app for the shop, as `billing::pricing_url` gives it. An error without a `handle`, as `app_handle`. |

```rust,ignore
use openshop::prelude::*;
use openshop::response::{IntoResponse, Response, StatusCode, header};

/// A link to the app in the admin of `?shop=`, for an email.
#[route(GET, "/open")]
pub async fn open(ctx: RouteContext) -> Result<Response> {
    let shop = ctx.request.query().get("shop").cloned().unwrap_or_default();
    let location = ctx.admin_url(&shop, &format!("apps/{}", ctx.app_handle(&shop)?))?;
    Ok((StatusCode::FOUND, [(header::LOCATION, location)]).into_response())
}
```

An authentication function has them too. They fail as `for_shop` does when the
shop is not a `*.myshopify.com` domain, or the app handle is unknown or missing
while several apps are configured; whether the shop installed the app is not
asked. `app_handle` and `pricing_url` also fail when the `shopify.app.toml` of
the app has no `handle`.

A route that wants a context of the shop, with `app_handle()` and
`admin_url(path)` of its own, calls `for_shop`.

Do not redirect a launch of the admin to the admin: the route runs in the
app's frame, and Shopify's admin cannot be framed. The welcome link of a plan is such a
launch: see [A launch Shopify signed](#a-launch-shopify-signed).

## A launch Shopify signed

The admin opens a path of the app in its frame with a query Shopify signs:
`hmac`, `timestamp`, `shop`, `host`, and what the app's own link adds, such as
the `plan_handle` of the welcome link of a plan. Anyone can call a route with
any query. `ctx.shopify_launch()` says whether Shopify signed this one:

| Method | Returns |
| --- | --- |
| `ctx.shopify_launch()` | `Option<ShopifyLaunch>`: the launch when the query verifies. `None` without `hmac`, with one that does not verify, older than 90 seconds, or for a `shop` that is not a `*.myshopify.com` domain. `RouteAuthContext` has it too. |

It checks what the admin UI checks for its own launches: the `hmac` is the one
the secret of a Shopify app of the application gives, the `timestamp` is of
the last 90 seconds, and one app only matches.

| `ShopifyLaunch` | Type | Contract |
| --- | --- | --- |
| `shop` | `String` | The shop of the launch, normalized. |
| `shopify_app` | `String` | The Shopify app whose secret signed it, as the configuration names it: `default` for the app of the environment. |
| `params` | `IndexMap<String, String>` | The parameters of the query, the first value of each. |
| `query` | `String` | The query as Shopify signed it, without `?`: a redirect to another page of the app in the same frame passes it on, `/?{query}`, for that page to verify it too. |
| `param(name)` | `Option<&str>` | A parameter. |
| `plan_handle()`, `charge_id()` | `Option<&str>` | What the welcome link of a plan carries. |
| `is_plan_welcome()` | `bool` | Whether it carries `plan_handle` or `charge_id`. |

The shop of a launch is the one the admin vouched for, not one a caller named:
`ctx.for_shop(&launch)` resolves it, for the app that signed it. Whether the
shop installed the app is not part of the launch: `for_shop` checks it.

The welcome link of a plan reads the plan again only for a launch Shopify
signed, and as a best effort:

```rust,ignore
use openshop::prelude::*;
use openshop::response::{IntoResponse, Response, StatusCode, header};

/// The welcome link of a plan, which the admin opens in the app's frame.
#[route(GET, "/welcome")]
pub async fn welcome(ctx: RouteContext) -> Result<Response> {
    let Some(launch) = ctx.shopify_launch() else {
        // Not signed by Shopify: the app's page in the admin, and no Partner API call.
        let shop = ctx.request.query().get("shop").cloned().unwrap_or_default();
        let location = ctx.admin_url(&shop, &format!("apps/{}", ctx.app_handle(&shop)?))?;
        return Ok((StatusCode::FOUND, [(header::LOCATION, location)]).into_response());
    };
    // Only a launch Shopify signed reads the plan again: an open route must
    // not spend the organization's rate limit of the Partner API.
    if launch.is_plan_welcome() {
        let refreshed = async { ctx.for_shop(&launch).await?.shopify.refresh_plans().await };
        if let Err(error) = refreshed.await {
            ctx.logger.warn(format!("The plan of {} was not read again: {error:#}", launch.shop));
        }
    }
    // Still in the admin's frame: the app's home, with the signed query.
    let location = format!("/?{}", launch.query);
    Ok((StatusCode::FOUND, [(header::LOCATION, location)]).into_response())
}
```

The route must not use `?` on `for_shop` or `refresh_plans()`: when the
Partner API fails, a merchant who just paid would land on a `500`. A call
without the signature asks the Partner API nothing: an open route must not
spend the organization's rate limit. See
[Billing](billing.md#the-welcome-link-of-a-route) for why the plan is read
anyway at the next check.


## Logging

`ctx.logger` is a `ContextLogger`, as in a webhook or an admin page: `info`,
`warn`, `error`, and their `_with` forms, which take a JSON payload. Each line
goes to the runtime logger at its level, with its payload and what it was
logged for:

| Logger | Fields added to the payload |
| --- | --- |
| `RouteContext`, `RouteAuthContext` | `route`: the method and the declared path, `POST /orders/:id` |
| `RouteShopContext`, from `ctx.for_shop(…)` | `route`, `shop`, `shopifyApp` |

```rust,ignore
#[route(POST, "/orders/:id", json)]
pub async fn order(ctx: RouteContext) -> Result<Value> {
    ctx.logger.info_with("Order received", json!({ "id": ctx.params["id"] }));
    let tenant = ctx.for_shop("demo.myshopify.com").await?;
    tenant.logger.warn("No warehouse for this shop");
    Ok(json!({ "ok": true }))
}
```

A key of the payload wins over a field of the same name. A route has no run:
its lines are not stored in the database. When the application reports to
[Sentry](sentry.md), they are the breadcrumbs of a failure of the route, without
their payload. See [Logging](logging.md#handler-logger).

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
