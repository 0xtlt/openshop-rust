# Differences from the TypeScript version

The Rust implementation follows the TypeScript framework's behavior: the same
database tables, HTTP routes, payloads, encryption formats and admin screens.
This page lists every place where it intentionally differs.

## How an application is assembled

| TypeScript | Rust |
| --- | --- |
| `openshop.config.ts` loaded by the `openshop` CLI | Your binary declares an `App` with `app!` and runs it with `APP.main()` |
| `defineFlow({ … })` and `defineProvider({ … })` returning a definition | An attribute on the function or the struct itself (`#[flow]`, `#[provider]`), which `app!` lists by name |
| `proxy/`, `routes/` and `admin/pages/` discovered from files | `#[proxy(GET, "/path")]`, `#[route(GET, "/path")]` and `#[admin_page("/path")]` on the code, listed in `app!` |
| Hot reload of the config module in `dev` | `openshop dev` rebuilds and restarts the process, and the admin UI reloads by itself |
| `process.env` read on each request | Environment captured once in `RuntimeEnv` at startup |
| `getDb()` and other process globals | An `OpenShop` handle (`ctx.app`, `ctx.db`) passed to every handler |
| `arktype` schemas | Types marked `#[input]`, read with `serde` and described by a `schemars` JSON Schema, and field validators |

Consequences:

- Shopify apps are resolved once at startup. A missing Shopify TOML file or
  missing `client_id` stops the process instead of failing each request.
- Invalid definitions are rejected when the configuration is built: ambiguous
  proxy, route or admin page paths, a route without handlers, navigation that
  points to an unknown page, an invalid cron expression, a block registered
  twice under the same key.
- The `input` of a cron is validated against the input of its flow when the
  configuration is built. A cron whose input the flow refuses stops the
  process at startup with
  `[openshop] Invalid config: crons[0].input is not an input of flow "syncOrders": …`,
  instead of producing a failed run at each occurrence.
- Custom admin pages are served as soon as they are listed in `admin_pages`:
  there is no separate switch to enable them. A page declares its entry in the
  menu with `nav = "Label"`, and the `admin_menu` setting sets the menu by
  hand. There is no `experimental` setting: `experimental.customPages.navigation`
  is `admin_menu`, and its errors name `adminMenu[0].path`.
- `GET /api/flows` and the MCP tool `openshop.flows.list` return `inputSchema`
  as JSON Schema (or `null`) rather than arktype's JSON format. Validation
  messages differ accordingly; they still start with
  `Invalid input for flow "<name>":`.
- Route parameters are written `:name`. The file-style `[name]` is accepted.

The TypeScript version infers `connectors.warehouse.push(data)` from the
provider definitions. Rust has no equivalent inference across a registry, so
connectors are reached by name. Two additions close most of that gap and make
larger configurations easier to split:

- A provider is a struct: `#[derive(ProviderConfig)]` makes its fields the
  configuration form, and `#[provider]` makes the methods of its `impl`
  block the methods of the provider, called with
  `Warehouse::on(&ctx.connectors).push(&orders)`. The compiler checks the
  method, its arguments and its result. See [Providers](providers.md).
