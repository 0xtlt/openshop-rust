# Proxy routes

To add a storefront endpoint, see [Add proxy routes](../guides/add-proxy-routes.md).

A proxy route is an async function declared with `#[proxy(METHOD, "/path")]`
and listed in the `proxies` of `app!`.

```rust,ignore
// src/proxy/reviews.rs
use openshop::prelude::*;

#[proxy(GET, "/reviews")]
pub async fn reviews(ctx: ProxyContext) -> Result<Value> {
    Ok(json!({ "shop": ctx.shop, "customerId": ctx.customer_id, "query": ctx.query }))
}
```

```rust,ignore
use openshop::prelude::*;

use proxy::reviews::reviews;

pub static APP: App = app! {
    proxies: [reviews],
};
```

The attribute leaves the function an ordinary function and adds a type of the
same name, which is what `proxies` lists, with the constants
`reviews::METHOD` and `reviews::PATH`. Because of that type, do not re-export
the function next to a module of the same name (`pub use reviews::reviews;`
beside `pub mod reviews;` does not compile): import it where the application
is declared.

## Routing

| Declared path | Mounted paths |
| --- | --- |
| `/` | `/proxy` and `/ext` |
| `/reviews` | `/proxy/reviews` and `/ext/reviews` |
| `/products/:id` | `/proxy/products/:id` and `/ext/products/:id` |

Dynamic segments are written `:name`; the file-style `[name]` is accepted too.
Parameter names may only contain letters, numbers, and `_`. Static segments
take precedence over dynamic ones, so `/products/new` wins over
`/products/:id`. Two paths that can match the same URLs, such as
`/products/:id` and `/products/:slug`, are rejected when the configuration is
built with
`[openshop] proxy routes "<a>" and "<b>" have the same route shape`.

`/proxy` and `/ext` are only mounted when at least one proxy route is
registered.

## Methods and request body

The method is `GET`, `POST`, `PUT`, `DELETE`, or `PATCH`. A path can have
several methods, each declared by its own function:
`#[proxy(GET, "/api/reviews")]` on one, `#[proxy(POST, "/api/reviews")]` on
another, both listed in `proxies`. Two functions that declare the same method
of a path are rejected when the configuration is built
(`[openshop] Invalid config: proxies./widgets handles GET twice`). Only
declared methods are served; a request with another method is answered with a
plain `404 Not Found`.

