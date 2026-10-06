# Proxy routes

To add a storefront endpoint, see [Add proxy routes](../guides/add-proxy-routes.md).

A proxy route is a `ProxyDefinition` registered under a path with `.proxy(…)`
on the configuration builder.

```rust,ignore
use openshop::{ProxyDefinition, json};

pub fn reviews() -> ProxyDefinition {
    ProxyDefinition::json().get(|ctx| async move {
        Ok(json!({ "shop": ctx.shop, "customerId": ctx.customer_id, "query": ctx.query }))
    })
}
```

```rust,ignore
use openshop::{OpenShopConfig, Result};

fn config() -> Result<OpenShopConfig> {
    OpenShopConfig::builder()
        .proxy("/reviews", proxy::reviews::reviews())
        .build()
}
```

## Routing

| Registered path | Mounted paths |
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

`ProxyDefinition` has one builder method per supported HTTP method: `.get(…)`,
`.post(…)`, `.put(…)`, `.delete(…)`, and `.patch(…)`. Only declared handlers
are registered; a request with another method is answered with a plain
`404 Not Found`.

For non-GET methods, OpenShop reads the body and attempts to parse it as JSON.
An empty or invalid JSON body becomes `None`; it is not an automatic 400.
GET bodies are not read. A body larger than the
[request body limit](configuration.md#builder-methods) is answered with `413`.

## Response types

A handler returns `Result<serde_json::Value>`.

| Constructor | Successful handler result | Content-Type |
| --- | --- | --- |
| `ProxyDefinition::json()` or `ProxyDefinition::default()` | The value serialized as JSON | `application/json` |
| `ProxyDefinition::html()` | `Value::String` | `text/html` |
| `ProxyDefinition::liquid()` | `Value::String` | `application/liquid` |

`html` or `liquid` falls back to JSON when the returned value is not a string.
Handlers return values, not a status/headers response object; the current API
always sends successful handler results with HTTP 200.

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
| `app` | `OpenShop` | The running application, for dispatch, database, and connector access. |

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
| `500` | `SHOPIFY_API_SECRET is not configured` | No Shopify secret is configured. |
| `500` | `Internal proxy error` | The handler returned an error or panicked. |

Authentication fails closed if zero or multiple configured apps match a
signature. Handler errors are logged without exposing their details.