- Underneath, `ProviderCall<A, R>` constants with `Provider::handle` and
  `Provider::typed_checker`, for a provider built without a struct. See
  [Typed calls](providers.md#typed-calls).
- Blocks are declared in the module that writes them and `app!` lists them by
  name, so a large application has no registration code to split.

## Engine

- A running step is interrupted as soon as its run is canceled, and is recorded
  as `canceled`. The TypeScript engine only checked between steps.
- Cancellation works across processes: when a worker's heartbeat finds that it
  no longer owns the run (canceled through the API, or the lease taken over),
  it cancels the flow.
- A flow that exceeds its `timeout` is stopped. In TypeScript the flow function
  kept running in the background after the run was marked failed.
- A panic inside a flow, a webhook, a proxy or route handler, a custom admin
  page loader, action or access rule, a provider checker, or the error hook is
  caught and treated as a failure of that unit of work.
- Log lines are written in emission order with their emission time, and are
  fully persisted when a run finishes. Lines logged within 50 ms of each other
  share one statement, and the lines a flow logged are stored before the
  outcome of its run.
- A worker keeps polling when claiming a run fails, for example during a
  database outage.
- A worker with every slot taken claims again as soon as a run finishes, and
  claims as many runs as it has free slots in one statement. The TypeScript
  engine waits a poll interval when its slots are full and claims one run per
  statement, which caps a worker at `concurrency` runs per poll interval.
- Claiming reads the queue through a partial index, so its cost does not grow
  with the number of runs waiting.
- The logs of a run are served by page (`limit`, `cursor`, `next`, and
  `levelCounts` beside `logs` and `total`) and the export is streamed. The
  `logs` table has indexes on its time and its level, and workers remove the
  lines past the `log_retention` setting when it is set. The columns are those
  of the TypeScript runtime, which keeps reading and writing the table.
- Runs past their deadline are failed once per poll interval, including while
  every slot is taken, instead of before each claim. A claim skips them, so
  none starts late.
- A worker extends its lease at least twice per lease period (every 15 seconds
  at most). The TypeScript engine always waited 15 seconds, so a lease shorter
  than that let another worker take over a run that was still executing.
- A worker only marks a run as failed while it still owns it.
- The `worker` command uses `worker.concurrency` from the configuration when
  `--concurrency` is absent; the original always fell back to 5. A value that
  is not a positive integer is an error.
- A step whose future is dropped before it finishes (flow timeout, panic) is
  recorded as `failed` with the error `Step was interrupted` instead of staying
  `running`.
- A run that completes after a failed attempt no longer keeps the error of that
  attempt; the logs still show it.
- Cron schedules accept nicknames such as `@daily` in addition to five- and
  six-field expressions. They are read in UTC, or in the IANA time zone given
  with `.timezone(…)`, and not in the time zone of the host. A process that
  wakes late fires the latest occurrence it missed, once.
- Error messages stored on runs and steps include the context chain
  (`while syncing: connection refused`).

## Admin API

- An identifier that is not a UUID returns `404` (`Run not found`,
  `Token not found`) instead of a database error.
- `POST /api/crons/toggle` requires `key` (string) and `enabled` (boolean) and
  answers `400` otherwise, and `404` (`Cron not found`) for a key that is not
  a cron the shop sees.
- A shop only sees the crons that apply to it: those that run once without a
  shop, those of every shop, and those that name it. `GET /api/crons`,
  `GET /api/flows` and the MCP tools `openshop.crons.list` and
  `openshop.flows.list` leave out the crons of other shops, and show only the
  shop of the caller among the shops a cron names.
- `POST /api/runs/bulk-delete` answers `400 No IDs provided` for a missing or
  invalid body; identifiers that are not UUIDs count as skipped.
- Retry, cancel and delete check the status of the run in the write itself, so
  a run that changed meanwhile is answered `409`. A retry of a flow that
  rejects concurrent runs answers `409` with `existingRunId` while another
  run of it is active for the shop.
- Log export uses JSON, in a `.json` file, for any `format` other than `csv`.
- Run details list steps in creation order.
- Unknown paths under `/api` return `404` after authentication.
- Request bodies are limited to 2 MB (`413` above that). Set
  `max_request_body_bytes` in `app!` to change the limit. The MCP endpoint
  keeps its own 1 MB limit.
- A request whose response has not begun after 60 seconds is answered with
  `503`, and one that comes while 1024 are being served with `503` and
  `Retry-After: 1`; `request_timeout` and `max_concurrent_requests` in `app!`
  change them. Verifying an MCP token runs at most a few scrypt hashes at
  once and remembers a right secret for five minutes; the stored hash is the
  same.
- Responses of 1 KB or more are compressed (gzip or Brotli) when the client
  accepts it.
- With custom admin pages, a static page takes precedence over a dynamic
  sibling: `/reviews/new` is handled by its own page rather than rejected as
  ambiguous with `/reviews/:id`.
- A custom admin page can be declared at `/`: it is the home of the admin,
  in place of the built-in dashboard, and its functions are called at
  `/api/pages/custom/_rpc/<kind>/<name>`. `AdminPagesConfig::none()` disables
  every built-in page, and such an application must declare that home.
- Turning MCP off, with `mcp: McpConfig::disabled()` or `.enabled(false)`,
  also disables the MCP page of the admin and `/api/mcp/*`, unless the
  application sets a mode for that page with the `builtin_pages` setting or
  `.builtin_page(AdminPageId::Mcp, …)`.

## Proxy and server routes

- A `#[proxy]` or `#[route]` function can take a typed input as its second
  argument, read from the query string or from the JSON body. A request that
  does not fit it is answered with `422` and
  `{ "error": "Invalid input: …" }` before the function runs. A function
  without a typed input reads the query and the body itself, and no request is
  refused for its shape.
- Liquid answers come from template files, embedded with `liquid!`. A value
  of the server is passed with `.assign(name, value)` and written before the
  template as a `{% assign %}` tag, escaped so that it cannot close its
  literal or its tag. A template is not built by interpolating values into a
  string.

## Security

- The Admin API only accepts session tokens issued to a staff user. A token
  whose `sub` is empty or a customer (`gid://shopify/Customer/…`) is rejected
  with `401`, even though it is signed with the same secret. Customer account
  tokens remain valid on `/ext` and `/proxy`.
- A blank `SHOPIFY_API_SECRET` (whitespace only) counts as not configured
  everywhere, including session token verification.
- Session tokens, launches, OAuth callbacks, app proxy requests and webhooks
  are verified by `shopify_api`: ten seconds of clock skew on a session token,
  whose `dest` and `iss` are exactly `https://<shop>.myshopify.com` and its
  `/admin`; a signed launch, callback or app proxy request of the last 90
  seconds; a webhook whose body is a JSON object, with its `X-Shopify-Topic`
  and `X-Shopify-Shop-Domain` headers.
- `POST /auth/webhooks/app-uninstalled` uninstalls the shop that both the
  signed payload (`myshopify_domain`) and the unsigned `X-Shopify-Shop-Domain`
  header name, and nothing when they differ.
- App proxy signatures are compared as strict hexadecimal. A valid signature
  followed by other characters is rejected.
- Static asset paths are decoded and checked segment by segment; a path that
  would leave the UI directory is answered with `404`.
- The admin UI shell is sent with
  `Content-Security-Policy: frame-ancestors https://<shop> https://admin.shopify.com;`
  and `Cache-Control: no-store`.
- MCP token hashing and verification run outside the async runtime.

## Admin UI

- The UI is written in Rust with Leptos and compiled to WebAssembly. It is
  embedded in the framework crate. An application with custom admin pages
  serves its own bundle with the setting `admin_ui: UiSource::Dir(…)`,
  `admin_ui: UiSource::Assets(…)` for a bundle compiled into the binary, or
  the `OPENSHOP_UI_DIR` environment variable.
- Assets whose file name carries a content hash are served with
  `Cache-Control: public, max-age=31536000, immutable`; other files with
  `no-cache`. They have an `ETag` and answer `304` to `If-None-Match`, and are
  sent with `X-Content-Type-Options: nosniff`.
- Custom admin pages are Rust components registered in your own UI bundle
  instead of `page.tsx` files compiled by Vite.
- In `dev`, the shell is served by the application itself (there is no separate
  UI port) and without the signed-query check. There is no Vite server and no
  module replacement: the shell reloads the whole page when the app has been
  started again or its bundle rebuilt, which `GET /__openshop/reload`, a route
  of `dev` only, tells it.
- A visitor without a valid launch receives a page written with Polaris web
  components, with the same `401` and the same sentence. `dev` answers its
  `403` with that page instead of plain text, and the admin UI shows it to a
  visitor without a session.
- The [operator dashboard](operator-dashboard.md) under `/ops` has no
  TypeScript counterpart: pages rendered by the server, without script, that
  show every shop over a period (runs, log lines, flows, shops, crons),
  read-only, behind the password the application gives with `ops_password`.
  It adds no table. `ops` is a reserved first segment for custom admin pages.
- [Sentry](sentry.md) reporting is part of the framework: with the `sentry`
  setting, failed runs, failed handlers, and panics are reported with their
  flow and their shop. It was not ported from the TypeScript version.

## Database and migrations

- Migrations are SQL files in `./migrations`, applied in file-name order and
  recorded in the `__openshop_migrations` table. There is no Drizzle journal.
- A model is a struct with `#[derive(Model)]` instead of a Drizzle table. Its
  typed rows cover insertion, lookup, filters on columns, ordering, update and
  deletion, limited to a shop; there is no general query builder, and other
  queries are SQL strings run with `sqlx`, which can check them at compile
  time with `sqlx::query!`.
- `Installation`, `FlowRun`, `StepResult`, and `LogRow` are read with typed
  columns, across shops or for the shop of a context, and never written
  through them. See
  [Read the framework tables](database.md#read-the-framework-tables).
- `#[model(references(Other))]` declares a foreign key, like `.references()`
  in Drizzle. There are no relational queries: the other side of a reference
  is read with a filter or a join.
- `migrate generate` compares the models with `migrations/schema.snapshot.json`
  and writes the difference, like `drizzle-kit generate` with its snapshots. It
  never asks questions: a rename is declared with `renamed_from`, and a
  statement that can lose data or fail is written with a warning.
- `migrate check` fails when the framework schema is not covered or when the
  models changed since the last generated migration.
- A migration that was modified after being applied is rejected.
- `dev` and the test harness create missing tables directly and add missing
  columns and indexes. They do not rename, retype or drop anything.
- The pool learns that PostgreSQL closed an idle connection when it uses it
  again, where `pg.Pool` drops the client as soon as its socket closes. A
  connection that waited more than a second is pinged first, and replaced when
  the ping fails; one reused sooner is not checked, and fails the statement it
  is given if it was closed. See
  [Idle connection check](database.md#idle-connection-check).
- A `PGPOOL_*` value that is not a non-negative integer falls back to its
  default instead of going through `Number()`. `0` keeps the meaning it has
  for `pg.Pool`: no timeout, and the default size for `PGPOOL_MAX`.
- While PostgreSQL refuses connections, a statement waits and the pool retries
  until `PGPOOL_CONNECTION_TIMEOUT_MS`, where `pg.Pool` fails at once. With
  the timeout disabled, it waits until PostgreSQL is back.

## Environment

- `OPENSHOP_ENV=production` enables production mode (`NODE_ENV=production` is
  honored too): the schema push is refused.
- `ENCRYPTION_KEY` is required unless `OPENSHOP_ENV` is
  `development` or `test` (`NODE_ENV` does not count), or the command is `dev`. The application does not
  start without it, in production or in any environment that is not explicitly
  a development one.
- `OPENSHOP_UI_DIR` replaces the embedded admin UI.

## Not ported

- The ESLint preset and the Vite plugins.
- GraphQL code generation as a separate step. `graphql!` checks an operation
  against Shopify's schema and types it when the crate compiles, so there is
  no `.graphqlrc.ts`, no generated file and no watch mode, and `dev` has
  nothing to generate. The first build downloads the schema of the version of
  `[package.metadata.shopify_api]`. Enum values are strings, and the types of
  an inline operation have no name. See [Typed GraphQL](graphql.md).
- `openshop build` and `openshop test` map to `cargo build --release` and
  `cargo test`.
- The proxy test client's `.expect(schema)`; deserialize the response instead.
