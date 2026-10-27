# Security

## Required production controls

1. Set `OPENSHOP_ENV=production` and a random 64-character hexadecimal
   `ENCRYPTION_KEY`, and keep the key stable. Without a key, the application
   refuses to start unless the environment is explicitly a development one.
2. Terminate HTTPS before OpenShop and set `HOST` to the public HTTPS origin.
3. Use a dedicated PostgreSQL role with access only to the OpenShop database.
4. Run committed migrations before web and workers; do not grant runtime
   processes schema-generation privileges.
5. Restrict logs, backups, and admin access because they may contain merchant
   and operational data.
6. If you serve the [operator dashboard](operator-dashboard.md), give it a
   long random password, kept with your other secrets. Production refuses
   one shorter than 16 characters.

Generate an encryption key once:

```bash
openssl rand -hex 32
```

## Encryption at rest

Provider configurations and Shopify access tokens are encrypted with
AES-256-GCM when the key is present. Each value gets a random 12-byte IV and a
16-byte authentication tag.

| Value | Stored form |
| --- | --- |
| Provider configuration (`provider_configs.config`) | `{ "__encrypted": "aes-256-gcm", "payload": "<iv>:<tag>:<ciphertext>" }`, hex-encoded |
| Access token (`installations.access_token`) | `enc:aes-256-gcm:<iv>:<tag>:<ciphertext>`, hex-encoded |

Values written without a key stay readable: a configuration that is not an
encrypted envelope and a token without the `enc:aes-256-gcm:` prefix are
returned as stored.

| Situation | Behavior |
| --- | --- |
| No key, development (`openshop dev`, or `OPENSHOP_ENV=development` or `test`) | Values are stored in plaintext. A warning is logged once: `[openshop] ENCRYPTION_KEY not set — provider credentials stored in plaintext`. |
| No key, anything else (production, `staging`, a typo, `OPENSHOP_ENV` unset) | The application does not start: `ENCRYPTION_KEY must be set (64 hex characters, generate one with \`openssl rand -hex 32\`); to store secrets in plaintext on a development machine, run \`openshop dev\` or set OPENSHOP_ENV=development`. |
| Key that is not 64 hex characters | Writes, and reads of encrypted values, fail with `ENCRYPTION_KEY must be 64 hex characters (32 bytes)`. |
| Encrypted value, no key, development | Reads fail with `Cannot decrypt provider config: ENCRYPTION_KEY not set` or `Cannot decrypt secret: ENCRYPTION_KEY not set`. |
| Wrong key or corrupted value | Reads fail with `Unable to decrypt: data is corrupted or the key is wrong`. |

Plaintext is opt-in: only `OPENSHOP_ENV=development` or `test` (and the `dev`
command) stores a secret without a key. `NODE_ENV=development` does not, and
`OPENSHOP_ENV=production` never does. `start` and `worker` are not development commands: run
locally without a key, they need `OPENSHOP_ENV=development`. A missing key is
checked when the application starts; a key that is set is checked when a value
is first encrypted or decrypted. Rotating the key requires decrypting and re-encrypting existing
values; replacing it without a migration makes stored secrets unreadable.

The format has no associated data and no key identifier: a ciphertext is not
bound to the row it is in, so it could be copied to another row by whoever can
write to the database, and nothing says which key encrypted it, so a rotation
cannot be done gradually. Both would change the stored format, which the
TypeScript runtime reads too; they need a re-encryption migration and are not
done yet. Until then, treat write access to the database as access to the
secrets of every row it can copy.

