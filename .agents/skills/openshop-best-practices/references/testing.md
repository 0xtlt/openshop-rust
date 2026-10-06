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
crate, the CLI, and the demo app.

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
cargo test -p openshop-demo
cargo test -p openshop-cli
```

`--lib` runs the unit tests inside `crates/openshop/src`, which do not need a
database. `--test <name>` runs one file of `crates/openshop/tests`.

Use targeted tests when the change is narrow:

- Engine behavior: `crates/openshop/tests/engine_*.rs` (dispatch, runner, step, worker, scheduler) and the unit tests in `crates/openshop/src/engine`.
- Server, Admin API, auth, security, proxy, routes, and webhooks: `api.rs`, `auth.rs`, `security.rs`, `proxy_routes.rs`, `server_routes.rs`, `server_limits.rs`, `webhooks.rs`, `pages.rs`, `static_ui.rs`, `functions.rs`, `mcp.rs`, and `custom_admin_pages.rs` in `crates/openshop/tests`.
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

## Demo App

For changes affecting app usage, flows, providers, app proxy, or demo conventions, check `apps/demo/tests` and run `cargo test -p openshop-demo`. `apps/demo/tests/common/mod.rs` shows how to boot an application with Shopify GraphQL answered by fixtures.

## Test Style

- Prefer assertions against behavior and public contracts over implementation details.
- Add regression tests for bugs in retries, step persistence, scheduling, auth, signatures, migrations, or generated templates.
- Keep integration tests for behavior that depends on DB state, HTTP requests, Shopify signatures/JWTs, worker execution, or generated app structure.
- Use the harness in `openshop::testing` (feature `testing`): `TestContext` starts the app on a random port against an isolated schema, `ctx.fakes` replaces provider methods with recording fakes, `ctx.transport` (`MockTransport`) scripts Shopify HTTP responses, `ctx.proxy()` signs app proxy requests, and `ctx.get(..)` / `ctx.post(..)` send requests with a session token.
- End each test with `ctx.shutdown().await` so the test schema is dropped.
- Assert on `RunFlowResult` returned by `ctx.run_flow(..)` instead of reading run rows when the outcome is enough.
