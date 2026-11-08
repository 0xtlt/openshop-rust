# Run against local-storefront

[local-storefront](https://github.com/0xtlt/local-storefront) (lsf) is a local
fake of Shopify. It serves the GraphQL Admin API (2026-10), OAuth and the token
exchange, delivers webhooks signed the way Shopify signs them, and has an admin
that frames embedded apps with its own App Bridge. With `LSF_URL` set, an
OpenShop app talks to lsf instead of Shopify. You need no development store and
no tunnel, and every run starts from the same data.

`LSF_URL` is for development and tests only. When it is unset, nothing changes.

## 1. Start local-storefront for the app

Run lsf from the project root, where `shopify.app.toml` is:

```bash
lsf app dev --app . --app-url http://localhost:3000
```

lsf reads the app's `client_id`, `scopes` and webhook subscriptions from
`shopify.app.toml`. It signs session tokens, OAuth answers and webhooks with the
`SHOPIFY_API_SECRET` of `.env`. Its `client secret:` start line names the secret
it uses, and OpenShop must verify with the same one. A webhook subscription
whose `uri` is a path (`/webhooks`) goes to `--app-url`. lsf listens on
`http://127.0.0.1:9292` by default (`--port` to change it).

## 2. Start the app with `LSF_URL`

```bash
LSF_URL=http://localhost:9292 openshop dev
```

You can also set `LSF_URL=http://localhost:9292` in `.env`. The process logs a
warning at startup that names lsf's address. Then:

| What | Without `LSF_URL` | With `LSF_URL=http://localhost:9292` |
| --- | --- | --- |
| The GraphQL Admin API of every shop | `https://{shop}/admin/api/…` | `http://localhost:9292/admin/api/…` |
| Token exchange, OAuth code exchange, client credentials grant, refresh | `https://{shop}/admin/oauth/access_token` | `http://localhost:9292/admin/oauth/access_token` |
| Where `/auth` sends the browser | `https://{shop}/admin/oauth/authorize?…` | `http://localhost:9292/admin/oauth/authorize?…&shop={shop}` |
| Where `/auth/callback` sends the browser | `https://{shop}/admin/apps/{client_id}` | `http://localhost:9292/admin/apps/{client_id}?shop={shop}` |
| App Bridge of the admin UI | Shopify's CDN | lsf's `/__lsf/admin/app-bridge.js`, loaded right before Shopify's tag |
| Polaris of the admin UI and of the page that says where the app opens from | `https://cdn.shopify.com/shopifycloud/polaris.js` | `http://localhost:9292/__lsf/cdn/shopifycloud/polaris.js` |
| `frame-ancestors` of the admin UI | Shopify admin, and `http://localhost:*` with `dev` | The same, plus lsf's origin |

The shop is still the shop: `local-supply-co.myshopify.com` for lsf's demo
store. Session tokens are still checked against `https://{shop}`, and
installations are still stored under the shop's domain. lsf serves every shop on
one origin, so each request OpenShop sends there names its shop in
`X-Forwarded-Host`.

Shopify's App Bridge tag keeps its place and its `data-api-key`. In lsf's admin,
it returns as soon as it loads, because lsf's App Bridge is already there.
Without lsf's App Bridge, Shopify's would wait for an admin that never answers:
`shopify.idToken()` would never resolve, and the admin UI would get no session
token.

## 3. Open the app in lsf's admin

Open `http://localhost:9292/admin/apps/<handle>`, where `<handle>` is the
`handle` of `shopify.app.toml` or its `client_id`. lsf's admin signs the launch
the way Shopify's admin does. lsf's App Bridge gives the admin UI session
tokens. The first request of the admin UI exchanges a session token for an
access token at lsf. After that, flows, crons and webhook handlers use the Admin
API of lsf's shop.

Open the admin on the same host name as `LSF_URL` (`localhost` here). lsf's App
Bridge takes the admin's origin from its own address, and `localhost` and
`127.0.0.1` are two different origins.

lsf redirects Polaris to Shopify's CDN. When lsf runs offline, it serves its own
copy of Polaris instead.

## Webhooks

lsf delivers the subscriptions of `shopify.app.toml` to `--app-url`, signed with
the client secret. `POST /webhooks` verifies them the way it verifies Shopify's.
To send one on demand:

```bash
curl -X POST http://localhost:9292/__lsf/webhooks/trigger \
  -H 'Content-Type: application/json' -d '{"topic": "products/create"}'
```

`GET http://localhost:9292/__lsf/webhooks` lists the deliveries and what the app
answered.

## `start`, `worker` and tests

`dev`, `start` and `worker` all read `LSF_URL`. A backend test can therefore run
the production commands against lsf, and a worker reaches the same Admin API as
the web process. Set the variable on each process. In a `TestContext`,
[`TestOptions::lsf_url`](../reference/testing.md#testoptions) does the same,
with the requests sent to the mock transport.

`LSF_URL` must be an `https://` URL, or an `http://` URL to `localhost`,
`127.0.0.1` or `[::1]`, with no path, query string or user, so that an access
token never travels in clear text off the machine. Any other value stops the
process at startup. Never set `LSF_URL` on a deployed app: it would send the
Admin API of every shop to lsf.
