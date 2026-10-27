# Changelog

## Unreleased

- Shopify is reached through `shopify_api` 1.0.0-rc.1 instead of a client of
  OpenShop's own. Breaking changes:
  - `graphql!` is `shopify_api`'s, re-exported as `openshop::graphql!` and in
    the prelude. The crate that writes an operation depends on `shopify_api`,
    names its Admin API version in `[package.metadata.shopify_api]`
    `api-version` (the templates say `2026-10`), and commits the schema the
    first build downloads into `graphql/admin.schema.json`. Input objects are
    literals checked field by field, or `shopify_api::input!`. The
    `openshop-graphql` crate, `openshop codegen`, `OPENSHOP_GRAPHQL_SCHEMA`,
    `GraphqlOperation`, `SHOPIFY_API_VERSION` and `is_api_version` are gone.
  - `ShopifyClient` runs on a `shopify_api::Shopify` client: throttled
    requests wait and go again, a query is retried after a server error, and
    `api()` gives that client for pagination, bulk operations and webhook
    subscriptions. `graphql(query, variables)` takes anything that serializes
    to an object, `()` for none; `with_api_version` takes an `ApiVersion`.
    Its errors are `shopify_api::Error` (`Http`, `Graphql`, `Decode`).
  - The client credentials grant, the token exchange of the session token and
    the OAuth code exchange are `shopify_api`'s requests. OpenShop still
    decides which shop may get a token and stores it in `installations` as
    before; one shared client per app and shop makes concurrent callers ask
    Shopify once.
  - Session tokens, launches, OAuth callbacks, app proxy requests and
    webhooks are verified by `shopify_api`, which is stricter: ten seconds of
    clock skew on a session token (was 60 on `nbf`), `dest` and `iss` written
    exactly as Shopify does, a launch or an app proxy request of the last 90
    seconds (a stale launch was served from the stored token; proxies had five
    minutes), and only a webhook whose body is a JSON object. The uninstall
    webhook needs its payload and its shop header to name the same shop. The
    checks OpenShop made beyond the signature of an app proxy request stay.
  - `OpenShop::with_transport` takes a `shopify_api` `Transport`.
    `ReqwestTransport` implements it on the `reqwest` of the workspace, so
    `shopify_api` brings no HTTP client or TLS of its own; it still follows no
    redirection and reads no answer over 10 MiB whole, and streams the results
    of a bulk operation. `MockTransport` implements it too; `OutboundRequest`,
    `OutboundResponse` and `FORM_CONTENT_TYPE` moved to `openshop::testing`.
  - Operations go to the Admin API 2026-10, the version of the schema, where
    they went to 2026-04. The MCP tool `openshop.admin.graphql` takes a version
    from 2026-04, the oldest `shopify_api` calls.

- CI checks three things it did not. The committed admin UI bundle
  (`crates/openshop/ui-dist`) is compared with its sources: `mise run ui:build`
  (now `crates/openshop-ui/build-dist.sh`) writes the hash of what the bundle is
  built from to `crates/openshop-ui/ui-dist.sha256`, and the `ui-dist` job fails
  when the sources moved without a new bundle. The bundle was already stale (the
  admin UI was fixed for Rust 1.88 without rebuilding it) and carried the paths
  of the machine that built it: it is rebuilt, from a fixed place with the paths
  rewritten. The bytes of a bundle depend on the platform that builds it, so CI
  cannot compare them. The `docker` job makes a project of the `minimal` and of
  the `custom` template and builds its image; the `docs` job builds the guide;
  the `windows` job checks that `openshop` and the CLI compile there.
- The `Dockerfile` of a generated project is stricter. The image runs as a user
  without privileges (`app`, uid 10001) instead of root, trunk is checked against
  the SHA-256 of its release before it runs, and both base images are named by
  digest (Dependabot proposes the next ones). A private `openshop` repository
  is fetched with the optional build secret `gitconfig` (a git configuration
  with a token, kept outside the project so that `COPY . .` cannot copy it, and
  never written to a layer): `docker build --secret id=gitconfig,src=<file> .`;
  without it the build is the same as before. `gitconfig` is in the generated
  `.dockerignore` and `.gitignore`.
- CI is hardened. Every action is pinned to the commit of a release, the
  workflows only get `contents: read`, trunk and cargo-deny are installed from
  their release with a SHA-256 written in the repository (a release asset can
  be replaced), checkouts keep no credentials, and
  cargo runs with `--locked` (in `mise.toml` too) so a build never moves
  `Cargo.lock`. The toolchain is the one of `mise.toml` (1.98.1) instead of
  whatever `stable` is that day. Two jobs are new: `msrv`, which checks the
  workspace and the admin UI on Rust 1.88, the `rust-version` we promise, and
  `deny`, which runs cargo-deny on both lockfiles (advisories, licenses,
  sources; `deny.toml`). Dependabot proposes weekly updates of the crates and
  of the actions. Both found something: `openshop-ui` did not build on 1.88
  (a temporary borrowed in a struct literal, fixed), and the lockfile had
  `rustls` 0.23.45, which has four advisories fixed in 0.23.46 (updated).
- The CLI fails on a command it does not know: `openshop frobnicate` prints
  `[openshop] Unknown command: frobnicate` and the usage on stderr and exits
  with status 2, where it used to print the usage and exit 0, so a script that
  mistyped a command went on. No command, `help`, `--help` and `-h` still print
  the usage and exit 0.
- `openshop init` leaves no partial project behind: when it fails after it
  started writing, it removes the files it wrote and the directories it made,
  and nothing else (another `init` may write next to it). It rewrites the manifest of a template
  with `toml_edit` instead of line by line, so `workspace = true` is replaced
  whatever the layout (tables on several lines, dotted keys,
  `[dependencies.<name>]`), with comments kept. The generated projects are
  unchanged.
- `openshop codegen` gives up when the schema endpoint does not connect in 15
  seconds or the download takes more than 120, with a message that names the
  endpoint, instead of waiting for ever.
- The install command is `cargo install --locked --git … openshop-cli` in the
  README and the docs, so that the CLI is built with the dependency versions
  the repository tested. CONTRIBUTING lists what keeps `openshop-cli` from
  being published on crates.io (nothing is published).
- The admin UI shows and logs its failures instead of swallowing them. When the
  app cannot start, the page says why (a banner) instead of "Open this app from
  Shopify admin", which is kept for a missing session. A failed refresh of the
  custom pages keeps the ones shown, where it used to empty them. A request
  that gets no answer is logged to the browser console, with a toast for a
  write; a flow or function list that cannot be loaded, a log export that
  fails (it saved the error body as a file), a cron toggle the server refuses
  and a run that cannot be loaded are shown. The embedded bundle
  (`crates/openshop/ui-dist`) is rebuilt.
- `openshop init` makes a project that builds: its `openshop` dependency was
  pinned to the tag `v<version>` of the CLI, which the repository does not
  have, so the project of `cargo install --git … openshop-cli` followed by
  `openshop init` failed on its first `cargo` command. The CLI now records the
  commit it is built from, and the project depends on that commit (`rev`). A
  CLI built without git, from a package say, falls back to the tag and says at
  the end of `init` that the tag has to exist. `--openshop-rev=<commit>`,
  `--openshop-tag=<tag>` and `--openshop-branch=<name>` name another source,
  next to `--openshop-path=<dir>`; at most one of the four. CI checks a project of
  the `minimal` template that depends on the commit under test through git.
