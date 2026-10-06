# Test an app

Use this guide for a generated app with the sample `syncOrders` flow and
`warehouse` provider. It uses Rust's built-in test runner, a real local HTTP
server, an isolated PostgreSQL schema for each test, and fake external responses.

The minimal template already has a `tests` module at the bottom of `src/main.rs`
with one database test. This guide adds a second one.

## 1. Prepare a test database

The test harness needs a reachable PostgreSQL server, nothing else. Start the
container of the generated project:

```bash
docker compose up -d
```

Each `TestContext` creates its own schema in that database, pushes the framework
tables and your registered models into it, and drops it on `shutdown()`. Your
development tables in the `public` schema are not touched, and there is no
migration step: the harness does not read `./migrations`.

The harness connects to `TEST_DATABASE_URL`, then `DATABASE_URL`, then the local
development database (`postgresql://openshop:openshop@localhost:5432/openshop`).
Set `TEST_DATABASE_URL` to use another server or a dedicated database:

```bash
export TEST_DATABASE_URL=postgresql://openshop:openshop@localhost:5432/openshop
```

## 2. Check the test setup

The harness lives in `openshop::testing`, behind the `testing` feature. The
generated `Cargo.toml` enables it for tests only:

```toml
[dev-dependencies]
openshop = { git = "https://github.com/0xtlt/openshop-rust", tag = "v<version>", features = ["testing"] }
```

No runner file is needed: `cargo test` discovers `#[test]` and `#[tokio::test]`
functions. The template marks tests that need PostgreSQL with
`#[ignore = "requires PostgreSQL"]`, so a plain `cargo test` passes without a
database and `cargo test -- --ignored` runs the database tests.

## 3. Test a flow and an authenticated request

In `src/main.rs`, add the `Value` import to the `tests` module and add the
test below, keeping the tests that are already there:

```rust,ignore
#[cfg(test)]
mod tests {
    use super::*;
    use openshop::Value;
    use openshop::engine::RunFlowResult;
    use openshop::testing::{MockTransport, TestContext, TestOptions};

    // ... existing tests ...

    #[tokio::test]
    #[ignore = "requires PostgreSQL"]
    async fn syncs_an_order_with_fake_external_services() {
        let ctx = TestContext::with_options(
            config().unwrap(),
            TestOptions {
                shop: "docs-test.myshopify.com".to_string(),
                access_token: Some("test-access-token".to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        // Shopify Admin API responses.
        let order = json!({ "id": "gid://shopify/Order/1", "name": "#1001" });
        ctx.transport.respond_json(json!({
            "data": { "orders": { "edges": [{ "node": order }], "nodes": [order] } },
        }));

        // The warehouse provider is replaced by a recording fake.
        let push = ctx.fakes.method("warehouse", "push");
        push.returns(true);

        let result = ctx.run_flow("syncOrders", json!({ "limit": 10 })).await.unwrap();

        assert_eq!(result, RunFlowResult::Completed);
        assert_eq!(push.call_count(), 1);
        assert_eq!(push.last_call().unwrap().args, json!([order]));

        // An authenticated Admin API request, signed like App Bridge signs it.
        let response = ctx.get("/api/runs").send().await.unwrap();
        assert_eq!(response.status(), 200);
        let runs: Vec<Value> = response.json().await.unwrap();
        assert_eq!(runs[0]["status"], "completed");

        ctx.shutdown().await.unwrap();
    }
}
```

The fixture supports both the template's `edges` query and the flow guide's
`nodes` query. Update it when your flow requests different fields.
`run_flow()` executes immediately with fake provider methods; it does not need a
worker. Shopify requests never leave the process: they are answered by
`ctx.transport`, and a request without a configured response gets a 404.
`ctx.get(..)` and the other request helpers add a staff session token for the
test shop; `ctx.request_as(method, path, shop, sub)` signs for another shop or
user.

## 4. Run and inspect the result

From the project root:

```bash
cargo test -- --ignored
```

The runner should report the database tests as passed, and `test result: ok`.
A failed assertion produces a nonzero exit code. If the tests fail with
`[openshop] Failed to connect to the test database`, check that PostgreSQL is
running and that the URL from step 1 points to it.

Every context owns its schema, so tests do not see each other's rows and can run
in parallel. Each context also opens its own connection pool; pass
`--test-threads=4` after `--` if PostgreSQL runs out of connections. End every
test with `ctx.shutdown().await`: it stops the HTTP server and drops the schema.
A test that panics before that line leaves its `test_…` schema behind.

Provider methods are faked only inside `run_flow()`. A provider reached through
the HTTP API, such as the provider check endpoint or a server route that calls a
connector, runs your real provider code. Keep production credentials out of
tests.

## Add other coverage

- Use `ctx.dispatch_flow(name, input)` to assert queue insertion; the test
  context does not start a worker. See [Test a flow](../reference/testing.md).
- Use the proxy test client, `ctx.proxy().get("/api/reviews").send()`, for
  signed storefront requests. `ctx.proxy_for(shop, secret)` signs for another
  shop or app secret.
- Use `ctx.http` with `ctx.url_for(path)` for requests without a session token,
  such as a server route or an unsigned webhook.
- Use factories (`define_factory` and `ctx.create(..)`) when a test must create
  resources through the Shopify client and remove them afterwards.
- See [custom admin page testing](../reference/custom-admin-pages.md#testing) for
  loader and action helpers.

All helper options and cleanup contracts are in the [testing reference](../reference/testing.md).
