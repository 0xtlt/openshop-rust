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
use openshop::{RouteAuthOutcome, ServerRoute, json, response};
use subtle::ConstantTimeEq;

pub fn status() -> ServerRoute {
    ServerRoute::authenticated(|ctx| async move {
        let Some(token) = std::env::var("INBOUND_API_TOKEN").ok().filter(|token| !token.is_empty()) else {
            return Ok(RouteAuthOutcome::Unauthorized);
        };
        let expected = format!("Bearer {token}");
        let supplied = ctx.request.header("authorization").unwrap_or_default();

        Ok(if bool::from(supplied.as_bytes().ct_eq(expected.as_bytes())) {
            RouteAuthOutcome::Authenticated(json!({ "service": "warehouse" }))
        } else {
            RouteAuthOutcome::Unauthorized
        })
    })
    .get(|ctx| async move { Ok(response::json(json!({ "ok": true, "service": ctx.auth["service"] }))) })
}
```

Declare it in a new `src/routes/mod.rs` (`pub mod status;`), add `mod routes;`
next to the other `mod` lines of `src/main.rs`, and register the route on the
builder, keeping your other settings:

```rust,ignore
fn config() -> Result<OpenShopConfig> {
    OpenShopConfig::builder()
        // ... providers, flows and other settings ...
        .route("/status", routes::status::status())
        .build()
}
```

The route is served at `GET /routes/status`. Returning
`RouteAuthOutcome::Unauthorized` from the authentication function rejects the
request with 401; the value inside `RouteAuthOutcome::Authenticated` becomes
`ctx.auth` in the handler. The handler returns an HTTP response: use the helpers
in `openshop::response` (`json`, `json_with_status`, `text`, `status`) or build
any response with the custom status and headers you need.

Both functions read the request through `ctx.request`: `header(name)`,
`query()`, `json()`, `text()`, and the raw `body`. A body larger than 2 MB is
refused with HTTP 413 before your code runs; change the limit with
`.max_request_body_bytes(..)` on the builder.

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
`ctx.for_shop(RouteShopInput::new(shop))`, map the authenticated caller to a
shop it is authorized to access; a shop name in a request is not proof of
access. The [server route reference](../reference/server-routes.md)
shows the context contract and how an authenticated route dispatches a flow.
