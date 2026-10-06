# Add proxy routes

Use this guide to add an endpoint to an existing app for a storefront or Customer
Account extension. Have the app installed on a development store. For callers
outside Shopify, use [Add a server route](add-server-route.md).

OpenShop mounts each proxy route registered on the builder under both
`/proxy/*` and `/ext/*`.

## 1. Create a route

Create `src/proxy/api_reviews.rs`:

```rust,ignore
use openshop::{ProxyDefinition, json};

pub fn api_reviews() -> ProxyDefinition {
    ProxyDefinition::json()
        .get(|ctx| async move {
            let page = ctx
                .query
                .get("page")
                .and_then(|page| page.parse::<u32>().ok())
                .filter(|page| *page > 0)
                .unwrap_or(1);
            Ok(json!({ "shop": ctx.shop, "customerId": ctx.customer_id, "page": page }))
        })
        .post(|ctx| async move {
            let Some(customer_id) = ctx.customer_id else {
                return Ok(json!({ "ok": false, "error": "Customer login required" }));
            };
            Ok(json!({ "ok": true, "shop": ctx.shop, "customerId": customer_id, "review": ctx.body }))
        })
}
```

Declare it in a new `src/proxy/mod.rs` (`pub mod api_reviews;`), add `mod proxy;`
next to the other `mod` lines of `src/main.rs`, and register the route on the
builder, keeping your other settings:

```rust,ignore
fn config() -> Result<OpenShopConfig> {
    OpenShopConfig::builder()
        // ... providers, flows and other settings ...
        .proxy("/api/reviews", proxy::api_reviews::api_reviews())
        .build()
}
```

Supported handlers are `get`, `post`, `put`, `delete`, and `patch`. Non-GET
bodies are parsed as JSON; an empty or invalid body becomes `None`. A body
larger than 2 MB is refused with HTTP 413 unless you raise the limit with
`.max_request_body_bytes(..)` on the builder.

## 2. Check the mounted paths

The path given to `.proxy(..)` is mounted under both prefixes:

```txt
.proxy("/", …)             -> /proxy and /ext
.proxy("/reviews", …)      -> /proxy/reviews and /ext/reviews
.proxy("/products/:id", …) -> /proxy/products/:id and /ext/products/:id
```

Read a dynamic segment from `ctx.params`, for example `ctx.params.get("id")`.
Static segments win over dynamic ones, so `/products/new` is matched before
`/products/:id`. `build()` fails when two paths can match the same URLs, such as
`/products/:id` and `/products/:slug`.

Handlers are ordinary functions: put shared queries and services in any module
of your crate. Nothing is discovered from the file system, so a route exists
only once it is registered with `.proxy(..)`.

## 3. Configure the Shopify app proxy

In the linked Shopify app TOML, configure the destination and storefront path:

```toml
[app_proxy]
url = "/proxy"
prefix = "apps"
subpath = "openshop"
```

Include Shopify's required `write_app_proxy` permission in the app's requested scopes and complete renewed
scope authorization when required. With this mapping, the route from step 1 is
reachable through `https://<shop>/apps/openshop/api/reviews`. Verify the fields
against Shopify's [app proxy configuration](https://shopify.dev/docs/apps/build/online-store/app-proxies)
for your app configuration version.

Deploy the configuration:

```bash
shopify app deploy --config shopify.app.toml
```

Registering a route with `.proxy(..)` does not register the app proxy in Shopify.

## 4. Call from a Customer Account extension

Enable direct network access in the extension TOML:

```toml
[extensions.capabilities]
network_access = true
```

In the extension's own code (JavaScript), call the `/ext` mount on your app
origin:

```js
const token = await shopify.sessionToken.get()

const response = await fetch(`${apiOrigin}/ext/api/reviews`, {
  headers: {
    Authorization: `Bearer ${token}`,
  },
})
```

`/ext/*` requires a valid Customer Account session JWT. `/proxy/*` accepts that
JWT too, but `/ext` avoids Shopify CLI proxy interception.

## 5. Verify authentication and CORS

For storefront app proxy traffic, Shopify signs the query and OpenShop verifies
it against exactly one configured app secret. For extension traffic, OpenShop
verifies the JWT audience and signature.

Trust `ctx.shop`, `ctx.shopify_app`, and `ctx.customer_id`; do not accept identity
from unsanitized client fields. A missing/invalid credential returns HTTP 401.

Shopify extension, admin, storefront, local, `HOST`, and `SHOPIFY_APP_URL`
origins receive CORS headers. Arbitrary origins do not.

Handler errors and panics are logged and return
`{ "error": "Internal proxy error" }` with HTTP 500. A successful handler value
is always HTTP 200; the current proxy API does not expose custom status or
response headers.

## 6. Verify the endpoint

Open `/apps/openshop/api/reviews?page=2` on the development storefront. Expect a
JSON response with the verified shop and `page: 2`. An unauthenticated request
directly to the OpenShop `/proxy/api/reviews` URL should return 401:

```bash
curl -i http://localhost:3000/proxy/api/reviews
```

Run `cargo check` before deploying. To cover the route in your test suite, use
the signed [proxy test client](test-app.md#add-other-coverage). See the
[proxy reference](../reference/proxy-routes.md) for all methods and response
limitations.
