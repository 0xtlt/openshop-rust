# Authentication

OpenShop uses a different authentication mechanism for each surface. Do not
forward one surface's credential to another.

| Surface | Credential | Verification | Trusted identity |
| --- | --- | --- | --- |
| Embedded admin API `/api/*` | Shopify session token in `Authorization: Bearer …` | HMAC JWT signature, audience, time claims, destination, and a staff subject | `shop`, `shopify_app`, staff user subject (`sub`) |
| Admin UI shell (production) | Signed launch query from Shopify admin | Shopify HMAC and an active installation | shop and app |
| OAuth start `/auth` | Shop domain and optional configured app handle | Shop normalization and configured app lookup | requested shop and app |
| OAuth callback `/auth/callback` | Signed Shopify query plus OAuth `state` | Shopify HMAC and stored nonce | verified shop and app |
| `/webhooks` | `X-Shopify-Hmac-Sha256` | HMAC over the raw request body | shop and app selected by the matching secret |
| `/proxy/*` | Shopify App Proxy query signature, or Customer Account JWT | Query HMAC or JWT | shop, app, optional customer |
| `/ext/*` | Customer Account session JWT | JWT signature and audience | shop, app, customer |
| `/routes/*` | Route-defined | Authentication function named by `auth = …` in `#[route]`, or intentionally public without it | Route-defined; optional `for_shop()` context |
| `/mcp` | OpenShop token in `Authorization: Bearer …` | Stored token hash, status, expiry, shop, and permissions | token, shop, app, grants |
| `/ops/*` | Session cookie obtained with the password of the application | Signature and expiry of the cookie | The operator; no shop |

