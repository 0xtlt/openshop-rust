# Authentication

OpenShop uses a different authentication mechanism for each surface. Do not
forward one surface's credential to another.

| Surface | Credential | Verification | Trusted identity |
| --- | --- | --- | --- |
| Embedded admin API `/api/*` | Shopify session token in `Authorization: Bearer …` | HMAC JWT signature, audience, time claims, destination, and a staff subject | `shop`, `shopify_app`, staff user subject (`sub`) |
| Admin UI shell (production) | Signed launch query from Shopify admin | Shopify HMAC and an active installation | shop and app |
| OAuth start `/auth` | Shop domain and optional configured app handle | Shop normalization and configured app lookup | requested shop and app |
| OAuth callback `/auth/callback` | Signed Shopify query plus OAuth `state` and the cookie `/auth` set | Shopify HMAC, and a signed, unexpired, unspent `state` of the browser that started it | verified shop and app |
| `/webhooks` | `X-Shopify-Hmac-Sha256` | HMAC over the raw request body | shop and app selected by the matching secret |
| `/proxy/*` | Shopify App Proxy query signature, or Customer Account JWT | Query HMAC or JWT (never the session token of a staff user) | shop, app, optional customer |
| `/ext/*` | Customer Account session JWT | JWT signature and audience (never the session token of a staff user) | shop, app, customer |
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
| `Unauthorized: invalid session token` | `aud` matches no configured API key, or the token fails verification (signature, expiry, `dest`, `iss`, …). |
| `Unauthorized: session token was not issued to a staff user` | Empty or customer `sub`. |

The reason a token is refused is not sent to the client, which would learn from
it which claim to forge or which apps are configured. It is logged as a warning
(`[openshop] Session token refused`, with the reason and the path).

