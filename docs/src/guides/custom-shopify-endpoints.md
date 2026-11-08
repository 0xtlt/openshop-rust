# Custom Shopify endpoints: simulators, proxies, custom APIs

By default, an OpenShop app talks to Shopify. The Admin API and OAuth of each
shop are at `https://{shop}`, and the admin UI loads App Bridge and Polaris
from Shopify's CDN. `ShopifyEndpoints` moves any of them somewhere else:

- a simulator of Shopify, on your machine or in CI, such as
  [local-storefront](https://github.com/0xtlt/local-storefront);
- a proxy in front of the Admin API;
- an API of your own that answers as Shopify's does;
- your own copy of the App Bridge or Polaris script.

Each endpoint is independent: set one, two or all three. When none is set,
nothing changes.

## The endpoints

| Field | In place of | What moves |
| --- | --- | --- |
| `admin_origin` | `https://{shop}` | The GraphQL Admin API, the token exchange, the code exchange, the client credentials grant and the refresh of every shop. Also the install link that `/auth` sends the browser to, and the redirect at the end of `/auth/callback`, which goes to `{admin_origin}/admin/apps/{client_id}?shop={shop}`. The admin UI may also be framed by this origin (`frame-ancestors`), since a simulator serves its admin there |
| `app_bridge` | `https://cdn.shopify.com/shopifycloud/app-bridge.js` | The App Bridge script of the admin UI. The tag keeps its `data-api-key`, and the page keeps its `shopify-api-key` meta |
| `polaris` | `https://cdn.shopify.com/shopifycloud/polaris.js` | The Polaris script of the admin UI, and of the page that tells a visitor where the app opens from. That page's policy allows the script's origin in `script-src`, `style-src`, `font-src` and `connect-src`, where a copy of Polaris loads its styles, font and icons from |

The shop is still the shop. Session tokens are still checked against
`https://{shop}`, and installations are still stored under the shop's domain.
One origin can serve every shop, so each request sent to `admin_origin` names
its shop in `X-Forwarded-Host`, and the install link in its `shop` parameter.

`admin_origin` is `https://host[:port]`, or `http://` to `localhost`,
`127.0.0.1` or `[::1]`, with no path, query string, fragment or user. A
trailing `/` is dropped. A script is an absolute `https://` URL, or `http://`
to the same three hosts, with no user or fragment. Any other value stops the
application at startup, so that an access token never travels in clear text
off the machine. Prefer `localhost` to `[::1]`: the origins go into the
`Content-Security-Policy` of the admin UI, which has no syntax for an IPv6
address, so a browser would ignore them there. The error names the setting or
the variable, never the value:

```text
[openshop] Invalid config: shopify_endpoints.admin_origin is not valid: invalid admin origin, expected `https://host[:port]` (or `http://` to localhost): it has a path: give the scheme, the host and the port only
```

## Set them

The endpoints come from three places, the first one that sets a field wins,
field by field:

1. **At startup**, on the builder of the declaration, for what `app!` cannot
   know: a simulator in CI, a proxy in one environment. `APP.main_with` hands
   the builder to a closure once `.env` is loaded:

   ```rust,ignore
   fn main() -> Result<()> {
       let simulator = std::env::args().any(|arg| arg == "--simulator");
       APP.main_with(|app| {
           if simulator {
               app.shopify_endpoints(ShopifyEndpoints::new().admin_origin("http://localhost:9393"))
           } else {
               app
           }
       })
   }
   ```

   `APP.run_with(…)` does the same in a binary that starts its own runtime.

2. **In the declaration**, for what is always true of the app:

   ```rust,ignore
   static APP: App = app! {
       flows: [sync_orders],
       shopify_endpoints: ShopifyEndpoints::new().admin_origin("https://shopify-proxy.example.com"),
   };
   ```

   `shopify_endpoints` is a setting of its own, not a field of `shopify`. It
   does not change which Shopify apps are served: an app with no `shopify`
   setting still takes its credentials from `SHOPIFY_API_KEY` and
   `SHOPIFY_API_SECRET`, and an app with several apps sends all of them to the
   same endpoints.

3. **In the environment**, for a field the code does not set:

   | Variable | Field |
   | --- | --- |
   | `SHOPIFY_ADMIN_ORIGIN` | `admin_origin` |
   | `SHOPIFY_APP_BRIDGE_URL` | `app_bridge` |
   | `SHOPIFY_POLARIS_URL` | `polaris` |

A field nobody sets is Shopify's. `.shopify_endpoints(…)` merges with the
calls before it: a field it sets replaces the earlier one, a field it leaves
unset keeps it. With the declaration above and
`SHOPIFY_POLARIS_URL=https://assets.example.com/polaris.js`, a startup call that
sets `app_bridge` gives the proxy's `admin_origin`, its own `app_bridge`, and
the variable's `polaris`.

When an endpoint is not Shopify's, the process logs one line at startup, with
where each one comes from:

```text
[openshop] Shopify endpoints replaced: Admin API and OAuth at http://localhost:9393 (shopify_endpoints.admin_origin); App Bridge from http://localhost:9393/__lsf/admin/app-bridge.js (shopify_endpoints.app_bridge); Polaris from http://localhost:9393/__lsf/cdn/shopifycloud/polaris.js (SHOPIFY_POLARIS_URL)
```

`dev`, `start` and `worker` all use the endpoints. Give every process the same
ones: a worker must reach the same Admin API as the web process.
`app.shopify_endpoints()` returns the endpoints a running application uses.

## Example: local-storefront

[local-storefront](https://github.com/0xtlt/local-storefront) (lsf) is a local
simulator of Shopify. It serves the GraphQL Admin API, OAuth and the token
exchange, delivers webhooks signed the way Shopify signs them, and has an admin
that frames embedded apps with its own App Bridge. With it, you need no
development store and no tunnel, and every run starts from the same data.

### 1. Start lsf for the app

Run lsf from the project root, where `shopify.app.toml` is:

```bash
lsf app dev --app . --app-url http://localhost:3000 --port 9393
```

lsf reads the app's `client_id`, `scopes` and webhook subscriptions from
`shopify.app.toml`. It signs session tokens, OAuth answers and webhooks with the
`SHOPIFY_API_SECRET` of `.env`. Its `client secret:` start line names the secret
it uses, and OpenShop must verify with the same one. A webhook subscription
whose `uri` is a path (`/webhooks`) goes to `--app-url`.

### 2. Point the app at lsf

The three endpoints of lsf, at its port:

```rust,ignore
fn lsf(port: u16) -> ShopifyEndpoints {
    ShopifyEndpoints::new()
        .admin_origin(format!("http://localhost:{port}"))
        .app_bridge(format!("http://localhost:{port}/__lsf/admin/app-bridge.js"))
        .polaris(format!("http://localhost:{port}/__lsf/cdn/shopifycloud/polaris.js"))
}

fn main() -> Result<()> {
    let lsf_port = std::env::args().find_map(|arg| arg.strip_prefix("--lsf=")?.parse().ok());
    APP.main_with(|app| match lsf_port {
        Some(port) => app.shopify_endpoints(lsf(port)),
        None => app,
    })
}
```

Then `cargo run -- dev --lsf=9393`, or `openshop dev` with the endpoints in
`.env` instead:

```bash
SHOPIFY_ADMIN_ORIGIN=http://localhost:9393
SHOPIFY_APP_BRIDGE_URL=http://localhost:9393/__lsf/admin/app-bridge.js
SHOPIFY_POLARIS_URL=http://localhost:9393/__lsf/cdn/shopifycloud/polaris.js
```

lsf's App Bridge only talks to lsf's admin, and Shopify's only to Shopify's: in
lsf's admin, Shopify's would never resolve `shopify.idToken()`, and the admin
UI would get no session token. That is why `app_bridge` replaces the script
instead of adding one.

`/__lsf/cdn/shopifycloud/polaris.js` redirects to Shopify's CDN by default. With
`lsf app dev … --polaris offline`, lsf serves its own copy of Polaris, with its
icons and fonts, and the admin UI renders without the internet.

### 3. Open the app in lsf's admin

Open `http://localhost:9393/admin/apps/<handle>`, where `<handle>` is the
`handle` of `shopify.app.toml` or its `client_id`. lsf's admin signs the launch
the way Shopify's admin does, and its App Bridge gives the admin UI session
tokens. The first request of the admin UI exchanges a session token for an
access token at lsf. After that, flows, crons and webhook handlers use the Admin
API of lsf's shop, `local-supply-co.myshopify.com` for its demo store.

Open the admin on the same host name as `admin_origin` (`localhost` here). lsf's
App Bridge takes the admin's origin from its own address, and `localhost` and
`127.0.0.1` are two different origins.

### Webhooks

lsf delivers the subscriptions of `shopify.app.toml` to `--app-url`, signed with
the client secret. `POST /webhooks` verifies them the way it verifies Shopify's.
The templates subscribe to no topic: add a `[[webhooks.subscriptions]]` with
`uri = "/webhooks"` for each topic the app handles
([Handle webhooks](handle-webhooks.md)), and restart lsf. To send one on
demand:

```bash
curl -X POST http://localhost:9393/__lsf/webhooks/trigger \
  -H 'Content-Type: application/json' -d '{"topic": "products/create"}'
```

lsf refuses a topic the app does not subscribe to. To send one anyway, give
where it goes: `{"topic": "products/create", "address": "http://localhost:3000/webhooks"}`.

`GET http://localhost:9393/__lsf/webhooks` lists the deliveries and what the app
answered.

## Tests

In a `TestContext`, the
[`shopify_endpoints`](../reference/testing.md#testoptions) of `TestOptions`
set the endpoints, field by field over the ones of the configuration. The
requests still go to the mock transport, at the addresses of the endpoints, and
the environment variables are not read.

An in-memory simulator needs no network at all: implement `shopify_api`'s
`Transport` and give it to
[`OpenShop::with_transport`](../reference/sdk.md#openshop). Every exchange with
Shopify goes through it, at the addresses the endpoints give, as
`MockTransport` does in tests.