- Crons are read in UTC, not in the time zone of the host, and a cron takes an
  IANA `timezone` with `.timezone("Europe/Paris")`, daylight saving time
  included. This is a breaking change on a host whose time zone is not UTC: the
  same schedule fires at another hour until a `timezone` is given. A process
  that wakes late (a suspended host, a clock set forward) fires the latest
  occurrence it missed, once, where it used to fire every one at once. A
  scheduler that stops, because its schedule has no occurrence left or because it
  panicked, logs an error, reports it to Sentry and starts again after a wait
  that grows to five minutes, where it used to end without a word. The overrides
  of a firing are read in one query for all its shops instead of one per shop,
  and `/api/crons/toggle` and the MCP tool answer `Cron not found` to a key the
  application does not declare (the overrides of an old key, whose schedule
  changed, were never read and are still ignored).
- The log lines of a run wait in a queue of 10,000 instead of an unbounded one:
  a flow that logs faster than the database takes them loses the newest lines,
  never waits, and the run gets a `warn` line with the number dropped. A log
  write that fails is told to the runtime logger, and a NUL byte in a message
  or a payload is replaced by U+FFFD, where it used to make PostgreSQL refuse
  every line written in the same statement. A statement holds at most 1,000
  lines or 1 MiB of text: a flow that logged without pause used to grow one
  batch with everything the queue held.
- The framework schema has a migration, `openshop_drop_flow_runs_status_index`,
  that drops the index `flow_runs_status_available_created_idx` on
  `(status, available_at, created_at)`: the partial queue indexes of the second
  migration serve the claim and the expiry, and nothing else reads it. It loses
  no data. Run
  `migrate generate` then `migrate` before starting the new version: `start` and
  `worker` now refuse to start while `migrations/` holds a migration the
  database has not applied, or does not cover the framework schema, with
  `Not starting: … Run `migrate` first.`. Without a `migrations/` folder they
  start as before.
- `migrate generate` no longer records in the snapshot a change it can only
  leave as a `-- TODO` line (the primary key or the constraints of a column): the
  snapshot keeps the previous state, `migrate check` fails, and a second
  `generate` does not write the line again. Once the migrations write the change,
  `migrate generate --manual-done` records the models.
- `APP.main()` loads `.env` for `dev` before it starts the async runtime,
  instead of calling `set_var` from inside it. `cli::env::load_env_file` returns
  the number of variables it set; a `main` with its own runtime that calls
  `APP.run()` still gets `.env`, with a warning.
- Sentry events are scrubbed of URLs wherever they carry text: the tags, the
  extra data, every kind of context, the data of the breadcrumbs and the parameters of a
  log entry, and a request the application attaches loses its query string,
  cookies and body and the headers named like a secret.
- Several `start` processes can run crons: an occurrence of a cron for a shop
  is queued once, not once per process. Each process still starts a scheduler,
  and all of them compute the same occurrences; the first to insert a row in
  the new `cron_ticks` table `(app_handle, shop, cron_key, scheduled_at)` queues
  the run, in the same transaction, and the others skip it. A run that
  the flow's concurrency refuses leaves the occurrence free. There is no leader
  and nothing to elect again when a process stops. Two crons of the same flow
  and schedule, with another name, shops or input, each claim their own
  occurrences; the override key stays `flow:schedule`. The scheduler removes
  the rows older than a day, and skips, with a warning, an occurrence it
  reaches more than an hour late, such as after the process was paused: the
  others fired it long ago and its claim may be gone. The claim itself
  refuses an occurrence that old, by the clock of the database, so a process
  that stalls after the check cannot queue it again. The framework schema has
  a third migration, `openshop_cron_ticks`: run `migrate generate` then `migrate` before starting
  the new version, or every occurrence of a cron fails with a missing table
  (`migrate check` fails until it is there). The TypeScript runtime does not
  use the table. Until now, N replicas fired every cron N times, and the
  documentation told to keep a single web replica.
- The queries of the Admin API are bounded and escaped. Behaviour change:
  `GET /api/runs` and `GET /api/flows/:name/runs` answer `400` for an `offset`
  above 10,000 (a deeper page makes the database read every row before it;
  narrow the list with the filters), and `search` is text, where `%`, `_` and
  `\` used to be a pattern. The Shopify Functions listings read every
  page of Shopify's connections (50 at a time, up to 20 pages) instead of the
  first 50; the cart transform listing makes one request for the functions of
  the app and one for the cart transforms. On `/ops`, `Older` cuts a page on
  the time and the id of the last row, so rows created at the same instant are
  no longer skipped; an old `before` with the time alone still works. The
  percent-decoder of the custom admin pages and the one of the static assets
  are one (`openshop_shared::rpc::decode_uri_component`): `%+1` is no longer
  read as `%01`. Assets of the admin UI have a strong `ETag` and answer `304`
  to `If-None-Match`, are sent with `X-Content-Type-Options: nosniff` (the
  shell too), are not copied for each request when they are compiled in, and
  are read from a directory without blocking the runtime.
- Retrying, canceling and deleting a run are decided by the write, not by the
  status the request read before: a cancel no longer overwrites a run that
  completed meanwhile, a retry no longer resets a run that started again, and
  a delete (single or bulk) no longer removes a run that just started and its
  steps and logs; the request answers `409` with the current status, or `404`
  if the run is gone. Behaviour change: a retry follows the concurrency of the
  flow like a dispatch, under the same lock; when the flow rejects concurrent
  runs (the default) and another run of it is active for the shop, it answers
  `409` with `existingRunId` and the run stays as it was, where it used to run
  beside the other one. A cancel tells the worker of the run while it
  still holds the row, so a retry and a new claim of the run cannot take place
  before the signal and receive it.
- The Admin API answers what the caller got wrong with a `4xx`, and no longer
  shows what the server got wrong. Behaviour change:
  `POST /api/flows/:name/run` answers `404` for an unknown flow, and `422`
  with the reason when the input fails the input schema of the flow, instead
  of `500` and of `202` for a run that was bound to fail; the MCP tool
  `openshop.flows.run` checks the input too. A log query with an invalid or
  rejected regex, or past its limits (2,000 characters, 20 filters), answers
  `400` instead of an empty list, in the MCP tool as in the API; the regex is
  compiled once per query, not once per line, and `C:`, `A:` and `B:` counts
  are capped at 100. An unexpected failure keeps its `500` and gets an
  `x-error-id` header; an error of the database, the network or a file in an
  MCP tool or in a provider check is logged under an id, and the client reads
  `Internal error (error id <id>)` and that id, not the text, which can hold a
  query or a URL with a key. Errors a tool raises itself are shown as before.
  A provider check that takes more than 10 seconds answers `504`. Only an
  error written for the client is shown by the MCP endpoint and by a provider
  check: `openshop::PublicError`, `InvalidInput`, `InvalidFlowInput`,
  `FlowConcurrencyError` and the errors of the core tools. Any other error,
  including a decoding error that quotes a stored credential, gets the id.
  Behaviour change for an application: an MCP tool or a provider checker that
  returned a plain `anyhow!("…")` and wants it read by the client returns
  `PublicError::new("…")` instead. `app.dispatch_flow` (so `#[flow]`'s
  `dispatch`) now refuses input that fails the input schema of the flow with
  `InvalidFlowInput`, and a flow that is not registered with
  `UnknownFlowError`, before it queues a run; from a webhook handler this is a
  logged handler error, and Shopify still gets its `200`. A `last:` that
  reaches back further than the calendar answers `400` instead of panicking.
  The run page of the admin shows the reason when the server refuses a log
  search, instead of keeping the previous lines.
