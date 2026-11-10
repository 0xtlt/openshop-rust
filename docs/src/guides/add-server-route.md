# Add a server route

Use server routes for callers outside Shopify, such as a warehouse service. This
guide adds a token-protected status endpoint to an existing OpenShop app.
For storefront or Customer Account requests, use [proxy routes](add-proxy-routes.md).

## 1. Configure a service token

Generate a random token with `openssl rand -hex 32`. Save it as
`INBOUND_API_TOKEN` in the app's local `.env` and in the calling service's secret
configuration. `openshop dev`, which is also what `shopify app dev` runs in a
generated project, restarts the app when `.env` changes; restart development
yourself if you run `cargo run -- dev`.
In production, inject it through your deployment platform. This is an
application-defined variable, not a built-in OpenShop option.

## 2. Create the endpoint

The example compares the token in constant time with the `subtle` crate. Add it
to the project:

```bash
cargo add subtle
```

Create `src/routes/status.rs`:

```rust,ignore
use openshop::prelude::*;
use subtle::ConstantTimeEq;

/// What an authenticated request is known by.
pub struct Service {
    pub name: &'static str,
}

async fn service_token(ctx: RouteAuthContext) -> Result<RouteAuthOutcome<Service>> {
    let Some(token) = std::env::var("INBOUND_API_TOKEN").ok().filter(|token| !token.is_empty()) else {
        return Ok(RouteAuthOutcome::Unauthorized);
    };
    let expected = format!("Bearer {token}");
    let supplied = ctx.request.header("authorization").unwrap_or_default();

    Ok(if bool::from(supplied.as_bytes().ct_eq(expected.as_bytes())) {
        RouteAuthOutcome::Authenticated(Service { name: "warehouse" })
    } else {
        RouteAuthOutcome::Unauthorized
    })
}

#[route(GET, "/status", json, auth = service_token)]
pub async fn status(ctx: RouteContext<Service>) -> Result<Value> {
    Ok(json!({ "ok": true, "service": ctx.auth.name }))
}
```

`#[route]` declares the function as the handler of one method of a path, and
`auth = service_token` names the function that authenticates its requests.
Without `auth`, a route is public. With `json`, the function returns anything
that serializes, and the answer is sent as JSON.

Declare the module in a new `src/routes/mod.rs` (`pub mod status;`), add
`mod routes;` next to the other `mod` lines, and list the function in the
`routes` of `app!`, keeping your other entries:

```rust,ignore
use routes::status::status;

pub static APP: App = app! {
    // ... providers, flows and other entries ...
    routes: [status],
};
```

The attribute adds a type named `status` next to the function, which is what
`routes` lists. Import the function where the application is declared, as
above: re-exporting it from `src/routes/mod.rs` (`pub use status::status;`)
does not compile, because the module has the same name.

The route is served at `GET /routes/status`. Returning
`RouteAuthOutcome::Unauthorized` from the authentication function rejects the
request with 401; the value inside `RouteAuthOutcome::Authenticated` becomes
`ctx.auth` in the route function, with its type: the function takes a
`RouteContext<Service>`, and the compiler checks that both agree. Without `json`, the route function returns
anything that is an HTTP response: use the helpers of the `response` module
(`response::json`, `response::json_with_status`, `response::text`,
`response::status`) or build any response with the custom status and headers
you need.

Every route is served under `/routes/`, so that it never collides with
OpenShop's own paths (`/api`, `/auth`, `/webhooks`, `/proxy`, `/ext`,
`/health`, `/ops`) and the pages of the admin UI. When you port an app whose
URLs are set outside its code, point them at `/routes/…`: a managed pricing
welcome link in the Partner Dashboard (`/newplan` becomes `/routes/newplan`), a
link in an email, the App Store listing.

Both functions read the request through `ctx.request`: `header(name)`,
`query()`, `json()`, `text()`, and the raw `body`. A body larger than 2 MB is
refused with HTTP 413 before your code runs; change the limit with the
`max_request_body_bytes` setting of `app!`.

A route function can also take its input as a second argument, a type that
implements `Deserialize`:

```rust,ignore
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct StockUpdate {
    pub sku: String,
    pub quantity: i64,
}

#[route(POST, "/stock", json, auth = service_token)]
pub async fn update_stock(_ctx: RouteContext<Service>, update: StockUpdate) -> Result<Value> {
    Ok(json!({ "ok": true, "sku": update.sku, "quantity": update.quantity }))
}
```

The input is read from the query string for `GET`, `HEAD`, `DELETE` and
`OPTIONS`, and from the JSON body for `POST`, `PUT` and `PATCH`. A request
that does not fit the type is answered with HTTP 422 and
`{ "error": "Invalid input: …" }` before the function runs.

To answer another method on the same path, declare another function, such as
`#[route(POST, "/status", json, auth = service_token)]`, and list it too. Each
route function names its own authentication.

## 3. Check both authentication paths

With development running on its default port, call it without credentials:

```bash
curl -i http://localhost:3000/routes/status
```

Expect **401**. In your calling terminal, set `INBOUND_API_TOKEN` to the same
value stored in the app, then call:

```bash
curl --fail http://localhost:3000/routes/status \
  -H "Authorization: Bearer $INBOUND_API_TOKEN"
```

Expect `{"ok":true,"service":"warehouse"}`. Use the HTTPS app origin when
calling from a remote service. Run `cargo check` before deploying.

## Add shop-specific work

This token identifies a service, not a Shopify shop. Before using
`ctx.for_shop("example.myshopify.com")`, map the authenticated caller to a
shop it is authorized to access; a shop name in a request is not proof of
access.

`ctx.for_shop(..)` takes the domain of a shop, or
`RouteShopInput::new(domain).shopify_app(handle)` when the application serves
several Shopify apps, and fails when the shop is not actively installed. What
it returns belongs to that shop: it carries `db`, `shopify`, `connectors` and
`logger`, and it is what `sync_orders::dispatch(&shop, ..)` and
`Review::of(&shop)` take. The route context itself has `ctx.db`, and
`ctx.logger`, whose lines carry the route
(see [Logging](../reference/server-routes.md#logging)).

```rust,ignore
let shop = ctx.for_shop("example.myshopify.com").await?;
let accepted = Warehouse::on(&shop.connectors).push(&orders).await?;
```

The [server route reference](../reference/server-routes.md)
shows the context contract and how an authenticated route dispatches a flow.
