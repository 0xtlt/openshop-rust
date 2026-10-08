# Troubleshooting

Find your symptom below. Run commands from the app root with `DATABASE_URL`
exported for the intended environment. Start by checking migration status and
whether the HTTP process responds:

```bash
cargo run -- migrate status
curl -i http://localhost:3000/health
```

With a deployed binary, the first command is `app migrate status`.

The health endpoint does not check the database or worker. For a complete
verification, [run a smoke flow](operate-app.md#2-check-health-and-run-a-smoke-flow).

## The app does not start

The configuration is validated when it is built and again when the process
starts, so a mistake stops the process with a message instead of failing later
requests:

- `[openshop] Invalid config: …` names the entry of the application to fix,
  such as a cron that references an unknown flow, an invalid schedule, or a
  cron `input` that its flow does not accept.
- `… have the same route shape` means two proxy routes, server routes, or custom
  admin pages can match the same URL.
- `[openshop] Shopify app "<handle>" TOML not found` or
  `TOML is missing client_id` means a multi-app TOML file is absent from the
  working directory or not linked.
- `[openshop] Failed to connect to PostgreSQL` means `DATABASE_URL` is wrong or
  the database is unreachable. Without `DATABASE_URL`, the process tries the
  local development database.

## The app does not install

1. Confirm the shop ends in `.myshopify.com`.
2. Verify `SHOPIFY_API_KEY`, `SHOPIFY_API_SECRET`, and `HOST`.
3. In multi-app mode, include `app=<handle>` and confirm that handle exists.
4. Deploy the matching Shopify TOML and ensure its application URL points to
   the same `HOST`.
5. Retry from `/auth?shop=<shop>&app=<handle>`.

An `Invalid HMAC` response means the request was not signed with the secret of
a configured app. An `Invalid state/nonce` response means the OAuth callback is
stale or belongs to a different installation attempt.

## The embedded UI returns 401

Open the app from Shopify admin instead of navigating directly to its host. The
production UI requires a valid shop launch and an installed shop. For API calls,
send a fresh App Bridge session token as a bearer token. The Admin API only
accepts tokens issued to a staff user: a Customer Account or checkout extension
token is refused with
`Unauthorized: session token was not issued to a staff user`. Extensions call
[proxy routes](add-proxy-routes.md) instead.

## Runs stay pending

`start` queues runs but does not execute them. Start a worker:

```bash
app worker --concurrency=5
```

Check that web and worker use the same `DATABASE_URL` and the same build: a
worker that does not know a flow fails its runs with
`Flow "<name>" not registered`. If a run is scheduled in the future, inspect
`availableAt`.

If the worker repeatedly loses work, compare the `lease_duration` of the
`worker` setting with the 15-second interval at which a worker renews its
leases: keep the lease well above it. Also look for blocking code that stalls the process for longer than
the lease, because the renewal only happens while the worker can run its tasks.

## A run retries or fails unexpectedly

- Search the run logs and inspect the failed step.
- An error starting with `Invalid input for flow` means the run input does not
  match the flow's input type.
- Keep step results stable. A resumed run deserializes the stored output of each
  completed step into the step's current return type.
- Make provider writes idempotent because a failed step can run again.
- A canceled or timed-out step stops at its next `.await`. Blocking code and
  spawned tasks are not interrupted; pass them `ctx.signal`.
- Compare flow timeouts with retry delays. A retry scheduled after the flow
  deadline is not attempted.

## Database tables are missing

Apply the committed migrations before starting either process:

```bash
app migrate
app migrate status
```

Generate migrations only during development or CI. Production schema generation
is intentionally blocked, and so is the schema push of `dev`.

## A Shopify GraphQL call fails

- `No access token found for app "<app>" and shop "<shop>"` means the shop has
  no active installation for that app. Open the app from Shopify admin, or
  reinstall it.
- `Shopify GraphQL error (<status>)` is an HTTP failure from Shopify. A 401 or
  403 usually points to a revoked token or a missing scope.
- `GraphQL errors: …` lists the errors Shopify returned for the operation.
  Confirm the query against the API version in use and that the configured
  Shopify scopes allow the operation.

An operation written with `graphql!` is checked against the schema when the
project compiles. One sent with `ctx.shopify.graphql()` is not: a field that is
absent from the query is simply missing from the returned JSON.

## Proxy or extension calls return 401

For `/proxy/*`, configure a Shopify App Proxy or send a Customer Account session
JWT. For `/ext/*`, send a Customer Account JWT; App Proxy query HMAC alone is not
accepted. Verify the token audience matches the selected Shopify app.

## A request is refused with 413

Request bodies are limited to 2 MB by default. This applies to webhooks, proxy
routes, server routes, and the Admin API. Raise the limit with the
`max_request_body_bytes` setting of `app!` if a caller legitimately sends more.
The MCP endpoint keeps its own 1 MB limit.

## Provider secrets cannot be read back

Password fields are intentionally write-only. An empty password submitted from
the admin keeps the current value. To replace it, submit a new value. If the
`ENCRYPTION_KEY` changed, restore the original key; existing ciphertext cannot
be decrypted with a replacement.

## Tests cannot reach the database

The template's database tests are marked `#[ignore]` and run with
`cargo test -- --ignored`. They fail with
`[openshop] Failed to connect to the test database` when no PostgreSQL server
answers at `TEST_DATABASE_URL`, `DATABASE_URL`, or the local default. Follow
[Test an app](test-app.md) to start the database and point the tests at it.

## Still stuck?

Collect the OpenShop version (`cargo tree -i openshop`), failing command or URL,
run ID, and relevant web or worker error message. Remove credentials and
customer data, then
[open an issue](https://github.com/0xtlt/openshop-rust/issues) with a minimal reproduction.
