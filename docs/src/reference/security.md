# Security

## Required production controls

1. Set `OPENSHOP_ENV=production` and a random 64-character hexadecimal
   `ENCRYPTION_KEY`, and keep the key stable.
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
| No key, development | Values are stored in plaintext. A warning is logged once: `[openshop] ENCRYPTION_KEY not set — provider credentials stored in plaintext`. |
| No key, production | Writes, and reads of encrypted values, fail with `ENCRYPTION_KEY must be set in production`. |
| Key that is not 64 hex characters | Writes, and reads of encrypted values, fail with `ENCRYPTION_KEY must be 64 hex characters (32 bytes)`. |
| Encrypted value, no key, development | Reads fail with `Cannot decrypt provider config: ENCRYPTION_KEY not set` or `Cannot decrypt secret: ENCRYPTION_KEY not set`. |
| Wrong key or corrupted value | Reads fail with `Unable to decrypt: data is corrupted or the key is wrong`. |

The key is checked when a value is first encrypted or decrypted, not when the
process starts. Rotating the key requires decrypting and re-encrypting existing
values; replacing it without a migration makes stored secrets unreadable.

MCP tokens are never stored: OpenShop keeps a salted scrypt hash and a
display fingerprint. See [MCP](mcp.md#tokens).

## Authentication boundaries

- `/api/*` trusts only a verified Shopify session token issued to a staff user.
- `/webhooks` verifies the raw body with a matching Shopify app secret.
- `/proxy/*` accepts App Proxy HMAC or a Customer Account JWT.
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

## What Sentry receives

Nothing is sent to Sentry unless the application sets `sentry`. With a DSN,
an event carries the message of an error and of its causes, the stack of a
panic, the shop and the flow or the route it happened in, and the messages of
the log lines of the run. It carries no request header, query string, or body,
no input of a flow, and no payload of a log line. The path of a request is
never sent: an event is named after the route as it is declared, such as
`/orders/:id` or `/api/runs/{id}`. A URL in an error message or in a log
message is sent without its credentials and its query string. The rest of a
message is sent as written: keep secrets and personal data out of it. See
[Sentry](sentry.md).

## Operator dashboard exposure

`/ops` is off unless the application sets `ops_password`. One password
opens it for everyone who has it: nothing records who signed in. Its pages
only read, are sent with `Cache-Control: no-store` and a
`Content-Security-Policy` that allows no script and no framing, and write
what comes from a shop as text. Wrong passwords are answered one at a time,
after a growing delay. See [Operator dashboard](operator-dashboard.md).

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
