# Test an app

Use this guide for a generated app with the sample `syncOrders` flow and
`warehouse` provider. It uses Rust's built-in test runner, a real local HTTP
server, an isolated PostgreSQL schema for each test, and fake external responses.

The minimal template already has a `tests` module at the bottom of `src/main.rs`
with one database test. This guide adds a second one.

## 1. Prepare a test database

The test harness needs a reachable PostgreSQL server, nothing else. Start the
container of the generated project, and wait until it accepts connections:

```bash
docker compose up -d --wait
```

Each test gets a `TestContext` with its own schema in that database: the
framework tables and the models of the application are pushed into it, and it
is dropped when the test ends. Your
development tables in the `public` schema are not touched, and there is no
migration step: the harness does not read `./migrations`.

The harness connects to `TEST_DATABASE_URL`, then `DATABASE_URL` when it points
at this machine, then the local development database
(`postgresql://openshop:openshop@localhost:5432/openshop`). A `DATABASE_URL` of
another host is refused, because tests create and drop schemas: set
`TEST_DATABASE_URL` to use another server or a dedicated database:

```bash
export TEST_DATABASE_URL=postgresql://openshop:openshop@localhost:5432/openshop
```

## 2. Check the test setup

The harness lives in `openshop::testing`, behind the `testing` feature. The
generated `Cargo.toml` enables it for tests only:

```toml
[dev-dependencies]
openshop = { version = "<version>", features = ["testing"] }
```

No runner file is needed: `cargo test` discovers `#[test]` functions and those
marked `#[openshop::test(APP)]`. That attribute makes an `async` function a
test of the application: it starts `APP` on an isolated database with its shop
installed, gives the function a `&TestContext`, and shuts the context down
when the function returns. The template marks tests that need PostgreSQL with
`#[ignore = "requires PostgreSQL"]`, below that attribute, so a plain
`cargo test` passes without a database and `cargo test -- --ignored` runs the
database tests.

## 3. Test a flow and an authenticated request

In `src/main.rs`, add the test below to the `tests` module, keeping the tests
that are already there:

```rust,ignore
#[cfg(test)]
mod tests {
    use super::*;
    use openshop::testing::TestContext;

    // ... existing tests ...

    #[openshop::test(APP)]
    #[ignore = "requires PostgreSQL"]
    async fn syncs_an_order_and_lists_its_run(ctx: &TestContext) {
        // Shopify answers the operation of the flow, `query GetRecentOrders`, by its name.
        ctx.shopify.on(
            "GetRecentOrders",
            json!({ "orders": { "nodes": [{ "id": "gid://shopify/Order/1", "name": "#1001" }] } }),
        );

        // The warehouse provider is replaced by a recording fake.
        let push = ctx.fakes.for_call(Warehouse::PUSH);
        push.returns(1);

        let outcome = ctx.run_flow(sync_orders::NAME, json!({ "limit": 10 })).await.unwrap();

        assert_eq!(outcome, RunOutcome::Completed);
        assert_eq!(push.call_count(), 1);
        let orders = push.last_call().unwrap().args;
        assert_eq!(orders[0].name, "#1001");

        // An authenticated Admin API request, signed like App Bridge signs it.
        let response = ctx.get("/api/runs").send().await.unwrap();
        assert_eq!(response.status(), 200);
        let runs: Vec<Value> = response.json().await.unwrap();
        assert_eq!(runs[0]["status"], "completed");
    }
}
```

`APP` is the application declared with `app!` in `src/main.rs`.
`sync_orders::NAME` is the name of the flow declared by `#[flow]` on
`sync_orders`, `"syncOrders"`, and `Warehouse::PUSH` names the `push` method of
the provider. They come with `use super::*`, like `json!`, `Value` and
`RunOutcome`: `src/main.rs` imports the prelude, and the flow and the provider
to list them in `app!`.

`ctx.fakes.for_call(Warehouse::PUSH)` is the fake of the method, with its
types: `returns` takes a `usize`, what `push` returns, and the `args` of a
recorded call are the `Vec<Order>` the method would have received.
`ctx.fakes.method("warehouse", "push")` is the same fake without the types,
with arguments and results as JSON.

The fixture is the `data` of the `GetRecentOrders` query of the template and
of the flow guide, which `ctx.shopify` answers by the name the query gives
itself. Update it when your flow requests different fields.
`run_flow()` executes immediately with fake provider methods; it does not need a
worker. Shopify requests never leave the process: `ctx.shopify` answers the
GraphQL operations, and one it does not answer fails with an error that names
it: ``The test does not answer the Admin API operation `GetRecentOrders` ``.
`ctx.shopify.active_subscriptions([subscription("Pro")])` answers what the
[billing](../reference/billing.md) helpers ask of the Admin API, and
`ctx.shopify.active_subscription(app_pricing_subscription("pro"))` what they
ask of the Partner API; see
[Answer the Admin API](../reference/testing.md#answer-the-admin-api) for the
rest, and for the operations OpenShop sends of its own.

The test reads the `shopify.app.toml` of the project, as the app does: the
pricing page and the links to the admin name the app by its `handle`. A
Shopify request that fails with a `5xx` is retried as many times as in
production, without waiting.
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
`--test-threads=4` after `--` if PostgreSQL runs out of connections.
`#[openshop::test]` shuts the context down when the function returns: it stops
the HTTP server and drops the schema. When a test panics, the schema is still
dropped on a best-effort basis as the context goes out of scope.

A test function returns nothing, or a `Result` whose error fails the test, so
that its body can use `?`.

Provider methods are faked only inside `run_flow()`. A provider reached through
the HTTP API, such as the provider check endpoint or a server route that calls a
connector, runs your real provider code. Keep production credentials out of
tests.

## Add other coverage

- Use `ctx.dispatch_flow(name, input)` to assert queue insertion; the test
  context does not start a worker. See [Test a flow](../reference/testing.md).
  To run a flow or a cron by hand against the development database, outside
  the tests, use [`openshop run`](../reference/cli.md#run-a-flow-or-a-cron-by-hand).
- Use the proxy test client, `ctx.proxy().get("/api/reviews").send()`, for
  signed storefront requests. `ctx.proxy_for(shop, secret)` signs for another
  shop or app secret.
- Use `ctx.webhook(topic, payload).send()` to deliver a webhook signed as
  Shopify signs it, with the headers it sends; `.signature("forged")` is one
  the app refuses. See [Deliver a webhook](../reference/testing.md#deliver-a-webhook).
- Use `ctx.http` with `ctx.url_for(path)` for requests without a session token,
  such as a server route.
- Start a context by hand with `TestContext::with_options(config, options)`
  for a configuration built in the test or for other options, such as another
  shop. `TestOptions::installed()` is the options of a shop that has installed
  the application. A test that starts its context ends with
  `ctx.shutdown().await`.
- Use `ctx.call_loader(path, name, input)` and `ctx.call_action(…)` to call a
  loader or an action of an admin page, and assert on what it returns or on
  the error the page receives, such as `402 PLAN_REQUIRED`; `ctx.page(path)`
  and `ctx.submit(path, fields)` to render the page and send its forms, and
  `toast(&response)` and `toast_error(&response)` to read the toast a form
  got. See [Test admin functions](../reference/testing.md#test-admin-functions)
  and [Test a page](../reference/testing.md#test-a-page).
- Name the requests and the answers of the context in a helper as
  `openshop::testing::RequestBuilder` and `openshop::testing::Response`: the
  tests need no `reqwest` dependency.

All helper options and lifecycle contracts are in the [testing reference](../reference/testing.md).