`/health` and `/robots.txt` are public. `/ops` exists only when the
application sets `ops_password`; see
[Operator dashboard](operator-dashboard.md#session) for its session.

## Embedded admin API

Send the Shopify session token exactly as a bearer token:

```http
Authorization: Bearer <shopify-session-token>
```

OpenShop rejects missing, invalid, expired, or wrong-audience tokens with
`401`. API handlers read the verified shop and app identity from the token;
client-supplied `shop` query parameters do not replace that identity.

A session token is accepted when:

- it is an HS256 JWT signed with the secret of the app named by its `aud`
  claim;
- `exp` is in the future and `nbf` is at most 60 seconds ahead;
- `dest` and `iss` are the same `*.myshopify.com` shop;
- `sub` identifies a staff user. Tokens issued to checkout or customer account
  extensions, whose `sub` is empty or a customer
  (`gid://shopify/Customer/…`), are rejected even though they are signed with
  the same secret.

| `error` | Condition |
| --- | --- |
| `Unauthorized: missing session token` | No `Authorization: Bearer …` header. |
| `Unauthorized: missing token audience` | The token has no readable `aud` claim. |
| `Unauthorized: [openshop] No Shopify app matches the session token audience` | `aud` matches no configured API key. |
| `Unauthorized: Invalid JWT signature`, `JWT expired`, `JWT not yet valid`, `JWT dest/iss mismatch`, … | The token fails verification. |
| `Unauthorized: session token was not issued to a staff user` | Empty or customer `sub`. |

When the shop has no stored access token yet, the first authenticated request
gets one and stores it encrypted: first with the
[client credentials grant](#client-credentials-grant), which only a development
store of the organization that owns the app obtains, then, when Shopify refuses
it, by exchanging the session token for an offline access token. Both are
best-effort: their failure does not fail the request.

## OAuth installation

Single-app mode starts installation at:

```text
/auth?shop=example.myshopify.com
```

Multi-app mode also requires the configured handle:

```text
/auth?shop=example.myshopify.com&app=clientA
```

`/auth` stores a one-time nonce and redirects to Shopify's authorization page
with the app's scopes and the redirect URI `<app URL>/auth/callback`. The
callback validates the HMAC and the one-time state before storing the access
token, then redirects to the app in Shopify admin. Access tokens are encrypted
when `ENCRYPTION_KEY` is configured.

| Endpoint | Status | `error` |
| --- | --- | --- |
| `/auth` | `400` | `Missing shop parameter`, `Invalid shop parameter`, `[openshop] Missing Shopify app handle`, or `[openshop] Unknown Shopify app "<handle>"` |
| `/auth/callback` | `400` | `Missing required parameters` or `Invalid shop parameter` |
| `/auth/callback` | `401` | `Invalid HMAC` or `Invalid state/nonce` |
| `/auth/callback` | `500` | `Token exchange failed: <Shopify response>` |

Only `*.myshopify.com` domains are accepted as shops, everywhere a shop is
read from a request.

## Client credentials grant

A development store of the Shopify organization that owns the app can get an
access token without OAuth, a browser or a merchant, with Shopify's
[client credentials grant](https://shopify.dev/docs/apps/build/authentication-authorization/client-credentials-grant):
OpenShop posts the API key and secret of the resolved app (the app of the
handle in multi-app mode) to `https://<shop>/admin/oauth/access_token` with
`grant_type=client_credentials`, as a form, and stores the token it receives
encrypted in `installations`, like any other. Shopify only grants it when the
app and the store belong to the same organization, and refuses anything else,
for example with `shop_not_permitted`. A merchant's store keeps installing
through OAuth or the token exchange.

OpenShop only asks for a shop Shopify vouched for, and only when the app has
no usable token for it (no row, no token, or a row marked uninstalled):

| Where | Shop | When Shopify refuses |
| --- | --- | --- |
| A launch from Shopify admin (production shell) | The `shop` of the query, whose `hmac` covers it | Redirected to `/auth`, as before |
| An Admin API request (`/api`) | The `dest` of the verified session token | The session token is exchanged, as before |

A launch from Shopify admin for a development store with an empty database
then renders the app instead of redirecting to `/auth`, which Shopify refuses
to frame. A token obtained this way also brings back a shop marked
uninstalled, since Shopify vouched for it.

Nothing else asks for an arbitrary shop: not a webhook (its signature covers
the body, not the `X-Shopify-Shop-Domain` header, and a delivery can be
replayed), not `/auth?shop=`, not a route that calls `for_shop` with a shop of
its request, not a flow, a cron or the MCP `openshop.admin.graphql` tool. They
use the stored token, or, for work no request vouches for, the shop of
`SHOPIFY_CLIENT_CREDENTIALS_SHOP`:

```dotenv
SHOPIFY_CLIENT_CREDENTIALS_SHOP=dev-store-1.myshopify.com
```

For that one shop, a flow, a cron, a route's `for_shop`, a proxy handler, an
admin handler or the MCP `openshop.admin.graphql` tool asks for a token when
the app has none, for example after the database was emptied, and a cron of
every shop (`.all_shops()`) targets it even without an installation row, once
Shopify gives a token for it. It never brings back a shop marked uninstalled,
and a webhook does not ask for it either. Without the variable, only a signed
launch or a session token asks.

A token is used as it is stored, whoever stored it:

- A token this process obtained with the grant is asked again five minutes
  before it expires, from the `expires_in` Shopify gave with it. That is kept
  in memory only: another process, or the same one after a restart, uses the
  stored token as it is.
- When the Admin API answers `401` to a client that may ask for the shop (the
  shop of the variable, or the shop of the session token of an admin
  handler), it asks once and sends the request again once. When Shopify
  refuses, or gives no other token, the `401` is the answer.
- A token without `expires_in` is only asked again after a `401`. A token
  without `scope` keeps the scopes stored before it.
- One request at a time asks Shopify for an app and shop in a process; the
  others wait for it, then use the token it stored.
- A refusal, an answer that is not `2xx` or a request that fails, is
  remembered by the process for one hour for that app and shop: nothing asks
  again meanwhile, and every caller, the ones that waited included, falls back
  at once to what it does without the grant. It is logged once, with the HTTP
  status and Shopify's error code, never the client ID, the secret or the
  token:

```text
[openshop] Shopify refused the client credentials of app "default" for dev-store-1.myshopify.com (400: shop_not_permitted)
```

The shop the token goes to is always `<name>.myshopify.com`, read without a
scheme, a path, a port or a user: a value such as
`evil.example://dev-store-1.myshopify.com` or `dev-store-1.myshopify.com:8443`
given to a `ShopifyClient` fails with `[openshop] Invalid shop domain` and
sends nothing, and a webhook whose `X-Shopify-Shop-Domain` is not a shop domain
is answered `400`.

`POST /auth/webhooks/app-uninstalled` is authenticated like a webhook. It marks
the installation as uninstalled and deletes its access token. See
[Webhooks](webhooks.md#http-endpoint-and-context).

## Admin UI shell

In production (`start`), the page that hosts the admin UI is only served to a
launch from Shopify admin: the request must carry a `shop` parameter and a
query `hmac` that matches exactly one configured app. Otherwise the server
answers `401` with a page that says the app opens from Shopify admin, and
shows nothing of the application. A valid launch for a shop without an
active installation first asks Shopify for a token with the
[client credentials grant](#client-credentials-grant), and is redirected to
`/auth` when Shopify refuses it.

In development (`dev`), the signature is not checked. A request is still
refused with `403`, and the same page, unless it comes from a local host, is
loaded in an iframe, has a Shopify admin or shop referrer, or carries a `shop`
parameter. A local visitor without a Shopify session receives the admin UI,
which shows that page too: the Admin API answers nothing without a session
token.

The page is written with Shopify's Polaris web components, loaded from
Shopify's CDN, and sent with `Cache-Control: no-store` and a
`Content-Security-Policy` that allows Polaris and nothing else.

## Webhook and proxy bodies

Webhook HMAC validation uses the raw body. Reverse proxies and middleware must
not rewrite it before OpenShop receives the request. App Proxy signatures cover
the signed query parameters. In proxy handlers, trust `ctx.shop`,
`ctx.shopify_app`, and `ctx.customer_id`; do not replace them with values you
parse yourself. A secret that is empty or only whitespace never verifies a
signature.

## Browser origins

OpenShop allows known Shopify origins, configured app origins, local development
origins, and `https://extensions.shopifycdn.com`. Arbitrary origins are not
reflected in CORS responses.

| Allowed origin | Example |
| --- | --- |
| Local development hosts, any port | `http://localhost:3000`, `https://app.localhost`, `http://127.0.0.1:8080`, `http://[::1]:3000` |
| Shopify admin and Shopify | `https://admin.shopify.com`, `https://shopify.com` |
| Shop domains | `https://example.myshopify.com` |
| Shopify's extension CDN | `https://extensions.shopifycdn.com` |
| Origins of `HOST` and `SHOPIFY_APP_URL` | `https://your-app.example.com` |

CORS applies to `/proxy/*` and `/ext/*` always, and to `/api/*` in `dev` and
when the server runs without an admin UI. In production the admin UI and the
API share an origin, so `/api/*` sends no CORS headers.

See [Security](security.md) for secret handling and deployment
requirements.

## App selection

| Request | App resolution |
| --- | --- |
| Manual OAuth start | `?app=<handle>`; optional when exactly one app exists |
| Signed admin launch or OAuth callback | Query HMAC matches exactly one app secret |
| App proxy | Query signature matches exactly one app secret |
| Webhook | Raw-body HMAC matches exactly one app secret |
| App Bridge or Customer Account JWT | Audience matches exactly one API key, then signature verification |

Zero or ambiguous matches fail authentication. Configured API keys must be
unique, and each app should use its own secret. See
[Configure Shopify apps](../guides/configure-shopify-apps.md) for setup and
[Shop and app isolation](../concepts/shop-isolation.md) for the data boundary.