When the shop has no stored access token yet, the first authenticated request
gets one and stores it encrypted, by exchanging the session token for an
offline access token, which does not expire. When Shopify refuses the exchange,
or it cannot run, the request asks for a token with the
[client credentials grant](#client-credentials-grant), which only a development
store of the organization that owns the app obtains. Both are best-effort:
their failure does not fail the request.

One request at a time runs the exchange of a shop; the requests that arrive
while it is out wait for it, then use the token it stored, so a shop gets one
exchange, not one per request. Shopify is asked for five seconds at most, and a
failed exchange (a refusal, an error, a timeout) is not tried again for that
shop for thirty seconds. A shop never waits for the exchange of another. When
Shopify did not answer in time, the request is served at once, without asking
the client credentials grant, which would wait for the same Shopify. A token this process obtained with the
grant is replaced by the exchange when it is about to expire.

## OAuth installation

Single-app mode starts installation at:

```text
/auth?shop=example.myshopify.com
```

Multi-app mode also requires the configured handle:

```text
/auth?shop=example.myshopify.com&app=clientA
```

`/auth` redirects to Shopify's authorization page with the app's scopes and the
redirect URI `<app URL>/auth/callback`. It writes nothing, since anybody can
call it: the `state` it sends is `<expiry>.<nonce>.<signature>`, signed with
the secret of the app over the app handle, the shop, the expiry, the nonce and
the browser, and good for ten minutes. The browser is a random identifier in
the cookie `openshop_oauth` (`HttpOnly`, `SameSite=Lax`, `Path=/auth`,
`Max-Age=600`, and `Secure` when the application URL is `https`), which a
browser that already has one keeps. So a shop may have any number of
installations pending, a second `/auth` never invalidates the first, and a
`state` is of no use to a browser other than the one that started it.

The callback validates the HMAC, then the `state` (signature compared in
constant time, shop, app, expiry, cookie) and spends it: a `state` works once
per process, and Shopify's own code only once. It stores the access token,
then redirects to the app in Shopify admin. A callback whose token request
fails has spent its `state`: the merchant starts again at `/auth`. Access
tokens are encrypted when `ENCRYPTION_KEY` is configured.

The `installations.nonce` column is no longer written or read: a nonce an
earlier version, or the TypeScript runtime, left in a row is not accepted, and
the callback clears it. An installation started in one runtime is finished in
the same one.

| Endpoint | Status | `error` |
| --- | --- | --- |
| `/auth` | `500` | `[openshop] The Shopify app has no API secret` |
| `/auth` | `400` | `Missing shop parameter`, `Invalid shop parameter`, `[openshop] Missing Shopify app handle`, or `[openshop] Unknown Shopify app "<handle>"` |
| `/auth/callback` | `400` | `Missing required parameters` or `Invalid shop parameter` |
| `/auth/callback` | `401` | `Invalid HMAC` or `Invalid state/nonce` (missing cookie, expired, spent, altered, or another browser) |
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
| A launch from Shopify admin (production shell) | The `shop` of the query, whose `hmac` covers it, when its `timestamp` is within 90 seconds of the server's clock | Redirected to `/auth`, as before |
| An Admin API request (`/api`), once Shopify refused the token exchange, or it could not run | The `dest` of the verified session token, which must not have expired | The request is served without a token, as before |

A launch from Shopify admin for a development store with an empty database
then renders the app instead of redirecting to `/auth`, which Shopify refuses
to frame. A token obtained this way also brings back a shop marked
uninstalled, since Shopify vouched for it, and sets its `installed_at` as OAuth
does. A signed launch query older than 90 seconds, or one dated in the future
by more, is still accepted as before, from the stored token alone: it never
asks for a token and never brings a shop back, so that a launch URL replayed
later starts nothing.

The client of an admin handler (`/api` functions, custom admin pages, and
`testing::admin_context` in tests) carries the same right, for the shop and
the app of its session only: a clone of it whose `shop` or `shopify_app` was
changed may do what background work may, as any client of the application.

A token obtained at a launch lasts about 24 hours. The first Admin API request
of a session after it, in the same process, tries the token exchange once in
its place: the offline token it gives does not expire. When Shopify refuses
the exchange, the token of the launch is kept, and renewed as below.

A token is stored only where the installation row still holds what it held
when Shopify was asked: a shop uninstalled by its `app/uninstalled` webhook,
or given a token by OAuth or the token exchange, meanwhile keeps what it has,
and the caller uses that. A launch whose token cannot be stored (production
without `ENCRYPTION_KEY`, or a database that refuses the write) is sent to
`/auth`, as when Shopify refuses.

Shopify refuses the store of a merchant: at its first launch, that refusal
costs one request to Shopify, before the redirection to `/auth`, and is
logged as information, not as a warning; the process does not ask again for
that shop for an hour.

Unsigned input never starts a grant for a shop of its choosing: not a webhook
(its signature covers the body, not the `X-Shopify-Shop-Domain` header, and a
delivery can be replayed), not `/auth?shop=`, not a route that calls `for_shop`
with a shop of its request, not a flow, a cron, a proxy or the MCP
`openshop.admin.graphql` tool. That background work, the `shopify` client of a
webhook handler included, uses the stored token, and may ask Shopify in two
cases only:

- For the shop of the `client_credentials_shop` setting of
  [`app!`](configuration.md), with or without an installation row. The
  templates of `openshop init` read it from the environment:

  ```rust,ignore
  client_credentials_shop: std::env::var("SHOPIFY_CLIENT_CREDENTIALS_SHOP").ok(),
  ```

  ```dotenv
  SHOPIFY_CLIENT_CREDENTIALS_SHOP=dev-store-1.myshopify.com
  ```

  For that one shop, a flow (one a webhook dispatches included), a webhook
  handler, a cron, a route's `for_shop`, a proxy handler or the MCP
  `openshop.admin.graphql` tool asks for a token when the app has none, for example after the database was
  emptied, and a cron of every shop (`.all_shops()`) targets it even without an
  installation row, once Shopify gives a token for it. It never brings back a
  shop marked uninstalled.
- To renew the token an installation row holds, for any shop: once after the
  Admin API answers `401`, or before a token this process obtained expires.
  The row proves that the shop installed the app earlier through a signed flow
  (OAuth, the token exchange or a vouched grant), so a development store that
  got its token from a launch keeps working in crons, flows and webhook
  handlers after that token expires. Only the token of a row that holds one
  and is not marked uninstalled is replaced; no row is created this way.

Without the setting, only a signed launch, a session token whose exchange
Shopify refused or the renewal of a stored token asks.

A token is used as it is stored, whoever stored it:

- A token this process obtained with the grant is asked again five minutes
  before it expires, from the `expires_in` Shopify gave with it. That is kept
  in memory only, until the token expires: another process, or the same one
  after a restart, uses the stored token as it is until the Admin API refuses
  it.
- When the Admin API answers `401` to a client that may ask, it asks once and
  sends the request again once. When Shopify refuses, or gives no other
  token, the `401` is the answer.
- A token without `expires_in` is only asked again after a `401`. A token
  without `scope` keeps the scopes stored before it.
- One request at a time asks Shopify for an app and shop in a process; the
  others wait for it, then use the token it stored. The token request has its
  own 5-second timeout, whatever the transport allows, so that an admin page
  waiting for it is not held for long.
- A failure is remembered by the process for that app and shop, and every
  caller, the ones that waited included, falls back at once to what it does
  without the grant meanwhile:
  - a definite refusal, a `4xx` other than `408` and `429` carrying Shopify's
    error code (`shop_not_permitted`, `invalid_client`, `invalid_request`…),
    for one hour;
  - anything else (a `5xx`, a `429`, a timeout, a connection error, a
    redirection, an answer without a token, a token the database would not
    store), for 30 seconds.

  The failure of background work holds back background work only: a launch
  or an Admin API request that Shopify vouched for still asks, so that a
  store where the app was installed since is served at once. The failure of
  a vouched request holds back both. A success clears both. A corrected
  client secret takes a restart, which also forgets the failures. The process
  only keeps something of an app and shop it asked Shopify for, while a
  failure holds back or the token it obtained has not expired: nothing for a
  shop a caller may not ask a token of.

  It is logged once, with the HTTP status and Shopify's error code, never the
  client ID, the secret or the token:

```text
[openshop] Shopify refused the client credentials of app "default" for dev-store-1.myshopify.com (400: shop_not_permitted)
```

In production without `ENCRYPTION_KEY`, nothing is asked: the caller fails
with `ENCRYPTION_KEY must be set in production`, which is not remembered as a
refusal, and no token is ever stored in plaintext. When the token cannot be
written, the error is
`[openshop] Could not store the client credentials token of app "<app>" for <shop>`,
which quotes neither the token, its ciphertext nor the database's message; it
holds back the next requests for 30 seconds, as a failure that may pass, so
that callers do not each get a token they cannot keep.

The shop the token goes to is always `<name>.myshopify.com`, read without a
scheme, a path, a port or a user: a value such as
`evil.example://dev-store-1.myshopify.com` or `dev-store-1.myshopify.com:8443`
given to a `ShopifyClient` fails with `[openshop] Invalid shop domain` and
sends nothing, and a webhook whose `X-Shopify-Shop-Domain` is not a shop domain
is answered `400`, like one that is a bare `<name>`.

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
[client credentials grant](#client-credentials-grant) when its `timestamp` is
within 90 seconds, and is redirected to `/auth` when Shopify refuses it or the
launch is older.

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

OpenShop allows known Shopify origins, configured app origins,
`https://extensions.shopifycdn.com`, and in development (`OPENSHOP_ENV` other
than `production`) local origins. Arbitrary origins are not reflected in CORS
responses. In production a page on `localhost` is never allowed: it runs on the
visitor's own machine, not on an origin of the application, unless `HOST` or
`SHOPIFY_APP_URL` is itself that address.

| Allowed origin | Example |
| --- | --- |
| Local development hosts, any port, outside production | `http://localhost:3000`, `https://app.localhost`, `http://127.0.0.1:8080`, `http://[::1]:3000` |
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