MCP tokens are never stored: OpenShop keeps a salted scrypt hash and a
display fingerprint. See [MCP](mcp.md#tokens). Checking a secret is bounded:
at most as many scrypt hashes as CPUs (4 at most) run at once, so a flood of
wrong secrets cannot take the CPU or the memory of the server, and a secret
that was right is remembered for five minutes in the memory of the process.
Revoking, disabling, expiring or rotating a token takes effect at the next
request.

## Secrets in logs and memory dumps

`Debug` of a configuration or of a stored row never shows a secret: it prints
`[redacted]` for `api_secret` and `shopify_api_secret` (`ShopifyAppConfig`,
`RuntimeEnv`, the resolved app), `encryption_key`, the access token and the
nonce of an installation, the configuration of a provider, MCP tokens and their
hashes, and the secret headers and body fields of a request `MockTransport`
records (`OutboundRequest`), as `shopify_api` does for the requests it sends. A
failed MCP or session token is logged with the reason and the id of the token,
never the token.

## Outbound responses

The body of an answer to a request OpenShop sends (Shopify, token exchanges,
provider checks) is read as it arrives and refused beyond 10 MiB
(`MAX_RESPONSE_BYTES`), by its `Content-Length` when it has one and by counting
when it has not. A server that streams without end cannot fill the memory of the
process.

## Authentication boundaries

- `/api/*` trusts only a verified Shopify session token issued to a staff user.
- `/webhooks` verifies the raw bytes of the body with a matching Shopify app
  secret, and runs a delivery Shopify repeats once (see
  [Webhooks](webhooks.md#idempotence-and-durable-work)).
- `/proxy/*` accepts App Proxy HMAC or a Customer Account JWT. `/proxy/*` and
  `/ext/*` refuse the session token of a staff user.
- `/ext/*` accepts only a Customer Account JWT.
- `/routes/*` uses the authentication declared by each route function.
  `#[route(…, auth = <function>)]` runs the function before the handler; a
  route function declared without `auth` is public. Each method of a path
  declares its own.
- `/mcp` accepts only OpenShop-issued MCP tokens and applies explicit grants.
- `/ops/*` accepts only the session cookie of the operator dashboard, signed
  by the server after the password of the application. It reads every shop
  and writes nothing.

Signatures and token hashes are compared in constant time. An app proxy
signature must be exactly the hexadecimal digest; a valid signature followed by
other characters is rejected.

See [Authentication](authentication.md) for request formats.

## Multi-app isolation

Framework state is scoped by `(app_handle, shop)`. For custom tables, the rows
of a model returned by `Review::of(&ctx)` or `Review::for_shop(&db, shop)` are
limited to that shop in every operation. Everything else must apply its own
tenant filter: a query written with `sqlx`, `Review::all_shops(&db)`, and the
app handle when several apps share a table.

The rows of the framework follow the same rule: `FlowRun::of(&ctx)` gives the
runs of the shop and the app of a context, and `FlowRun::all_shops(&db)`
those of every shop. Use the second behind an authentication of your own,
never in what a merchant can call. See
[Read the framework tables](database.md#read-the-framework-tables).

## MCP permissions

Give tokens the smallest required permission set, use expirations, and revoke
unused tokens. High-risk custom tools should declare their risk level and a
confirmation hint. Audit logs record capability, token, target shop, outcome,
and duration; they do not replace external security monitoring.

## Proxy and webhook handlers

- Treat request bodies and Shopify payloads as untrusted input.
- Validate input before provider or database writes. A `#[proxy]` or
  `#[route]` function with a typed input refuses a request that does not fit
  it with `422` before the function runs.
- Give server values to a Liquid template with `.assign(…)`, which writes them
  as literals that their text cannot close. Do not build a template from
  request data.
- Make webhook and retryable flow side effects idempotent.
- Trust the verified context identity, not duplicated query/body fields.
- Do not return secrets or raw upstream error bodies to clients.

Request bodies are limited to 2 MB by default (`413` above that); change the
limit with the `max_request_body_bytes` setting of `app!`. The MCP endpoint
has its own limit of 1,000,000 bytes.

A request whose response has not begun after 60 seconds is answered with
`503`, and a request that comes while 1024 are being served is answered with
`503` and `Retry-After: 1` (`/health` is always answered). Change them with the
`request_timeout` and `max_concurrent_requests` settings of `app!`. Neither
applies to a response already streaming. A server behind a proxy should still
set the proxy's own limits on header and body read time and on connections.

## What Sentry receives

Nothing is sent to Sentry unless the application sets `sentry`. With a DSN,
an event carries the message of an error and of its causes, the stack of a
panic, the shop and the flow or the route it happened in, and the messages of
the log lines of the run. It carries no request header, query string, or body,
no input of a flow, and no payload of a log line. The path of a request is
never sent: an event is named after the route as it is declared, such as
`/orders/:id` or `/api/runs/{id}`. A URL in an error message or in a log
message is sent without its credentials, its query string, and the parts of
its path that look like a token; see
[what an event carries](sentry.md#what-an-event-carries) for exactly what is
removed. The rest of a message is sent as written: keep secrets and personal
data out of it.

## Operator dashboard exposure

`/ops` is off unless the application sets `ops_password`. One password
opens it for everyone who has it: nothing records who signed in. Its pages
only read, are sent with `Cache-Control: no-store` and a
`Content-Security-Policy` that allows no script and no framing, and write
what comes from a shop as text. Each client may try a password again only after
a growing delay, and a client that guesses does not keep the others out. See
[Operator dashboard](operator-dashboard.md).

## Embedded admin exposure

The production UI shell requires a valid installed shop launch. Any other
visitor receives a page that says the app opens from Shopify admin, without
anything of the application. The shell is sent with
`Content-Security-Policy: frame-ancestors https://<shop> https://admin.shopify.com;`
and `Cache-Control: no-store`. Every response carries
`X-Robots-Tag: noindex, nofollow, noarchive, nosnippet`, and `/robots.txt`
disallows all crawling; the app host is not a public website. Asset requests
whose path would leave the UI directory are answered with `404`. Keep the
documentation deployment separate from the application deployment.