- Minor security findings of the review, one by one:
  - A webhook's HMAC (`/webhooks` and `/auth/webhooks/app-uninstalled`) is
    computed over the raw bytes of the body. It was computed over a lossy
    UTF-8 conversion of it, so a body that was not UTF-8 verified against a
    mangled payload. `verify_webhook_hmac` and `ShopifyApps::by_webhook_hmac`
    take `&[u8]`; `sign_webhook_body` takes anything that is bytes.
  - A webhook delivery that Shopify sends again (same `X-Shopify-Webhook-Id`,
    or `X-Shopify-Event-Id` without it) is acknowledged and its handler is
    not run again, within an hour, once its handler has completed. A handler
    that fails or panics, or a request Shopify cancels by hanging up, leaves
    nothing remembered and the repeat runs; a repeat that arrives while the
    handler is still running is answered `503` with `Retry-After: 5`. The
    memory is the process's, bounded to 100,000 deliveries, and keyed by app,
    topic, id and signature; a handler must still be idempotent across
    replicas and restarts. A delivery without either header is run as before.
  - A refused session token on `/api` is answered `Unauthorized: invalid
    session token`, where the answer named the claim that failed or the apps
    configured. The reason is logged as a warning. **Behaviour change** for a
    client that read the message.
  - A refused MCP token is logged (`[openshop] MCP token refused`: token id,
    reason, status, client address), never with the secret.
  - **Behaviour change:** `read_providers`, `write_providers`, `read_functions`
    and `manage_mcp_tokens` are no longer MCP permissions: no tool checked
    them, so granting them promised something that did not exist. A new token
    cannot be given them (`Unknown permission`); a token that holds one keeps
    working, the grant is ignored and listed in `stalePermissions`.
  - **Behaviour change:** in production, CORS no longer allows `localhost`,
    `127.0.0.1` and `[::1]` origins (unless `HOST` or `SHOPIFY_APP_URL` is
    one). `*.myshopify.com`, `admin.shopify.com`, `shopify.com`, Shopify's
    extension CDN and the application's own origin are allowed as before.
    `is_allowed_cors_origin` takes a `production` flag.
  - The body of an answer to an outbound request (`ReqwestTransport`) is read
    up to 10 MiB (`MAX_RESPONSE_BYTES`) and refused beyond, by
    `Content-Length` or by counting.
  - The sign-in of the operator dashboard no longer has one lock for everyone,
    that a client guessing wrong passwords kept held: each client (the peer
    address, a `/64` for IPv6, or the last `X-Forwarded-For` address when the
    peer is on a private network) attempts once, then again after a delay that
    doubles from 250 ms to 8 seconds. An attempt that comes sooner is `429`
    with `Retry-After`, at once, and not looked at. The server now hands the
    address of the peer to the handlers (`ConnectInfo`).
  - Shop domains are read as ASCII only: `normalize_shop_domain` refuses a
    non-ASCII name instead of lowercasing it with Unicode rules (the Kelvin
    sign became `k`).
  - **Behaviour change:** `/proxy/*` and `/ext/*` refuse the session token of a
    staff user (`Unauthorized: session token was issued to a staff user`); they
    took it for a customer-account token without a customer. Tokens of customer
    account and checkout extensions, buyers without an account included, are
    accepted as before.
  - `Debug` of `ShopifyAppConfig`, `RuntimeEnv`, the resolved Shopify app, the
    generated and parsed MCP tokens, the rows of installations, provider
    configurations and MCP tokens, and `OutboundRequest` prints `[redacted]`
    for secrets.
  - The CSV export of the logs of a run prefixes a cell that starts with `=`,
    `+`, `-`, `@`, a tab or a carriage return with `'`, so that a spreadsheet
    does not run it.
  - The JSON that `Liquid::assign` gives for a list or an object writes `<`,
    `>`, `&`, U+2028 and U+2029 as `\uXXXX`, so `</script>` or `<!--` in a value
    cannot end the `<script>` that prints it; the `custom` template prints
    every value with `| escape`.
  - Not addressed: the AES-GCM format has no associated data nor key id (a
    re-encryption migration, TypeScript parity); see
    [Security](docs/src/reference/security.md#encryption-at-rest).

- The token exchange of the session token, run by the first `/api` request of
  a shop without a token, is bounded: one request at a time exchanges for an
  app and shop, and the requests that arrive meanwhile wait for it and use its
  token, where each asked Shopify on its own, for up to the 60 seconds of the
  HTTP client. Shopify is asked for five seconds at most, and a failed
  exchange is not tried again for thirty seconds. A shop never waits for the
  exchange of another. After a timeout, the request is served without asking
  the client credentials grant too. `MockTransport::delay_responses` makes the
  mock answer late, for tests of a slow Shopify.
- `/auth` no longer writes to the database, and no longer overwrites the nonce
  of an installation in progress, which anybody could do by calling it for a
  shop. The `state` of the authorization is signed with the secret of the app
  (app handle, shop, expiry of ten minutes, nonce, browser) and bound to the
  browser by the cookie `openshop_oauth` (`HttpOnly`, `SameSite=Lax`, `Secure`
  on an `https` application URL, ten minutes). Several installations of a shop
  may be pending; the callback compares the signature in constant time,
  refuses a `state` that is expired, altered, of another browser, shop or app,
  or already spent, and creates the row of the installation if there is none.
  The `installations.nonce` column is no longer read or written, so an
  installation begun in the TypeScript runtime is not finished in this one,
  nor the reverse; `pending` in the operator dashboard only shows rows left by
  earlier versions. A callback whose token request fails has spent its
  `state`: the merchant starts again at `/auth`.
- Breaking, a security fix: an MCP token only acts on its own shop. The staff
  of one shop could create a token with `shopify_admin_graphql`, list every
  installed shop of the app with `openshop.shops.list`, and run
  `openshop.admin.graphql` on any of them with its offline token.
  `openshop.admin.graphql` now always runs against the shop of the token: its
  `shop` argument is optional, and a shop other than the token's is refused,
  without a request to Shopify. `openshop.shops.list` shows the installation of
  the token's shop only, and needs the `shopify_admin_graphql` permission; a
  token without it no longer sees the tool. A client that passed another shop
  must use a token of that shop.
- A shop only sees the crons that apply to it. `openshop.crons.list`,
  `openshop.flows.list`, `GET /api/crons` and `GET /api/flows` returned every
  configured cron, with its input and the shops it names, so a shop read the
  input and the domain of another shop's cron. They now list the crons that run
  without a shop, those of every shop and those that name the shop, and a cron
  naming several shops shows only the shop of the caller. Switching a cron
  that the shop does not see is refused (`Unknown cron` through MCP, `404
  Cron not found` on `POST /api/crons/toggle`).
- **Breaking:** secrets are no longer stored in plaintext by default. Without
  an `ENCRYPTION_KEY`, the offline access tokens of Shopify and the
  credentials of providers were stored in plaintext unless `OPENSHOP_ENV` was
  exactly `production`, so `staging`, `prod`, a typo or no `OPENSHOP_ENV` at
  all silently stored them in clear. Now an application without a key does not
  start (`OpenShop::new` fails with `ENCRYPTION_KEY must be set …`, which
  names how to set one), unless the environment is explicitly a development
  one: `OPENSHOP_ENV` is `development` or `test` (`NODE_ENV` does not count),
  or the command is `openshop dev`. `OPENSHOP_ENV=production` never allows plaintext.
  `start` and `worker` run locally without a key now need a key or
  `OPENSHOP_ENV=development`; set `ENCRYPTION_KEY` in every deployed
  environment. Rows already stored in plaintext are still read, and the
  encryption format is unchanged. A `RuntimeEnv` built by hand gets a
  `development` field (false by default) that
  `RuntimeEnv::allows_plaintext_secrets()` reads, and `TestOptions` a
  `development` field, true by default, so tests keep working without a key;
  with `false` and no key the context does not start.
- An app proxy request is checked beyond its signature. Its `timestamp` must
  be present and within five minutes of the server clock, on either side: a
  signed URL that was captured could be replayed for ever, since nothing in
  the signature expires. A parameter whose name or value contains `=`, or
  whose name is empty, is refused: Shopify signs the `key=value` pairs glued
  without separator, so the same signature stood for the query cut in other
  pairs, with another `shop` for instance. A repeated parameter is verified as
  Shopify signs it, its values joined by `,` (`extra=1&extra=2` is signed
  `extra=1,2`; it was verified as `extra=1`, and refused), and the pairs are
  sorted as the `key=value` strings Shopify sorts, not by key. `shop`,
  `timestamp`, `path_prefix`, `logged_in_customer_id` and `signature` cannot be
  repeated. A `=` is not the only way to move the end of a value into the
  next key: a parameter of the visitor is also refused when the value before
  it, glued to its name, ends with `shop`, `timestamp`, `path_prefix` or
  `logged_in_customer_id` (a request of an anonymous visitor could be read as
  the one of customer 42); `timestamp` must be digits, `path_prefix` a plain
  path, and `shop` exactly a `<name>.myshopify.com` domain (`shop` used to be
  read as a URL, and `a.myshopify.comt@b.myshopify.com` is one for `b`; it is
  answered `Invalid shop parameter`). All of these answer `401 Invalid proxy
  signature`. The signature
  still covers neither the path, the method nor the body, which Shopify does
  not sign. Breaking for code that calls `server::hmac::verify_proxy_signature`
  (it takes the query pairs and the current Unix time) or signs proxy queries
  without a `timestamp` in its tests: `testing::proxy_signature` signs what it
  is given, add a fresh `timestamp`.
- `migrate generate` refuses to write a migration that loses data: a dropped
  table or column, or a column converted to a type that can truncate or round
  its values (a widening such as `integer` to `bigint` is fine). The error
  lists the statements, and nothing is written or recorded in the snapshot.
  Pass `--allow-destructive` to generate it anyway, or declare a rename with
  `renamed_from` when the data is meant to be kept. The `dev` push never
  drops anything and is unchanged.
- A request that has no response after 60 seconds is answered with `503`
  (`Request timed out`) and its work is cancelled, and a request that comes
  while 1024 are being served is answered with `503` and `Retry-After: 1`;
  `/health` is always answered. The `request_timeout` and
  `max_concurrent_requests` settings of `app!` change them, and a value of
  zero is refused. A response already streaming is not counted. Before, a
  request could hold the server for as long as its handler ran, and nothing
  bounded how many ran at once.
- Checking the secret of an MCP token no longer lets whoever knows a token id
  cost the server a scrypt hash per request: at most as many hashes as CPUs (4
  at most) run at once, a request that gets no turn in five seconds is
  answered with `503`, and a secret that was right is remembered for five
  minutes under a keyed hash that lives in the memory of the process. The
  stored hash is unchanged (scrypt, shared with the TypeScript runtime), and
  the state of a token is still read at every request, so a revoked, disabled,
  expired or rotated token is refused at once.
- The macros refuse more of what they used to take, each with an error at the
  item: an option of `#[flow]` given twice (`timeout`, `step_timeout`,
  `concurrency`, `retry_policy`); a field of a `Model` named `table`,
  `columns` or `has_shop`, whose constant would hide `Record::TABLE`,
  `Record::COLUMNS` or `Model::HAS_SHOP`, and two fields that give the same
  constant; an item of a `#[provider]` block named `NAME`, `provider` or `on`,
  or a method whose constant (`name` is `NAME`) is already in the block; an
  item named `PATH`, or like the constant of a loader or an action, in an
  `#[admin_page]` block. `#[input]` leaves the naming of the fields to a type
  only when its `#[serde(…)]` has a `rename_all` option, no longer when the
  text appears anywhere in it.
- `graphql!` checks the default value of a variable against its type, and
  refuses an integer literal beyond the 32 bits of `Int`. When `openshop
  codegen` creates `graphql/admin.schema.json`, it touches `src/main.rs` and
  `src/lib.rs`, so that the crate is built again with the schema; `cargo` did
  not know a file that did not exist at the first build.

- The tests refuse a `DATABASE_URL` that is not on the machine: the harness
  (`TestDb`, `#[openshop::test]`) uses `TEST_DATABASE_URL`, then `DATABASE_URL`
  when its host is `localhost`, a loopback address or a socket, then the local
  development database. A shell with the `DATABASE_URL` of a real database in
  it no longer creates and drops schemas there; set `TEST_DATABASE_URL` to
  choose such a database on purpose. `openshop::testing::test_database_url()`
  gives the URL the harness uses. Without PostgreSQL, a test fails within ten
  seconds saying so (`mise run db:up`, or `TEST_DATABASE_URL`) instead of
  hanging.
- `FakeMethod::call` takes its place in the record when the call starts: two
  calls running at once each get their own index for `on_call_returns` and
  `on_call_rejects`, and `calls()` lists them in the order they started in
  (it listed them in the order they finished, the second one taking the rule
  of the first). A call is recorded as soon as it starts, with its result
  filled in when it ends; a `reset()` in the meantime forgets it.
- `docker-compose.yml` publishes PostgreSQL on `127.0.0.1:5432` only, not on
  every interface of the machine.
- The workspace declares its lints (`[workspace.lints]`: `unsafe_code` and
  unused `#[must_use]` results denied, no `dbg!`, `todo!` or `unimplemented!`
  left in) and its members inherit them with `[lints] workspace = true`. A
  project made by `openshop init` does not get the section. `rand` is 0.10, the
  version `sentry` already uses: the lockfile loses `rand` 0.9, its
  `rand_chacha` and `rand_core`, and one `getrandom`.
- The logs of a run are indexed, bounded and paged. The framework schema gets
  a migration, `openshop_logs_indexes` (also added to the three templates):
  indexes on `logs (created_at)`, on `logs (level, created_at)` and on
  `flow_runs ((id::text))`, which the operator dashboard reads the latest
  lines and joins a line to its run through. Nothing else changes in the
  table, which the TypeScript runtime shares: `flow_run_id` stays text, with
  no foreign key. The statements of the dashboard are cancelled after 10
  seconds instead of running for as long as the database lets them.
  `GET /api/runs/:id/logs` returns the newest 1,000 lines (`limit` up to
  5,000) with a `next` cursor to pass as `cursor` for the following page, and
  `levelCounts`, the lines of each level in the run, which the admin UI reads
  instead of fetching the whole run a second time; levels and time bounds are
  filtered by the database, and the text filters on the lines as they are read.
  The export is streamed instead of built in memory. The tool
  `openshop.logs.search` is unchanged. The admin UI bundle is rebuilt.
- The new `log_retention` setting of `app!` (a `Duration`, or `None`) has the
  workers remove the log lines older than it, once an hour, 5,000 at a time.
  It is unset by default: the lines are kept as before.

- A provider configuration is checked against the type of each field before it
  is saved, and a Shopify Function configuration likewise. A `select` value
  must be one of the options, a `number` a number (`"abc"` was dropped without
  an error for an optional field), a `checkbox` `true`, `false`, `"true"` or
  `"false"` (`"yes"` was saved as false), and a `text` or `password` a string.
  The error names the field: `Field "mode": must be one of "live", "test"
  (was "prod")`. Keys that are not fields are still ignored. Configurations
  that used to be accepted are now refused with `400`.
- `validators::url()` accepts only `http` and `https` URLs with a host:
  `file:///etc/passwd` and `gopher://…` are refused. The new
  `validators::public_url()` also refuses `localhost`, `metadata.google.internal`
  and IP addresses that are loopback, private, link-local (the cloud metadata
  endpoint), shared, multicast or unspecified, for a URL the server fetches on
  behalf of a merchant. It cannot see through DNS rebinding.
- `openshop init` makes a project from one of three templates: `minimal`, one
  provider and one flow, as before; `custom`, a custom app with every building
  block and a page of its own next to the built-in admin; `public`, a public
  app whose admin is made of its own pages, with the operator dashboard for
  whoever runs it. `--template=<name>` names it. In a terminal, `init` asks for
  the directory and the template it was not given, the template chosen with
  the arrow keys; elsewhere it fails with the list of templates. A project
  gets the migrations of its template, the tables of its models included, and
  a test that they cover its schema, and the `rustfmt.toml` its template is
  formatted with. A directory named after a dependency of the template
  (`serde`, `leptos`, `openshop`) is refused, since cargo would not build the
  project. Ctrl-C during a question shows the cursor again.
- The `Dockerfile` of a project made from the `custom` or `public` template
  builds its admin UI bundle: it installs the WebAssembly target, trunk and
  the `openshop` CLI of the tag, branch or rev of `openshop` that
  `Cargo.toml` names, runs `openshop build`, copies `dist` into the image and
  sets `OPENSHOP_UI_DIR=/app/dist`. Without it, the image served the admin
  built into the framework, without the pages of the application. `dist/` is
  in the generated `.gitignore` and `.dockerignore`. The `Dockerfile` of every
  template builds with `--locked` once the project has its `Cargo.lock`, and
  without it before, which failed, and keeps the git checkouts of cargo
  between builds.
- The demos are these templates: `apps/demo-custom` and `apps/demo-public`
  are `templates/custom` and `templates/public`, next to `templates/minimal`
  and `templates/shared`, the files every project gets. They are crates of the
  workspace, compiled and tested with it, and the CLI embeds them when it is
  built. The `demo:*` tasks of mise are `templates:ui`, `store:custom` and
  `store:public`, and `mise run init` makes a project from the templates of a
  checkout, depending on it. The MCP tool of the custom template is
  `app.status`, behind the permission `status:read`.
- A development store of the organization that owns the app gets its Admin
  API token with Shopify's client credentials grant, with no OAuth. A launch
  from Shopify admin whose signed query names a shop and is less than 90
  seconds old asks Shopify for a token when the app has none for that shop,
  before redirecting to `/auth` as before; a development store with an empty
  database then opens in the admin, where Shopify refuses to frame `/auth`. An
  older signed launch is served as before, from the stored token alone. An
  Admin API request whose verified session token names a shop without a token
  exchanges the session token first, as before, and asks for the grant only
  when Shopify refuses the exchange or it cannot run; a token this process
  obtained with the grant, at a launch for example, is replaced by the
  exchange at the first session after it, and when it is about to expire.
  Unsigned input never starts a grant for a shop of its choosing.
  Background work (flows, including the ones a webhook dispatches, webhook
  handlers, crons, routes, proxies and the MCP `openshop.admin.graphql` tool)
  may ask in two cases only: for the one shop the new `client_credentials_shop`
  setting of `app!` names (the templates read it from
  `SHOPIFY_CLIENT_CREDENTIALS_SHOP`), without ever bringing back a shop
  marked uninstalled (a cron of every shop targets it even without an
  installation row), and to renew the token a shop's installation row holds,
  once after a `401` or before a token this process obtained expires. The
  token is stored
  encrypted in `installations` like the others, with no schema change, and
  only where the row still holds what it held when Shopify was asked: a shop
  uninstalled, or given a token by OAuth or the exchange, meanwhile keeps
  what it has. A vouched grant that brings back a shop marked uninstalled
  sets its `installed_at`, as OAuth does. The client of an admin session
  pointed at another shop or app may do what background work may. A token request has its own 5-second timeout.
  A definite refusal (a `4xx` with Shopify's error code, such as
  `shop_not_permitted`) is remembered by the process for an hour per app and
  shop, any other failure (a token the database would not store included)
  for 30 seconds; the failure of background work never holds back a launch or
  an Admin API request Shopify vouched for. Nothing is remembered for a shop a
  caller may not ask a token of, which is refused before it waits for the
  turn of another, nor once a failure or a token is over. Shopify's refusal
  of a merchant's store at its launch is logged as information. In
  production without `ENCRYPTION_KEY`, nothing is asked: a launch is sent to
  `/auth`, background work fails, and a cron of every shop says so once per
  app. A renewal after a `401` that fails leaves the `401` as the answer, and
  the clients of the application do not wait on each other while one of them
  asks for a token.
- Breaking for some users: `testing::TestOptions` has a new public field,
  `client_credentials_shop`, which takes the place of the setting of the
  application in a test; a struct literal of it without `..Default::default()`
  no longer compiles. `ReqwestTransport` no longer follows redirections: a request to
  Shopify carries a secret, a token or a code, which a `3xx` would send on to
  another host. The `3xx` is the answer, and a custom `HttpTransport` must do
  the same.
- A shop name longer than 63 characters (one DNS label) is not a shop
  domain.
- `ShopifyClient` keeps the token it read per Shopify app and shop: a clone
  whose `shop` or `shopify_app` was changed used to send the token of the
  first shop to the second one, and now reads its own.
- A `ShopifyClient` whose `shop` is not `<name>.myshopify.com` or a bare
  `<name>` fails with `[openshop] Invalid shop domain` and sends nothing; it
  used to build the address of the Admin API from the value as it was given.
  A webhook whose `X-Shopify-Shop-Domain` is not a shop written in full as
  `<name>.myshopify.com` is answered `400` when a handler matches, and its
  `shop` is lowercased.
- `testing::admin_context` gives the `AdminContext` the client of an admin
  request, as in production: a token the Admin API refuses is renewed once.
- `ReqwestTransport` sends `OutboundRequest.body` as a URL-encoded form when
  the media type of the request's `Content-Type` is
  `application/x-www-form-urlencoded` (`runtime::FORM_CONTENT_TYPE`), whatever
  its parameters. The `HttpTransport` trait is unchanged, but OpenShop now
  sends such a request for the client credentials grant: a custom
  `HttpTransport` must encode it as a form too.
- A caller that is still sending when its request is refused reads the answer.
  A body over the request body limit is answered `413` as soon as the limit is
  crossed, and the connection was closed there: cut off in the middle of its
  upload, the caller saw a connection reset instead of the `413`. The server
  now closes its side first and reads what the caller still sends until it
  closes in turn, for five seconds at most.
- The payload of a log line is stored as the TypeScript runtime stores it:
  compact, with its keys in the order they were given. It went through `jsonb`
  on its way to the `json` column, which spaced it and sorted its keys.
- Sentry is supported natively. With the `sentry` setting of `app!`, which
  takes the DSN the application reads
  (`sentry: std::env::var("SENTRY_DSN").ok()`), `dev`, `start` and `worker`
  report what fails: each failed attempt of a flow run, a warning while it
  will be retried and an error at the last one; the runs a worker fails
  without an attempt; a cron that cannot queue its runs; the webhook, proxy,
  route and admin handlers that return an error; the `500` of the Admin API;
  and the panics, once each, where they happen, without a thread of the
  server waiting on Sentry. An event is named after its flow or its route, as
  the route is declared and never by the path of the request, carries the
  shop as a tag and as its user, and has the log lines of the run as
  breadcrumbs, without their payload. No header, query string, body or flow
  input is sent, and a URL in a message loses its credentials, its query
  string and what looks like a token in its path. The panic of an attempt
  that will be retried is a warning, like its error would be, and a cron that
  fails for many shops is one event. `SentryConfig` names the environment (`production` or `development`
  by default, after `OPENSHOP_ENV`) and the release, and `.configure(…)`
  reaches the other options of the SDK. The SDK is re-exported as
  `openshop::sentry` for what the application captures itself, in the scope
  of the flow or the request. Events are sent with the HTTP client of the
  framework. The integration is the `sentry` Cargo feature, on by default;
  the framework reads no environment variable for it.
- In `dev`, the admin UI reloads by itself: when the app has been started
  again, and when the bundle it is served from disk has been built again. The
  shell is served with a script that listens to `GET /__openshop/reload`, an
  event stream that names the version being served. Neither exists in `start`.
  With `openshop dev`, a change to a page or to the server is on screen
  without a restart or a reload by hand.
- `openshop dev` also watches the crates on disk the project is compiled with,
  its path dependencies and theirs: the app is started again, and its admin UI
  bundle built again, when one of them changes. `openshop dev -p my-app` in a
  workspace watches the crate of `my-app`, which it did not.
- `openshop::cli::shutdown_signal()` is public: it resolves on `SIGINT` or
  `SIGTERM`, for a process that embeds the server and stops as `dev` does.
- An admin can be made of the pages of the application only.
  `builtin_pages: AdminPagesConfig::none()` disables every built-in page
  (flows, runs, logs, providers, crons, functions, MCP) for the merchant; a
  page is opened again by naming it. Such an application declares the home of
  its admin, or its configuration is rejected.
- A custom admin page can be declared at `/`: `#[admin_page("/")]` is the home
  of the admin, in place of the built-in dashboard. It has no menu entry, and
  its access rule is its own.
- The operator dashboard, under `/ops`: what happens in every shop over a
  period, read-only, outside Shopify admin. An overview with a chart of the
  runs and the failures, the runs and the log lines with their filters, the
  flows, the shops and each of them, the crons. It
  is served when the application gives it a password,
  `ops_password: std::env::var("OPS_PASSWORD").ok()`, and answers `404`
  otherwise. Its pages are HTML rendered by the server, without script,
  with charts drawn as SVG; the session is a signed cookie, with no table. Production refuses a password
  shorter than 16 characters. `ops` is a reserved first segment for custom
  admin pages.
- A visitor who opens the application without a shop is told where the app
  opens from, on a page written with Polaris web components: the `401` of
  `start`, the `403` of `dev`, which was plain text, and what the admin UI
  shows without a Shopify session. It shows nothing of the application.
- The framework tables are read with typed columns: `FlowRun::of(&ctx)` for
  the runs of a shop, `FlowRun::all_shops(&db)` and
  `Installation::installed(&db)` across shops, `StepResult::of_run` and
  `LogRow::of_run` for a run. These records are never written, and
  `Installation` does not read the access token. `Record` is the part of
  `Model` that reads: `TABLE` and `COLUMNS` moved to it, and
  `openshop::db::schema::Installation`, the whole row, is now
  `InstallationRow`.
- A custom admin page is written in one place. The view of a page is the
  function marked `#[view]` in its `#[admin_page]` block, next to the loaders
  and the actions it calls, and no longer lives in a separate `ui/` crate. It
  calls them through a constant the block gains per function,
  `use_loader(Self::LIST_REVIEWS, || ())`, which carries the type the
  function takes and the type it returns: the names and the types are no
  longer written a second time. Views need the `ui` feature of `openshop`,
  which re-exports `openshop-ui` as `openshop::ui`.
- The admin UI bundle of an application is built by `openshop` from its
  pages: `openshop dev` and `openshop build` compile to WebAssembly, on their
  own, the modules that declare a page with a view, and write the bundle into
  `dist/`. The application has no file of the bundle to keep, and the rest of
  it is written as before. `openshop build-ui` builds the bundle alone.
- `openshop init-ui` writes an example page, `src/admin/hello.rs`, and adds
  the `ui` feature and `leptos` to the manifest. It no longer takes a
  directory.
- Every context of a shop gives the same three things: `ctx.db`, `ctx.shopify`
  and `ctx.connectors`, in a flow, a webhook, a proxy, an admin function, an
  MCP tool, and on the shop a route resolves with `ctx.for_shop(…)`. Nothing
  is read when a context is built: the access token is read at the first
  Shopify request, the provider configuration at the first provider call.
  `OpenShop::shopify_client` and `OpenShop::connectors` are no longer `async`.
- `#[flow]` adds `dispatch` to the flow: `sync_orders::dispatch(&ctx,
  SyncOrdersInput { limit: 10 })` queues a run for the shop and the Shopify
  app of a context, with a typed input. `app.dispatch_flow(DispatchOptions::new(
  name, shop)…)` remains for a flow known by name, a delay or a retry policy.
- `#[input]` makes a type the input of a flow, of an MCP tool or of an admin
  function: it derives `Deserialize`, `Serialize` and `JsonSchema` without
  `serde` or `schemars` among the dependencies of the application, and names
  fields in camel case outside Rust.
- The logger of a flow takes the message first: `ctx.logger.info(message)`,
  and `info_with(message, json!({ … }))` for data, likewise for `warn` and
  `error`.
- Liquid templates live in files next to their handler: `liquid!(
  "reviews.liquid")` embeds one when the crate compiles, and
  `.assign("name", value)` gives it what only the server knows, written as a
  Liquid literal the value cannot close.
- `#[route(GET, "/path", json)]`: the function returns anything that
  serializes, like a proxy.
- What the authentication function of a route returns for an authenticated
  request keeps its type: `RouteAuthOutcome<Partner>` on one side,
  `ctx: RouteContext<Partner>` and `ctx.auth.key` on the other, and a route
  whose handler and authentication disagree does not compile. Without a type,
  both carry a `Value`, as before.
- An admin page is served as soon as it is listed in `admin_pages`, and
  `#[admin_page("/reviews", nav = "Reviews")]` gives it its entry in the menu
  of the admin. The `admin_menu` setting sets the menu by hand, with
  `AdminMenuItem::new(label, path)`: it replaces `custom_pages`,
  `CustomAdminNavigationItem`, `ExperimentalConfig` and `.experimental(…)`,
  and its errors name `adminMenu[0].path`.
- An MCP permission is a constant, `McpPermission::new("reviews:read", "Read
  reviews")`, and the tools and resources that list it register it.
  `McpConfig::disabled()` turns MCP off, and the MCP page of the admin with
  it unless the application sets a mode for that page.
- The input of a cron is checked against the input of its flow when the
  application is built. A cron runs for no shop, `.all_shops()`, `.shop(…)` or
  `.shops([…])`; `"all"` and `"global"` are no longer read from a string.
- `#[openshop::test(APP)]` makes an `async fn(ctx: &TestContext)` a test of
  the application: started on an isolated database with its shop installed,
  and shut down when the function returns. The argument can also be an
  expression that resolves to a test context, `#[openshop::test(context())]`.
  `TestContext::for_app`, `APP.test()` and `TestOptions::installed()` do the
  same by hand. A test context stands for its shop: `Review::of(ctx)` and
  `sync_orders::dispatch(ctx, input)` take it.
- `use openshop::prelude::*;` imports what the code of an application uses.
- The root of the crate holds what an application names, about half of what
  it exported: the attributes, `app!`, the contexts, the settings and the
  values handlers exchange. The definitions a block is built from by hand
  moved to `openshop::builder` with `OpenShopBuilder` (`Flow`, `Provider`,
  `Field`, `Proxy`, `Route`, `Webhook`, `Function`, `McpTool`, `McpResource`,
  `AdminPage`, `Table`, `InputSchema`…). `Worker`, `Scheduler`, `run_flow`,
  `cancel_run`, `StepExecutor` and `SleepSignal` are in `openshop::engine`,
  `ServerOptions` in `openshop::server`, `RuntimeEnv` in `openshop::runtime`,
  `IntoVariable` in `openshop::shopify` and `Declared` in `openshop::declare`.
- Durations are `Duration`s: `RetryPolicy::initial_interval` and
  `max_interval`, `WorkerConfig::poll_interval`, `poll_max_interval` and
  `lease_duration`, `DispatchOptions::delay`. In `#[flow]`, `timeout` and
  `step_timeout` are written as text checked when the crate compiles:
  `timeout = "10m"`, `"30s"`, `"1h30m"`.
- The setting of the built-in pages of the admin is `builtin_pages`, with
  `.builtin_pages(…)` and `.builtin_page(id, mode)` on the builder, next to
  `admin_pages`, the pages of the application.
- Names. A definition is named after what it defines: `Field`, `Fields`,
  `Function`, `FunctionType`, `Proxy`, `Route`, `McpTool`, `McpResource`,
  `McpPermission`, `Cron`. A context is named after its attribute:
  `RouteContext`, `RouteAuthContext`, `AdminContext`, `McpContext`, whose
  `app_handle` is `shopify_app` like everywhere else. `RetryPolicy` and
  `WorkerConfig` are what an application sets. `RunOutcome` and `QueuedRun`
  are what running and dispatching a flow return, and `DispatchOptions` what
  `dispatch_flow` takes. Constructors are associated functions:
  `AdminFunction::loader`, `action`, `typed_loader` and `typed_action`,
  `Table::conventional`, `Factory::new`, `FakeProviders::new`,
  `ShopifyClient::new`; `AdminPage::access` and `AdminFunction::authorize`
  take the function itself. The process logger is `openshop::logger()` and
  `set_logger`, the server is `openshop::server::router` and `listen`, and the
  builders of MCP capabilities say `permissions` and `risk` like the
  attributes.
- Typed provider calls: a `ProviderCall<A, R>` constant names a method of a
  provider (`Provider::handle`), with the saved configuration deserialized
  into a struct. `Provider::typed_checker` does the same for the connection
  check.
- Providers as structs: `#[derive(ProviderConfig)]` makes the fields of a
  struct the configuration form, and `#[provider]` on its `impl` block makes
  its public `async` methods the methods of the provider, with the saved
  configuration as `self`. The provider is named after the struct
  (`Warehouse` is `warehouse`), or by `#[provider("name")]`, and the block
  carries `Warehouse::NAME`. Flows call the methods with
  `Warehouse::on(&ctx.connectors).push(&orders)`.
- Fields of a provider or of a Shopify Function state what a value must
  satisfy in `#[field(…)]`: `url`, `non_empty`, `range(min = …, max = …)` and
  `length(min = …, max = …)`, before any `validate = …`. A field without a
  `label` is labeled after its name: `batch_size` is "Batch size".
  `Field::validate` adds a validator after the ones already set.
- In a test, `ctx.fakes.for_call(Warehouse::PUSH)` is a typed fake: `returns`
  takes what the method returns, and its calls give the arguments as the
  method receives them.
- `OpenShopBuilder::providers`, `flows` and `with`.
- Typed GraphQL: `graphql!("…", name = value)` checks a Shopify Admin
  operation against the schema at compile time and types its variables and its
  result; `ShopifyClient::execute` runs it. The module form,
  `graphql! { mod name = "…"; }`, names the generated types and input objects.
  The Admin schema of the default API version is bundled; `openshop codegen`
  downloads another one.
- `#[derive(Model)]` with `#[model("reviews")]`: a struct as the single
  description of an application table, with typed rows for insertion, lookup,
  filters, ordering, update and deletion, limited to a shop:
  `Review::of(&ctx)` for the shop a handler runs for, `Review::for_shop(&db,
  &shop)` without a context. The derive implements the trait `Model`;
  `Review::table()` is the `Table` the struct describes.
- `migrate generate` compares the models with `migrations/schema.snapshot.json`
  and writes what changed: created and dropped tables, added, dropped, renamed
  and altered columns, indexes. Statements that can lose data or fail come with
  a warning. `migrate check` fails while a model change has no migration.
- `#[model(references(Other), on_delete = "cascade")]` declares a foreign key.
  Tables are created before their constraints, in any registration order, and
  generated migrations add, drop and rename the constraints.
- `dev` adds the columns, indexes and foreign keys a model gained to an
  existing table.
- Blocks declared with attributes: a block is the function or the struct
  itself, and no function builds it. `#[flow]`, `#[webhook]`, `#[proxy]`,
  `#[route]`, `#[mcp_tool]` and `#[mcp_resource]` go on `async` functions,
  which stay ordinary functions; the input of a flow, the payload of a
  webhook, the arguments of an MCP tool, and the query or the JSON body of a
  proxy or route request are an optional, typed second argument. A proxy or a
  route answers `422` to a request that does not fit its type.
  `#[admin_page("/path")]` goes on an `impl` block whose functions are marked
  `#[access]`, `#[loader]` or `#[action]`. `#[derive(FunctionConfig)]` with
  `#[function(…)]` makes a struct the configuration form of a Shopify
  Function. Each block carries its constants: `sync_orders::NAME`,
  `orders_create::TOPICS`, `reviews::PATH`, `ReviewsPage::PATH`,
  `VolumeDiscount::NAME`.
- `app!` declares an application as a value, an `App`: it lists the blocks by
  name (`flows: [sync_orders]`, `models: [Review]`…) and takes the settings of
  the builder (`crons: […]`, `on_error: report`…), which can fail with `?`
  (`shopify: shopify_apps()?`). `APP.main()` runs the command given on the
  command line from a plain `fn main`, so an application does not depend on
  `tokio`; `APP.run()` does it on a runtime of your own. `APP.config()`
  returns the validated configuration and `APP.builder()` the builder to add
  to.
- `OpenShopBuilder::add::<Block>()` adds a declared block of any kind.
  `model_of` is removed: use `.add::<Review>()`, or `models: [Review]` in
  `app!`.
- `build()` refuses a key registered twice, with
  `[openshop] Invalid config: flows.syncOrders is registered twice`: providers,
  flows, functions, webhooks, models, and MCP tools, resources and permissions.
- The methods of a proxy or route path can be registered separately:
  `.proxy(path, …)` and `.route(path, …)` called again for a path add methods
  to it. A method registered twice is refused, as are two response types on a
  proxy path. Each method of a route keeps its own authentication.
- `.mcp(…)` adds to the MCP permissions, tools and resources already
  registered instead of replacing them.
- A project created by `openshop init` is written with the declared forms
  (`app!`, `#[flow]`, `#[input]`, `#[provider]`, `graphql!`) and depends on
  `openshop` and `serde` only. It starts with MCP off: `POST /mcp` is refused
  and the admin has no MCP page, until `mcp: McpConfig::disabled()` is removed
  from `src/main.rs`.
- `OpenShopBuilder::crons`, `mcp_tool` and `mcp_resource`.
- Responses under 1 KB are no longer compressed: on small JSON responses the
  compression took more CPU than the rest of the request and did not make them
  smaller.
- The pool no longer pings a connection before every statement, which cost
  each one a second round trip to PostgreSQL. Only a connection that waited
  more than a second in the pool is pinged, and replaced when the ping fails.
  If PostgreSQL closes a connection used less than a second ago, the next
  statement on it fails once and the pool drops the connection. The test
  harness uses the same check.
- `0` in a `PGPOOL_*` variable means what it means to node-postgres:
  `PGPOOL_IDLE_TIMEOUT_MS=0` and `PGPOOL_CONNECTION_TIMEOUT_MS=0` disable the
  timeout, and `PGPOOL_MAX=0` is the default size. Before, the first closed
  every connection as soon as it was released, the second kept the pool from
  connecting, and the third stopped the process with a panic.
- A worker with runs waiting is no longer held back by its poll interval or by
  the length of the queue. A run that finishes wakes the worker, which used to
  wait a full poll interval while its slots were taken: 5 runs per second
  with the default settings, whatever the flow did. It claims as many runs as
  it has free slots in one statement, through a partial index, so a claim
  costs the same with a few runs waiting or with 80,000.
- Runs past their deadline are failed once per poll interval, also while
  every slot of the worker is taken, instead of before each claim. A claim
  skips them, so none starts late.
- The framework schema has a second migration, `openshop_queue_indexes`: the
  two partial indexes on `flow_runs` that workers read the queue through.
  `migrate generate` adds it to an existing project, and `migrate check` fails
  until it is there. `dev` creates the indexes on start.
- A run writes its log lines in fewer statements. A line waits up to 50 ms for
  the next ones and they are inserted together, each with the time it was
  logged; `flush()` and the end of the flow do not wait. The lines a flow
  logged are now stored before the outcome of its run. A flow of three steps
  goes from 21 statements to 15: a busy worker completes about a third more
  runs per second and uses a fifth less CPU per run.
- An open connection no longer holds its own copy of the route table, which
  axum made for every connection it accepted. With the routes of the demo, a
  connection goes from 62 KB to 19 KB: 43 MB less with 1,000 connections open.
- A worker claims a run at most `max_attempts` times. A run whose lease
  expired after its last attempt, because its worker died before recording the
  outcome (out of memory, abort), is marked `failed` with a `Run abandoned`
  error and reported, instead of being claimed again for ever. A run without a
  retry policy, such as one requeued from the Admin API, has one attempt.
- Waking up from `ctx.step.sleep` no longer uses an attempt: a flow can sleep
  as often as it needs to, also with `max_attempts(1)`, and a failure after a
  sleep is retried as far as the policy says. Breaking: `attempts` of a run
  that slept is lower than before, and the steps run after a sleep belong to
  the attempt that slept. No schema change.
- Only the last claim of a run can record its outcome. Each claim sets a new
  `lease_id` on the run, and the write that ends an attempt, the heartbeat and
  the failure of a run only apply while the worker holds it. A slow execution
  whose lease expired, or whose run was canceled and retried, and which was
  claimed again by another worker or by the same one, can no longer finish,
  sleep, fail or cancel it, and stops at its next heartbeat. It no longer takes
  the cancellation token of the new execution with it either. The framework
  schema has a migration for it, `openshop_run_lease`, one nullable column on
  `flow_runs` that the TypeScript runtime ignores: run `migrate generate` then
  `migrate` before starting the new worker (`migrate check` fails until it is
  there; `dev` adds the column on start).
- A transient database error no longer fails a run. The write that records the
  outcome of a run (completed, sleeping, canceled, retry or failure), the one
  that reads its retry policy, the failure of a run, and the writes of a step
  are tried up to five times, 100 ms apart and doubling, when the error comes
  from the connection or the load of the database: I/O, TLS, pool timeout,
  SQLSTATE 08, 53, 57P01-03, 40001 and 40P01. Each try keeps the ownership
  check, and each failure is logged. When the outcome still cannot be written,
  the worker leaves the run `running` and stops its heartbeat instead of
  marking it `failed` through the same database: the lease expires and the
  run is claimed again, its completed steps replayed from their checkpoints.
- The writes of a step carry the worker and the lease of the claim as well:
  an execution whose run was taken over, by another worker or by the same one,
  can no longer store the result of a step, start one or suspend at a sleep.
  The step fails with the new `engine::LeaseLostError`, stops the flow, and
  the run is left to the new execution. Breaking: `StepExecutor::new` takes
  the owner (`engine::RunOwner`, `None` outside a worker) as a last argument,
  and a stale step is no longer recorded as `canceled` or `failed`.
  The write locks the row of the run until it ends, so a claim cannot slip in
  between the check and the write of a step that waits for its own row. A
  step or a sleep that starts is inserted under an ID chosen before the first
  try: when a try commits and its answer is lost, the next one recognizes its
  row instead of failing on the unique key and failing the run.
- A lease, a deadline, a retry and the wake-up of a sleeping run are dated by
  the clock of the database (`now()` in SQL), no longer by the one of the
  process that writes them. A replica whose clock was ahead made leases that
  the others saw already expired and took the runs over, or leases that
  outlived their worker. Breaking, only in timing: `compute_next_retry_at`
  takes the date it counts from as a last argument.
- The heartbeat that extends a lease no longer fails silently. It has a time
  limit of a quarter of the lease (250 ms to 10 s), for the wait for a
  connection of a busy pool as well as for the statement, which the server
  enforces with `statement_timeout`. A failure is logged as `Heartbeat of run
  … failed` (the first and every tenth in a row). The worker keeps a deadline for
  the lease, the moment it sent the last request that extended it (the claim for the
  first one, not the moment its answer came back; a run whose claim answered
  after that deadline is not started) plus `lease_duration` less a tenth, and it runs while the worker waits for a tick
  as well as for a heartbeat stuck on a saturated pool or a lock. When it
  passes, the lease has expired: the flow is dropped
  (not only told to stop through its signal) and nothing is recorded, so a run
  another worker may have claimed is not run by two at once; the same when the
  run turns out to be owned by another execution. The step that was running is
  left as it is, as it is not this execution's to write. `RunFlowOptions` has two new fields, `lease_duration` and
  `interrupt`.
- A worker that stops hands back the runs it still executes once its wait of
  one lease is over: they go back to `pending`, claimable at once, instead of
  staying `running` until the lease expires. Their flows are dropped first,
  without recording an outcome, and a run is only released once its flow is
  gone, so that nothing of it runs beside the next owner. The release waits at
  most two seconds for a connection and a lock, the releases run together, and
  one that fails is logged and left to its lease without holding back the
  shutdown or the others. The release is fenced by worker and
  claim (`lease_id`, which it clears) like every write of an execution. The
  interrupted attempt stays counted in `attempts`, which still keys its step
  rows, so the run has one retry fewer; it is not failed as abandoned for that, and it runs again even with
  `max_attempts(1)`.
- Retries wait between half of the backoff and all of it, drawn at random,
  instead of exactly the backoff, so that runs that failed together, because
  a provider went down, do not all retry at the same moment. `max_interval`
  still caps the delay.
- The documentation of steps no longer says that a step is never executed
  twice. Steps run at least once: a step whose result is stored is not run
  again, but a process that dies before the result is stored, or a lease that
  expires while the step runs, makes the next attempt run it again, side
  effects included.

## 0.0.12-beta.0

Initial Rust implementation, at feature parity with OpenShop (TypeScript)
`0.0.12-beta.0`.

- Flow engine: dispatch with per-shop concurrency control, checkpointed steps,
  durable sleeps, retries with exponential backoff, deadlines, leased workers
  and a cron scheduler.
- Server: Shopify OAuth and session tokens, multi-app support, the Admin API,
  webhooks, app proxy and extension routes, public server routes, Shopify
  Function instances, custom admin page loaders and actions, and the MCP endpoint
  with scoped tokens and audit logs.
- Admin UI compiled to WebAssembly and embedded in the binary.
- SQL migrations owned by the application, with the framework schema generated
  into `./migrations`.
- `openshop` CLI (`init`, `init-ui`, `dev`) and a testing harness with isolated
  databases, fake providers and signed requests.
