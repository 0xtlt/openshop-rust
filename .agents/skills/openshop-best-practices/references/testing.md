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
crate, the CLI, and the demo apps.

## Database

Integration tests need PostgreSQL 17. From the repo root, `docker compose up -d`
starts one on `localhost:5432` with the `openshop` role and database.

- The harness connects with `TEST_DATABASE_URL`, then `DATABASE_URL`, then `postgresql://openshop:openshop@localhost:5432/openshop`.
- Each test creates its own PostgreSQL schema, pushes the framework tables into it, and drops it afterwards. Tests run in parallel and never touch the `public` schema.
- Every test holds a small connection pool. CI runs `cargo test --workspace -- --test-threads=4` to stay under the server's connection limit; do the same if a local run exhausts connections.

## Package Tests

From the repo root:

```bash
cargo test -p openshop --lib
cargo test -p openshop --test engine_runner
cargo test -p openshop-demo-custom
cargo test -p openshop-demo-public
cargo test -p openshop-cli
```

`--lib` runs the unit tests inside `crates/openshop/src`, which do not need a
database. `--test <name>` runs one file of `crates/openshop/tests`.

Use targeted tests when the change is narrow:

- Engine behavior: `crates/openshop/tests/engine_*.rs` (dispatch, runner, step, worker, scheduler) and the unit tests in `crates/openshop/src/engine`.
- Server, Admin API, auth, security, proxy, routes, and webhooks: `api.rs`, `auth.rs`, `security.rs`, `proxy_routes.rs`, `server_routes.rs`, `server_limits.rs`, `webhooks.rs`, `pages.rs`, `static_ui.rs`, `functions.rs`, `mcp.rs`, and `custom_admin_pages.rs` in `crates/openshop/tests`.
- Declared blocks (`#[flow]`, `#[webhook]`, `#[proxy]`, `#[route]`, `#[admin_page]`, `#[mcp_tool]`, `#[mcp_resource]`, `#[derive(FunctionConfig)]`), `app!` and what the builder refuses: `crates/openshop/tests/declared.rs`, and the unit tests of the macros in `crates/openshop-macros/src`.
- What a context of a shop gives and what takes one (`dispatch`, `Model::of`), the typed inputs of proxies and routes, Liquid templates, and the input of a cron: `crates/openshop/tests/contexts.rs`.
- Migrations: `crates/openshop/tests/migrations.rs` and the unit tests in `crates/openshop/src/db/migrate.rs`.
- CLI behavior (`init`, `dev` watcher, restarts): unit tests in `crates/openshop-cli/src`.
- Shared rules: unit tests in `crates/openshop-shared/src`.

## Admin UI

The UI is its own workspace and has its own checks:

```bash
cd crates/openshop-ui
cargo fmt --check
cargo clippy --target wasm32-unknown-unknown -- -D warnings
cargo test
trunk build --release
```

`trunk build --release` writes `crates/openshop/ui-dist`. Commit that directory
when the UI changes.

The views of custom admin pages (`#[view]`, `openshop::ui`) are tested in
`crates/openshop/tests/admin_views.rs`, and what the bundle of an application
is made of in the unit tests of `crates/openshop-cli/src/bundle.rs`. Each demo
has views: `mise run demo:ui` builds their bundles.

## Demo Apps

For changes affecting app usage, flows, providers, app proxy, or demo conventions, check `apps/demo-custom/tests` and run `cargo test -p openshop-demo-custom`. `apps/demo-custom/tests` shows the tests of an application in their recommended form, and `apps/demo-custom/tests/common/mod.rs` how to answer Shopify GraphQL with fixtures (`answer_shopify(ctx)`).

For changes affecting what a merchant reaches in an admin closed to the built-in pages, the home of the admin, the records a page reads, or the uninstall webhook, check `apps/demo-public/tests` and run `cargo test -p openshop-demo-public`.

## Test Style

