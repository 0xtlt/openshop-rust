# Testing And Verification

Use this reference to choose checks after OpenShop changes.

## Baseline

Run the workspace checks after Rust changes:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

These are the checks CI runs for the main workspace: the framework, the shared
crate, the CLI, and the templates of `openshop init`.

## Database

Integration tests need PostgreSQL 17. From the repo root, `docker compose up -d`
starts one on `localhost:5432` with the `openshop` role and database.

- The harness connects with `TEST_DATABASE_URL`, then `DATABASE_URL` (only when it points at this machine), then `postgresql://openshop:openshop@localhost:5432/openshop`.
- Each test creates its own PostgreSQL schema, pushes the framework tables into it, and drops it afterwards. Tests run in parallel and never touch the `public` schema.
- Every test holds a small connection pool. CI runs `cargo test --workspace -- --test-threads=4` to stay under the server's connection limit; do the same if a local run exhausts connections.

## Package Tests

From the repo root:

```bash
cargo test -p openshop --lib
cargo test -p openshop --test it engine_runner::
cargo test -p openshop-template-custom
cargo test -p openshop-template-public
cargo test -p openshop-cli
```

`--lib` runs the unit tests inside `crates/openshop/src`, which do not need a
database. The integration tests are one binary, `crates/openshop/tests/it`,
with a module per subject: `--test it <module>::` runs one of them. A test that
reads the process logger does it with `common::LogCapture`, which takes turns
with the other tests of the binary. Only the Sentry tests (`sentry*.rs`) and
the `trybuild` tests of the macros (`--test macros`) are binaries of their own.

Use targeted tests when the change is narrow:

- Engine behavior: `crates/openshop/tests/it/engine_*.rs` (dispatch, runner, step, worker, scheduler) and the unit tests in `crates/openshop/src/engine`.
- Server, Admin API, auth, security, proxy, routes, and webhooks: `api.rs`, `auth.rs`, `security.rs`, `proxy_routes.rs`, `server_routes.rs`, `server_limits.rs`, `webhooks.rs`, `pages.rs`, `static_ui.rs`, `functions.rs`, `mcp.rs`, and `custom_admin_pages.rs` in `crates/openshop/tests/it`.
- Declared blocks (`#[flow]`, `#[webhook]`, `#[proxy]`, `#[route]`, `#[admin_page]`, `#[mcp_tool]`, `#[mcp_resource]`, `#[derive(FunctionConfig)]`), `app!` and what the builder refuses: `crates/openshop/tests/it/declared.rs`, and the unit tests of the macros in `crates/openshop-macros/src`.
- What a context of a shop gives and what takes one (`dispatch`, `Model::of`), the typed inputs of proxies and routes, Liquid templates, and the input of a cron: `crates/openshop/tests/it/contexts.rs`.
- Migrations: `crates/openshop/tests/it/migrations.rs` and the unit tests in `crates/openshop/src/db/migrate.rs`.
- CLI behavior (`init`, `dev` watcher, restarts): unit tests in `crates/openshop-cli/src`.
- The paths and modes of the admin pages: unit tests in `crates/openshop/src/admin_paths`.

## Admin UI

The admin is rendered by the server and tested with the workspace: the
framework pages and the custom admin pages (rendering, forms, `FormState`,
`Outcome`, the bounce) in `crates/openshop/tests/it/server_pages`. Check a page
you change in a browser against lsf's admin as well: a test does not see a
script error of `openshop.js`.

## Demo Apps

For changes affecting app usage, flows, providers, app proxy, or template conventions, check `templates/custom/tests` and run `cargo test -p openshop-template-custom`. `templates/custom/tests` shows the tests of an application in their recommended form, and `templates/custom/tests/common/mod.rs` how to answer Shopify GraphQL with fixtures (`answer_shopify(ctx)`).

For changes affecting what a merchant reaches in an admin closed to the built-in pages, the home of the admin, the records a page reads, or the uninstall webhook, check `templates/public/tests` and run `cargo test -p openshop-template-public`.

## Test Style

- Prefer assertions against behavior and public contracts over implementation details.
- Add regression tests for bugs in retries, step persistence, scheduling, auth, signatures, migrations, or generated templates.
- Keep integration tests for behavior that depends on DB state, HTTP requests, Shopify signatures/JWTs, worker execution, or generated app structure.
- Use the harness in `openshop::testing` (feature `testing`): `TestContext` starts the app on a random port against an isolated schema, `ctx.fakes` replaces provider methods with recording fakes, `ctx.transport` (`MockTransport`) scripts Shopify HTTP responses, `ctx.proxy()` signs app proxy requests, and `ctx.get(..)` / `ctx.post(..)` send requests with a session token.
- Test an application through its declaration, with `#[openshop::test(APP)]` on `async fn(ctx: &TestContext)`: it starts what `app!` lists on an isolated database with its shop installed, and shuts the context down when the function returns. Do not write `#[tokio::test]`, `TestContext::with_options(..)` or `ctx.shutdown().await` in such a test. The function returns `()` or a `Result`. Put `#[ignore]` and the other attributes of the test below `#[openshop::test(APP)]`.
- Write the attribute with its path: `openshop::prelude` does not have `test`, which would hide `#[test]`.
- Build a small test-only configuration with `OpenShopConfig::builder()`: `.add::<Block>()` for a declared block (`.add::<Warehouse>().add::<sync_orders>()`), the builder types (`Flow::new(..)`, `Provider::new(..)`, `Route::public()`…) for blocks made up in the test, and `APP.builder()?` to add to the whole application. `build()` refuses a key registered twice. Start it from the attribute too: its argument is an application or any expression that resolves to a `TestContext` or a `Result` of one, so write `#[openshop::test(context())]` with a function of the test file that returns the context, or `#[openshop::test(TestContext::with_options(config(), TestOptions::installed()))]`, where `installed()` is the options of a shop with an access token. Start a context by hand (`TestContext::new(config).await`, `TestContext::for_app(&APP)`) only when it depends on values the test makes itself, such as a recorder its configuration captures, and end that test with `ctx.shutdown().await` so the test schema is dropped. A test context is a `ShopScope` for its shop: `Review::of(ctx)` and `sync_orders::dispatch(ctx, input)` take it.
- Name a block by its constant: `ctx.run_flow(sync_orders::NAME, json!({ … }))`, `ctx.fakes.for_call(Warehouse::PUSH)`, `ReviewsPage::PATH`.
- Fake a provider method with its typed fake, `ctx.fakes.for_call(Warehouse::PUSH)`: `.returns(value)` takes the type the method returns, and `.last_call().unwrap().args` and `.calls()` give the arguments as the method receives them (a `Vec<WarehouseRecord>` for `push(records: Vec<WarehouseRecord>)`). It also has `.called()`, `.call_count()`, `.rejects(..)` and `.reset()`. Keep `ctx.fakes.method("warehouse", "push")`, with JSON arguments and results, for a method only known by name.
- Give the input of a run the names it has outside Rust: an `#[input]` type reads `json!({ "productId": "…" })` for its field `product_id`.
- Read the rows of a model in a test with `Review::for_shop(&ctx.db, &ctx.shop)`: a `TestContext` is not the context of a handler, so `Review::of` does not take it.
- Test admin loaders and actions with `admin_context` and `invoke_admin_function` on `config.admin_pages[..].functions["listReviews"]`; a function is keyed by its name in camel case, or by the name given to `#[loader]` or `#[action]`.
- Assert on `RunOutcome` returned by `ctx.run_flow(..)` instead of reading run rows when the outcome is enough.
