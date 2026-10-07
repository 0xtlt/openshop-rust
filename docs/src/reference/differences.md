# Differences from the TypeScript version

The Rust implementation follows the TypeScript framework's behavior: the same
database tables, HTTP routes, payloads, encryption formats and admin screens.
This page lists every place where it intentionally differs.

## How an application is assembled

| TypeScript | Rust |
| --- | --- |
| `openshop.config.ts` loaded by the `openshop` CLI | Your binary builds an `OpenShopConfig` and calls `openshop::cli::run` |
| `proxy/`, `routes/` and `admin/pages/` discovered from files | `.proxy(path, …)`, `.route(path, …)` and `.admin_page(…)` on the builder |
| Hot reload of the config module in `dev` | `openshop dev` rebuilds and restarts the process |
| `process.env` read on each request | Environment captured once in `RuntimeEnv` at startup |
| `getDb()` and other process globals | An `OpenShop` handle (`ctx.app`, `ctx.db`) passed to every handler |
| `arktype` schemas | `serde` types with a `schemars` JSON Schema, and validator closures |

Consequences:

- Shopify apps are resolved once at startup. A missing Shopify TOML file or
  missing `client_id` stops the process instead of failing each request.
- Invalid definitions are rejected when the configuration is built: ambiguous
  proxy, route or admin page paths, a route without handlers, navigation that
  points to an unknown page, an invalid cron expression.
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
  configuration form, and `#[provider("name")]` makes the methods of its `impl`
  block the methods of the provider, called with
  `Warehouse::on(&ctx.connectors).push(&orders)`. The compiler checks the
  method, its arguments and its result. See [Providers](providers.md).
- Underneath, `ProviderCall<A, R>` constants with `Provider::handle`,
  `Provider::typed_checker` and `Connectors::invoke`, for a provider built
  without a struct. See [Typed calls](providers.md#typed-calls).
- `.providers([…])`, `.flows([…])` and `.with(register)` on the builder.

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
  fully persisted when a run finishes.
- A worker keeps polling when claiming a run fails, for example during a
  database outage.
- A worker with every slot taken claims again as soon as a run finishes, and
  claims as many runs as it has free slots in one statement. The TypeScript
  engine waits a poll interval when its slots are full and claims one run per
  statement, which caps a worker at `concurrency` runs per poll interval.
- Claiming reads the queue through a partial index, so its cost does not grow
  with the number of runs waiting.
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
  six-field expressions.
- Error messages stored on runs and steps include the context chain
  (`while syncing: connection refused`).

## Admin API

- An identifier that is not a UUID returns `404` (`Run not found`,
  `Token not found`) instead of a database error.
- `POST /api/crons/toggle` requires `key` (string) and `enabled` (boolean) and
  answers `400` otherwise.
- `POST /api/runs/bulk-delete` answers `400 No IDs provided` for a missing or
  invalid body; identifiers that are not UUIDs count as skipped.
- Log export uses JSON for any `format` other than `csv`, and only letters and
  digits are kept in the file extension.
- Run details list steps in creation order.
- Unknown paths under `/api` return `404` after authentication.
- Request bodies are limited to 2 MB (`413` above that). Set
  `max_request_body_bytes` on the configuration builder to change the limit.
  The MCP endpoint keeps its own 1 MB limit.
- Responses of 1 KB or more are compressed (gzip or Brotli) when the client
  accepts it.
- With custom admin pages, a static page takes precedence over a dynamic
  sibling: `/reviews/new` is handled by its own page rather than rejected as
  ambiguous with `/reviews/:id`.

## Security

- The Admin API only accepts session tokens issued to a staff user. A token
  whose `sub` is empty or a customer (`gid://shopify/Customer/…`) is rejected
  with `401`, even though it is signed with the same secret. Customer account
  tokens remain valid on `/ext` and `/proxy`.
- A blank `SHOPIFY_API_SECRET` (whitespace only) counts as not configured
  everywhere, including session token verification.
- `POST /auth/webhooks/app-uninstalled` takes the shop from the signed payload
  (`myshopify_domain`). The unsigned `X-Shopify-Shop-Domain` header is only used
  when the payload has none.
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
  serves its own bundle with `.admin_ui(UiSource::Dir(…))`,
  `.admin_ui(UiSource::assets(…))` for a bundle compiled into the binary, or
  the `OPENSHOP_UI_DIR` environment variable.
- Assets whose file name carries a content hash are served with
  `Cache-Control: public, max-age=31536000, immutable`; other files with
  `no-cache`.
- Custom admin pages are Rust components registered in your own UI bundle
  instead of `page.tsx` files compiled by Vite.
- In `dev`, the shell is served by the application itself (there is no separate
  UI port) and without the signed-query check.

## Database and migrations

- Migrations are SQL files in `./migrations`, applied in file-name order and
  recorded in the `__openshop_migrations` table. There is no Drizzle journal.
- A model is a struct with `#[derive(Model)]` instead of a Drizzle table. Its
  typed rows cover insertion, lookup, filters on columns, ordering, update and
  deletion, limited to a shop; there is no general query builder, and other
  queries are SQL strings run with `sqlx`, which can check them at compile
  time with `sqlx::query!`.
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

## Environment

- `OPENSHOP_ENV=production` enables production mode (`NODE_ENV=production` is
  honored too): `ENCRYPTION_KEY` is required and the schema push is refused.
- `OPENSHOP_UI_DIR` replaces the embedded admin UI.

## Not ported

- The ESLint preset and the Vite plugins.
- GraphQL code generation as a separate step. `graphql!` checks an operation
  against Shopify's schema and types it when the crate compiles, so there is
  no `.graphqlrc.ts`, no generated file and no watch mode, and `dev` has
  nothing to generate. `openshop codegen` only downloads the schema of another
  API version. Enum values are strings, and the types of an inline operation
  have no name. See [Typed GraphQL](graphql.md).
- `openshop build` and `openshop test` map to `cargo build --release` and
  `cargo test`.
- The proxy test client's `.expect(schema)`; deserialize the response instead.