- Prefer assertions against behavior and public contracts over implementation details.
- Add regression tests for bugs in retries, step persistence, scheduling, auth, signatures, migrations, or generated templates.
- Keep integration tests for behavior that depends on DB state, HTTP requests, Shopify signatures/JWTs, worker execution, or generated app structure.
- Use the harness in `openshop::testing` (feature `testing`): `TestContext` starts the app on a random port against an isolated schema, `ctx.fakes` replaces provider methods with recording fakes, `ctx.transport` (`MockTransport`) scripts Shopify HTTP responses, `ctx.proxy()` signs app proxy requests, and `ctx.get(..)` / `ctx.post(..)` send requests with a session token.
- Test an application through its declaration, with `#[openshop::test(APP)]` on `async fn(ctx: &TestContext)`: it starts what `app!` lists on an isolated database with its shop installed, and shuts the context down when the function returns. Do not write `#[tokio::test]`, `TestContext::with_options(..)` or `ctx.shutdown().await` in such a test. Take `&mut TestContext` to create resources with a factory (`Factory::new(create, destroy)`, `ctx.create(..)`). The function returns `()` or a `Result`. Put `#[ignore]` and the other attributes of the test below `#[openshop::test(APP)]`.
- Write the attribute with its path: `openshop::prelude` does not have `test`, which would hide `#[test]`.
- Build a small test-only configuration with `OpenShopConfig::builder()`: `.add::<Block>()` for a declared block (`.add::<Warehouse>().add::<sync_orders>()`), the builder types (`Flow::new(..)`, `Provider::new(..)`, `Route::public()`…) for blocks made up in the test, and `APP.builder()?` to add to the whole application. `build()` refuses a key registered twice. Start it from the attribute too: its argument is an application or any expression that resolves to a `TestContext` or a `Result` of one, so write `#[openshop::test(context())]` with a function of the test file that returns the context, or `#[openshop::test(TestContext::with_options(config(), TestOptions::installed()))]`, where `installed()` is the options of a shop with an access token. Start a context by hand (`TestContext::new(config).await`, `TestContext::for_app(&APP)`, `APP.test()`) only when it depends on values the test makes itself, such as a recorder its configuration captures, and end that test with `ctx.shutdown().await` so the test schema is dropped. A test context is a `ShopScope` for its shop: `Review::of(ctx)` and `sync_orders::dispatch(ctx, input)` take it.
- Name a block by its constant: `ctx.run_flow(sync_orders::NAME, json!({ … }))`, `ctx.fakes.for_call(Warehouse::PUSH)`, `ReviewsPage::PATH`.
- Fake a provider method with its typed fake, `ctx.fakes.for_call(Warehouse::PUSH)`: `.returns(value)` takes the type the method returns, and `.last_call().unwrap().args` and `.calls()` give the arguments as the method receives them (a `Vec<WarehouseRecord>` for `push(records: Vec<WarehouseRecord>)`). It also has `.called()`, `.call_count()`, `.rejects(..)`, `.on_call_returns(n, value)`, `.on_call_rejects(n, ..)`, `.reset()`, and `.untyped()` for the raw `FakeMethod`. Keep `ctx.fakes.method("warehouse", "push")`, with JSON arguments and results, for a method only known by name.
- Give the input of a run the names it has outside Rust: an `#[input]` type reads `json!({ "productId": "…" })` for its field `product_id`.
- Read the rows of a model in a test with `Review::for_shop(&ctx.db, &ctx.shop)`: a `TestContext` is not the context of a handler, so `Review::of` does not take it.
- Test admin loaders and actions with `admin_context` and `invoke_admin_function` on `config.admin_pages[..].functions["listReviews"]`; a function is keyed by its name in camel case, or by the name given to `#[loader]` or `#[action]`.
- Assert on `RunOutcome` returned by `ctx.run_flow(..)` instead of reading run rows when the outcome is enough.