For non-GET methods, OpenShop reads the body and attempts to parse it as JSON.
An empty or invalid JSON body becomes `None`; it is not an automatic 400.
GET bodies are not read. A body larger than the
[request body limit](configuration.md#settings) is answered with `413`.

## Typed input

A proxy function can take a second argument: its input, read into a type that
implements `Deserialize`.

| Method | Input |
| --- | --- |
| `GET`, `DELETE` | The query string, `ctx.query`. |
| `POST`, `PUT`, `PATCH` | The JSON body, `ctx.body`. |

```rust,ignore
// src/proxy/api_reviews.rs
use openshop::prelude::*;
use serde::{Deserialize, Serialize};

/// The query of `GET /api/reviews`.
#[derive(Debug, Deserialize)]
pub struct ListReviews {
    pub page: Option<u32>,
}

#[proxy(GET, "/api/reviews")]
pub async fn list_reviews(ctx: ProxyContext, query: ListReviews) -> Result<Value> {
    Ok(json!({ "shop": ctx.shop, "page": query.page.unwrap_or(1) }))
}

/// The body of `POST /api/reviews`.
#[derive(Debug, Serialize, Deserialize)]
pub struct SubmittedReview {
    pub text: String,
    pub rating: u8,
}

#[proxy(POST, "/api/reviews")]
pub async fn submit_review(ctx: ProxyContext, review: SubmittedReview) -> Result<Value> {
    Ok(json!({ "ok": true, "shop": ctx.shop, "review": review }))
}
```

A request that does not fit the type is answered with `422` and
`{ "error": "Invalid input: <reason>" }` before the function runs.

The values of a query string are text: a field that is a number or a boolean
is read from its text. The parameters Shopify adds to sign a request (`shop`,
`path_prefix`, `timestamp`, `signature`, `logged_in_customer_id`) are not part
of the input; the verified shop and customer are in the context. A missing,
empty, or invalid JSON body is read as `null`, which fits only a type that
accepts it, such as an `Option`.

A function refuses an input the same way by returning `InvalidInput`:
`bail!(InvalidInput("rating is 1 to 5".into()))` is answered with `422` and
`{ "error": "Invalid input: rating is 1 to 5" }`.

`ctx.query`, `ctx.body`, and `ctx.params` stay available next to the input.

## Response types

A proxy function returns `Result<T>`, where `T` is anything that serializes.
The response type follows the path in the attribute.

| Attribute | Successful result | Content-Type |
| --- | --- | --- |
| `#[proxy(GET, "/path")]` or `#[proxy(GET, "/path", json)]` | The value serialized as JSON | `application/json` |
| `#[proxy(GET, "/path", html)]` | A string, sent as the body | `text/html` |
| `#[proxy(GET, "/path", liquid)]` | A `Liquid` template, sent as the body | `application/liquid` |

`html` or `liquid` falls back to JSON when the returned value does not
serialize to a string. The functions of one path share one response type; a
path declared with two is rejected when the configuration is built
(`[openshop] Invalid config: proxies./banner answers with both liquid and json; a path has one response type`).
Proxy functions return values, not a status/headers response object; the
current API always sends successful results with HTTP 200.

An `html` function can return the content of a file next to it:
`Ok(include_str!("page.html"))`.

## Liquid templates

Shopify renders the answer of a `liquid` proxy with the objects of the
storefront: `shop`, `customer`, `cart`, and so on. The template is a file next
to the function that answers with it, never a Rust string.

`src/proxy/banner.liquid`:

```liquid
<p>{{ shop.name }} via {{ app_shop }}</p>
```

`src/proxy/banner.rs`:

```rust,ignore
use openshop::prelude::*;

#[proxy(GET, "/banner", liquid)]
pub async fn banner(ctx: ProxyContext) -> Result<Liquid> {
    Ok(liquid!("banner.liquid").assign("app_shop", &ctx.shop))
}
```

`liquid!("banner.liquid")` embeds the file when the crate compiles and gives a
`Liquid`. The path is relative to the source file that names it, like
`include_str!`. A missing file is a compile error, the binary carries its
templates, and `openshop dev` restarts when one changes.

`.assign(name, value)` gives the template a variable, which it reads as
`{{ name }}`. The value is anything that serializes. Each variable is written
as a Liquid `assign` tag before the content of the file, in the order of the
calls. For `demo.myshopify.com`, the function above sends:

```liquid
{%- assign app_shop = "demo.myshopify.com" -%}
<p>{{ shop.name }} via {{ app_shop }}</p>
```

A value keeps its type where Liquid has one:

| Rust value | Liquid value |
| --- | --- |
| A string | A string. |
| A number | A number. |
| `true`, `false` | A boolean. |
| `None` | `nil`. |
| A list or an object | A string holding its JSON text, to print in a `<script type="application/json">` for the scripts of the page. |

A value is never pasted into the template. A string is written as a literal
that its text cannot close: `"`, `%`, `{`, and `}` each go in a literal of
their own, joined with `append`. What a customer typed is displayed and never
executed.

The name of a variable is made of letters, digits, `_`, and `-`, and starts
with a letter or `_`. Another name, or a value that does not serialize, is a
handler error: the request is answered with `500`.

`Liquid::new(source)` builds a template from a string, for a test for
example, and `.render()` returns the text that is sent.

## Context

| `ProxyContext` field | Type | Source |
| --- | --- | --- |
| `shop` | `String` | Verified `shop` query parameter or JWT `dest`. |
| `shopify_app` | `String` | Handle of the app whose secret or API key authenticated the request. |
| `customer_id` | `Option<String>` | Numeric signed `logged_in_customer_id`, customer JWT subject, or `None`. |
| `auth` | `ProxyAuthKind` | `AppProxyHmac` or `CustomerAccountJwt`. |
| `query` | `IndexMap<String, String>` | Query strings, with trusted shop/customer values substituted. The first value wins for a repeated key. |
| `params` | `IndexMap<String, String>` | Dynamic route parameters. |
| `headers` | `IndexMap<String, String>` | Request headers with lower-cased names. |
| `path`, `method` | `String` | Request pathname and method. |
| `body` | `Option<Value>` | Parsed JSON for non-GET requests, otherwise `None`. |
| `db` | `PgPool` | The database of the application. |
| `shopify` | `ShopifyClient` | The Admin API of the shop. |
| `connectors` | `Connectors` | The providers of the shop. |
| `app` | `OpenShop` | The running application. |

Nothing is read when the context is built: the access token of the shop is
read at the first Shopify request, and the provider configuration at the first
provider call.

`ProxyContext` belongs to a shop, so what needs one takes the context itself:
`Review::of(&ctx)` gives the rows of the shop, and
`sync_orders::dispatch(&ctx, SyncOrdersInput { limit: 10 }).await?` queues a
run of a flow for it.

## Authentication mounts

`/proxy/*` accepts either:

- a Shopify app proxy signature matched against exactly one configured app; or
- a Customer Account session JWT whose audience matches a configured API key.

`/ext/*` accepts only the Customer Account JWT. It exists so extensions can call
the app origin directly without Shopify CLI intercepting `/proxy`.

A request with an `Authorization: Bearer …` header is authenticated as a JWT,
even on `/proxy`; the query signature is then not considered.

The app proxy customer ID is trusted only when it is numeric. For JWTs,
the customer ID is taken from a subject of the form
`gid://shopify/Customer/<id>`; any other subject, including numeric Shopify
admin subjects, yields `None`.

## Shopify app proxy configuration

OpenShop does not add app proxy settings to Shopify. Configure the external
Shopify route in the app TOML, then deploy that configuration with Shopify CLI.
The exact TOML fields and public storefront URL are owned by Shopify; point the
proxy destination at your OpenShop `/proxy/...` route.

## Customer Account extension and CORS

Enable network access in the extension's TOML:

```toml
[extensions.capabilities]
network_access = true
```

Send the session token as `Authorization: Bearer ...`. CORS allows local
origins, Shopify admin, Shopify storefront domains, Shopify's extension CDN,
and configured `HOST` / `SHOPIFY_APP_URL` origins. Other origins receive no
`Access-Control-Allow-Origin` header. Preflight `OPTIONS` requests are answered
with `204`.

## Errors

Error bodies are JSON objects of the form `{ "error": "<message>" }`.

| Status | Message | Condition |
| --- | --- | --- |
| `401` | `Invalid proxy signature` | No bearer token, and the query signature matched zero or several configured apps. |
| `401` | `Invalid shop parameter` | Valid signature, but `shop` is not a `*.myshopify.com` domain. |
| `401` | `Unauthorized: missing token audience` | The bearer token has no readable `aud` claim. |
| `401` | `Unauthorized: invalid or expired session token` | The audience matches no single app, or the token fails verification. |
| `401` | `Unauthorized: missing session token` | `/ext` request without a bearer token. |
| `422` | `Invalid input: <reason>` | The request does not fit the [typed input](#typed-input) of the function. |
| `500` | `SHOPIFY_API_SECRET is not configured` | No Shopify secret is configured. |
| `500` | `Internal proxy error` | The handler returned an error or panicked. |

Authentication fails closed if zero or multiple configured apps match a
signature. Handler errors are logged without exposing their details.

## Without the attribute

`#[proxy]` is built on `Proxy`, which can be used directly for a
route that is only known at run time, or in a test:

```rust,ignore
use openshop::builder::Proxy;
use openshop::prelude::*;

pub fn reviews() -> Proxy {
    Proxy::json().get(|ctx| async move {
        Ok(json!({ "shop": ctx.shop, "customerId": ctx.customer_id, "query": ctx.query }))
    })
}
```

Register it with `.proxy("/reviews", reviews())` on
`OpenShopConfig::builder()`, or on `APP.builder()?` to add it to a declared
application.

`Proxy::json()`, `Proxy::html()`, and
`Proxy::liquid()` start a definition with its response type;
`Proxy::default()` answers with JSON. It has one method per
supported HTTP method: `.get(…)`, `.post(…)`, `.put(…)`, `.delete(…)`, and
`.patch(…)`. Each takes a handler that returns `Result<serde_json::Value>`; a
`Value::String` is the body of an HTML or Liquid response. A handler has no
typed input: it reads `ctx.query` and `ctx.body`.

Calling `.proxy(path, …)` again for a path adds methods to it. A method
registered twice, or a second response type, is rejected as for proxy
functions.
