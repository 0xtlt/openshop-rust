# Architecture

Use this reference before moving behavior across OpenShop boundaries.

## Repo Map

- `crates/openshop/src/engine`: flow execution, steps, scheduling, retries, aborts, and worker behavior.
- `crates/openshop/src/server`: axum server, Shopify auth, webhooks, app proxy, Admin API routes (`server/api`), functions, MCP (`server/mcp`), crypto, logs, and shop isolation.
- `crates/openshop/src/cli`: commands of an application binary: `dev`, `start`, `worker`, and `migrate`.
- `crates/openshop/src/db`: connection pool, framework row types, models (`#[derive(Model)]`, `define_model`), typed rows (`record.rs`), the schema snapshot and its comparison (`diff.rs`), and migrations.
- `crates/openshop/src/admin`: server side of custom admin pages (access rules, loaders, actions).
- `crates/openshop/src/config/validate.rs`: validation of an application definition.
- `crates/openshop/src/runtime`: environment (`RuntimeEnv`), logging, and the outbound HTTP transport.
- `crates/openshop/src/shopify`: Shopify Admin GraphQL client.
- `crates/openshop/src/testing`: test harness, compiled with the `testing` feature.
- `crates/openshop/src/types.rs`: the definitions an application registers (providers, flows, crons, webhooks, proxies, routes, functions, MCP).
- `crates/openshop/migrations`: framework schema SQL, embedded in the crate.
- `crates/openshop/ui-dist`: prebuilt admin UI bundle, committed and embedded in the crate.
- `crates/openshop-shared`: types and rules shared by the server and the WebAssembly UI.
- `crates/openshop-ui`: embedded admin UI written with Leptos and Shopify's Polaris web components, compiled to WebAssembly; its own workspace. Its library lets an application build a bundle with custom admin pages.
- `crates/openshop-cli`: the `openshop` command (`init`, `dev`); `templates/minimal` is the generated app template.
- `apps/demo`: runnable demo app and integration surface; update it when examples or framework usage change. `apps/demo/ui` is its admin UI bundle with a custom page, also its own workspace.
- `docs/src`: user-facing documentation (mdBook).

## Boundary Rules

- Keep reusable framework behavior in `crates/openshop/src`; keep sample usage in `apps/demo` and the template.
- Do not duplicate framework behavior in the template or the demo app when it can live in OpenShop.
- If a public API changes, update docs, the template, and demo usage in the same change.
- Prefer existing config validation in `crates/openshop/src/config/validate.rs` for definition checks. It runs in `OpenShopBuilder::build()` and again when an `OpenShop` is created.
- Preserve shop/app isolation by `(app_handle, shop)` in server and database behavior.
- Keep secrets in environment-backed config; never put API secrets in TOML examples.
- In framework code, read environment-backed settings from `app.env()` (`RuntimeEnv`). It is captured once at startup so that requests never read process-global state.
- Template files are embedded with `include_str!` in `crates/openshop-cli/src/init.rs`. Add a new template file to its `TEMPLATE` list.
- Framework schema changes are SQL files in `crates/openshop/migrations`, listed in `FRAMEWORK_MIGRATIONS` (`crates/openshop/src/db/migrate.rs`). Keep them idempotent: `dev` and the test harness execute them on every start.

## Shared App Code

- Follow the layout of `apps/demo/src`: one module per definition under `flows/`, `providers/`, `webhooks/`, `proxy/`, `routes/`, `admin/`, `functions/`, and `models/`, each exposing a function that returns the definition, all registered in a single `config()` function.
- Nothing is discovered from the file system. A proxy, route, webhook, or admin page exists only once it is registered on the builder with `.proxy(..)`, `.route(..)`, `.webhook(..)`, or `.admin_page(..)`.
- Keep proxy and server route handlers as thin adapters for auth checks, params, body parsing, and response shaping.
- Put reusable business logic and Admin GraphQL operations in ordinary modules whose functions take what they need (`&ShopifyClient`, `&PgPool`, `&Connectors`), so flows, webhooks, and routes can share them.
- Outside a flow, get the shop's client and connectors from the application handle: `ctx.app.shopify_client(&ctx.shop, &ctx.shopify_app)` and `ctx.app.connectors(&ctx.shop, &ctx.shopify_app)`.

## Flow And Provider Rules

- Step results must implement `Serialize` and `DeserializeOwned` and round-trip through JSON, because completed step output is persisted and reused on retry.
- Propagate the result of `ctx.step.run(..)` and `ctx.step.sleep(..)` with `?`. Sleeping and cancellation travel as errors (`SleepSignal`, `FlowCanceledError`) that the runner must receive.
- Give every step a name that is stable across deployments and unique within a run.
- A canceled or timed-out step is dropped at its next `.await`. Use `ctx.signal` (a `CancellationToken`) in work that does not yield or that you spawn.
- Keep provider and Function fields validated with `openshop::validators` or a closure passed to `FieldDef::validate`.
- Describe an application table with a `#[derive(Model)]` struct registered with `.model_of::<T>()`, and read and write it through `T::for_shop(&ctx.db, &ctx.shop)`: every operation is then limited to the shop. Use `all_shops` only for maintenance work, and filter on the app handle yourself when several apps share a table. After changing a model, run `cargo run -- migrate generate`, review the SQL and its warnings, declare renames with `#[model(renamed_from = "…")]`, and commit the migration with `migrations/schema.snapshot.json`. Link two models with `#[model(references(Other), on_delete = "…")]` on the id field and index that field; read the other side with a filter, or with a join written in SQL (`sqlx::query!` checks it at compile time when the project depends on `sqlx` with the `macros` feature and a database is reachable while compiling).
- Prefer typed provider calls: declare a `ProviderCall<A, R>` constant next to the provider, implement it with `Provider::handle(CALL, |config: MyConfig, args| ..)` and call it with `ctx.connectors.invoke(CALL, &args)`. The compiler then checks the method name, the arguments and the result. `typed_method` with `ctx.connectors.call(..)` / `call_as::<T>(..)` remains for calls whose name is only known at run time.

## UI Rules

- Match the existing compact admin UI instead of adding marketing-style layouts.
- Use Shopify's Polaris web components (`s-page`, `s-section`, `s-table`, …) where the surrounding UI already uses them.
- Keep Admin API calls routed through the fetch helpers in `crates/openshop-ui/src/fetch.rs`, which attach the session token.
- Put types and rules needed by both the server and the UI in `crates/openshop-shared`.
- After changing `crates/openshop-ui`, rebuild with `trunk build --release` and commit `crates/openshop/ui-dist`. The server embeds that directory, so rebuild the server afterwards.
- Keep the `shopify-api-key` meta tag and the App Bridge script tag of `crates/openshop-ui/index.html` as they are: the server rewrites them when it serves the shell.

## Customer Account Extensions

- Customer Account UI extensions should call OpenShop proxy handlers through `/ext/*` with `Authorization: Bearer <session token>`.
- Extension-backed proxy handlers should trust `ctx.shop`, `ctx.shopify_app`, and `ctx.customer_id`; do not trust customer IDs supplied by query strings or request bodies.
- Use `ctx.app.shopify_client(&ctx.shop, &ctx.shopify_app)` inside extension-backed proxy handlers so multi-app installs resolve the correct access token.
- For extension frontend code, keep backend origin handling explicit: Shopify CLI tunnel origin in dev, deployed app origin or extension settings in production.
