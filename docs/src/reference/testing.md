# Testing

To set up a test suite, see [Test an app](../guides/test-app.md).

OpenShop exposes its test API from `openshop::testing`, behind the `testing`
cargo feature:

```toml
[dev-dependencies]
openshop = { git = "https://github.com/0xtlt/openshop-rust", tag = "v<version>", features = ["testing"] }
```

A `TestContext` starts the application on a real local HTTP server against an
isolated PostgreSQL schema that it creates and drops itself. Shopify HTTP calls
are answered by a scripted `MockTransport` unless you opt into live requests,
and provider methods are replaced with recording fakes when a flow is run
through the context.

## Context lifecycle

```rust,ignore
// tests/app.rs
use openshop::testing::{TestContext, TestOptions};

#[tokio::test]
async fn serves_health() {
    let ctx = TestContext::with_options(
        my_app::APP.config().expect("valid config"),
        TestOptions {
            access_token: Some("test-access-token".to_string()),
            ..Default::default()
        },
    )
    .await
    .expect("test context");

    let response = ctx.http.get(ctx.url_for("/health")).send().await.unwrap();
    assert_eq!(response.status(), 200);

    ctx.shutdown().await.unwrap();
}
```

`APP.config()` is the validated configuration of the application declared with
`app!`; each call builds a new one. `TestContext::new(config)` uses the default
options. Creating a context:

1. creates a schema named `test_<uuid>` in the test database and pushes the
   framework tables and the configuration's models into it;
2. builds the `OpenShop` handle with the options' credentials, without reading
   Shopify settings from the process environment;
3. inserts an installation for `shop` when `access_token` is set;
4. serves the application on a random port of `127.0.0.1`.

The test database is `TEST_DATABASE_URL`, then `DATABASE_URL`, then the local
development database. Contexts are independent, so tests can run in parallel
against one database.

`shutdown()` destroys factory-created resources, closes the server, and drops
the schema. A context that is dropped without `shutdown()`, for example because
an assertion panicked first, still drops its schema on a best-effort basis: the
attempt is limited to a few seconds and gives up when the schema is locked.
Factory-created resources are only destroyed by `shutdown()` or `cleanup()`.

The example imports the application from the app's library target. In a
binary-only project, write the tests in a `#[cfg(test)]` module of
`src/main.rs`, next to the declaration of `APP`.

### Test-only configurations

A test that needs only some blocks, or blocks of its own, builds its
configuration with `OpenShopConfig::builder()`. `.add::<Block>()` adds a
declared block of any kind, by its name; the builder types, such as
`Flow::new(…)`, make up a block in the test. `APP.builder()?` is the builder
of the declaration, to add to the whole application.

```rust,ignore
use my_app::flows::sync_orders::sync_orders;
use my_app::providers::warehouse::Warehouse;
use openshop::testing::TestContext;
use openshop::{Flow, OpenShopConfig};

async fn context() -> TestContext {
    let config = OpenShopConfig::builder()
        .add::<Warehouse>()
        .add::<sync_orders>()
        .flow(Flow::new("noop", |_ctx| async { Ok(()) }))
        .build()
        .expect("valid config");

    TestContext::new(config).await.expect("test context")
}
```

`build()` refuses a key registered twice, for example with
`[openshop] Invalid config: flows.syncOrders is registered twice`.

## `TestOptions`

| Field | Default | Purpose |
| --- | --- | --- |
| `shop` | `test.myshopify.com` | Default shop for flows, proxy signatures, tokens, and factories. |
| `secret` | `test-secret` | Shopify API secret of the single-app setup. Signs proxy requests and session tokens. |
| `api_key` | `test-app` | Shopify API key of the single-app setup. Session-token audience. |
| `access_token` | `None` | Inserts an installation so Shopify client calls and factories can run. |
| `encryption_key` | `None` | `ENCRYPTION_KEY` of the application. |
| `host` | `None` | `HOST` of the application. |
| `production` | `false` | Production mode, for example to test encryption enforcement. |
| `cwd` | A directory without Shopify TOML files | Project root used to resolve Shopify TOML files. |
| `server` | `ServerOptions::default()` | API only: no admin UI is served. |
| `transport` | A new `MockTransport` | Transport shared with other contexts or prepared in advance. |
| `live_shopify` | `false` | Sends Shopify requests over the network instead of to the mock transport, for tests that create real resources with factories. |

`TestOptions` implements `Default`; set the fields you need with struct update
syntax.

## `TestContext`

| Member | Contract |
| --- | --- |
| `url`, `port` | Base URL (`http://127.0.0.1:<port>`) and port of the server. |
| `url_for(path)` | Absolute URL of a path. |
| `app` | The `OpenShop` handle; `ctx.app.config()` is the configuration. |
| `db` | `PgPool` on the context's schema. |
| `shop`, `secret`, `api_key` | The values from the options. |
| `fakes` | `FakeProviders` used by `run_flow`. |
| `transport` | The `MockTransport` behind every Shopify HTTP call. |
| `http` | A `reqwest::Client` that does not follow redirects, for unauthenticated requests. |
| `run_flow(flow, input)` / `run_flow_for(flow, input, shop)` | Executes a flow immediately with the fakes. |
| `dispatch_flow(flow, input)` / `dispatch_flow_for(flow, input, shop)` | Queues a run and returns its `Uuid`. |
| `proxy()` / `proxy_for(shop, secret)` | App proxy client that signs requests. |
| `get`, `post`, `put`, `patch`, `delete`, `request(method, path)` | Request builders authenticated with a session token for the default shop. |
| `request_as(method, path, shop, sub)` | Request authenticated as staff user `sub` of `shop`. |
| `session_token()` / `session_token_for(shop, sub)` | A signed session token, valid for one minute. |
| `authorization_header()` / `authorization_header_for(shop, sub)` | `Bearer <session token>`. |
| `create(&factory, overrides)`, `cleanup()` | Factory resources; see below. |
| `reset_fakes()` | Resets every provider fake. |
| `shutdown()` | Cleans up and drops the schema. |

## Test a flow

```rust,ignore
use my_app::flows::sync_orders::sync_orders;
use openshop::engine::RunFlowResult;
use openshop::json;
use openshop::testing::{MockTransport, TestContext};

async fn syncs_orders(ctx: &TestContext) {
    ctx.transport.respond_with(|request| {
        let query = request.body["query"].as_str().unwrap_or_default();
        assert!(query.contains("orders"));
        Ok(MockTransport::json(
            200,
            json!({ "data": { "orders": { "edges": [
                { "node": { "id": "gid://shopify/Order/1", "name": "#1001" } },
            ] } } }),
        ))
    });
    ctx.fakes.method("warehouse", "push").returns(true);

    let result = ctx.run_flow(sync_orders::NAME, json!({ "limit": 10 })).await.unwrap();

    assert_eq!(result, RunFlowResult::Completed);
    let push = ctx.fakes.method("warehouse", "push");
    assert!(push.called());
    assert_eq!(push.call_count(), 1);
    assert!(push.last_call().unwrap().args.is_array());
}
```

`sync_orders::NAME` is the name of the flow declared by `#[flow]` on
`sync_orders`, here `"syncOrders"`; the string works as well.
`run_flow(flow, input)` inserts a running row and executes the flow
immediately with fake provider connectors. It does not require a worker, and
the run is not retried: a failing flow returns
`RunFlowResult::Failed { error, will_retry: false }`.

| `RunFlowResult` | Meaning |
| --- | --- |
| `Completed` | The flow returned `Ok(())`. |
| `Sleeping { resume_at }` | The flow called `step.sleep`. |
| `Failed { error, will_retry }` | The flow returned an error, panicked, or received invalid input. |
| `Canceled` | The run was canceled. |
| `LeaseLost` | Another worker owns the run. |

Shopify GraphQL calls go to `ctx.transport`:

| `MockTransport` | Contract |
| --- | --- |
| `respond_with(responder)` | Sets the responder, a closure that receives the `OutboundRequest` (`url`, `headers`, `body`) and returns a `Result<OutboundResponse>`. |
| `respond_json(body)` | Answers every request with status `200` and this JSON body. |
| `MockTransport::json(status, body)` | Builds a JSON `OutboundResponse`. |
| `requests()`, `last_request()` | The recorded requests. |
| `clear()` | Forgets the recorded requests. |

Without a responder every request is answered with `404` and the body
`not mocked`. The transport also receives the OAuth token requests made by the
server.

Use `dispatch_flow(flow, input)` to test queueing:

```rust,ignore
use my_app::flows::sync_orders::sync_orders;
use openshop::testing::TestContext;
use openshop::{json, sqlx};

async fn queues_a_run(ctx: &TestContext) {
    let run_id = ctx.dispatch_flow(sync_orders::NAME, json!({ "limit": 10 })).await.unwrap();

    let (status,): (String,) = sqlx::query_as("SELECT status FROM flow_runs WHERE id = $1")
        .bind(run_id)
        .fetch_one(&ctx.db)
        .await
        .unwrap();

    assert_eq!(status, "pending");
}
```

`dispatch_flow` only queues the run. A `TestContext` does not start a worker or
the cron scheduler; start a `Worker` on `ctx.app.clone()` when a test needs
queued runs to execute.

## Configure provider fakes

Every configured provider method gets a `FakeMethod`:

```rust,ignore
use openshop::json;
use openshop::testing::TestContext;

fn configure(ctx: &TestContext) {
    let push = ctx.fakes.method("warehouse", "push");

    push.returns(json!({ "accepted": true }));
    push.rejects("warehouse offline");
    push.on_call_returns(0, json!({ "accepted": true }));
    push.on_call_rejects(1, "rate limited");
    push.implementation(|args| async move { Ok(json!({ "accepted": args.is_array() })) });

    println!("{} {}", push.called(), push.call_count());
    println!("{:?} {:?}", push.calls(), push.last_call());

    push.reset();
    ctx.reset_fakes();
}
```

A call is answered by the first rule that applies: the rule for that call
index (zero-based), then the implementation, then `rejects`, then `returns`. A
fake without any rule returns `null`. `ctx.fakes.method(…)` panics for an
unknown provider or method; `ctx.fakes.get(…)` returns an `Option` instead.

`ctx.fakes.for_call(Warehouse::PUSH)` returns the fake of a method of a
[provider struct](providers.md#the-methods), or of any
[typed call](providers.md#typed-calls): the same one as
`ctx.fakes.method("warehouse", "push")`. Give it a result of the type the call
returns: `connectors.invoke(…)` refuses the default `null` with
`Invalid result from "warehouse.push": …`, unless the call returns `()` or an
`Option`.

Each `FakeCall` contains `args`, `returned_value`, `thrown_error`, and
`timestamp`. `reset()` clears behavior and history for one method;
`ctx.reset_fakes()` resets every provider fake.

The module also exports `create_fake_providers(&config.providers)` for tests
that do not need an HTTP context. `FakeProviders::connectors()` returns the
`Connectors` to pass to `RunFlowOptions::connectors`.

## Test a proxy route

The proxy client signs app proxy parameters like Shopify does:

```rust,ignore
use openshop::testing::TestContext;

#[derive(serde::Deserialize)]
struct Reviews {
    reviews: Vec<Review>,
}

#[derive(serde::Deserialize)]
struct Review {
    id: u64,
    rating: u8,
}

async fn lists_reviews(ctx: &TestContext) {
    let response = ctx
        .proxy()
        .get("/api/reviews")
        .as_customer("123")
        .qs(&[("page", "1")])
        .header("X-Test-Request", "proxy-spec")
        .send()
        .await
        .unwrap();

    assert_eq!(response.status, 200);
    assert!(response.content_type.contains("json"));
    assert_eq!(response.body["reviews"][0]["rating"], 5);

    let typed: Reviews = response.json().unwrap();
    assert!(typed.reviews.iter().all(|review| review.id > 0 && review.rating <= 5));
}
```

Available builders are `get`, `post`, `put`, `delete`, and `patch`. The path is
relative to `/proxy`. Chain:

- `as_customer(id)` to set `logged_in_customer_id`;
- `qs(&[(key, value)])` to add signed query parameters;
- `json(value)` to send a JSON body;
- `header(name, value)` to add a header;
- `send()` to receive a `ProxyResponse` with `status`, `headers`, `body`,
  `text`, and `content_type`.

`body` is the parsed JSON for JSON responses and the text as a JSON string
otherwise. `response.json::<T>()` deserializes it. `ctx.proxy_for(shop, secret)`
signs for another shop or with another app's secret.

## Test the Admin API

The context's request builders add a signed Shopify session token:

```rust,ignore
use openshop::Value;
use openshop::response::Method;
use openshop::testing::TestContext;

async fn lists_runs(ctx: &TestContext) {
    let response = ctx.get("/api/runs").send().await.unwrap();
    assert_eq!(response.status(), 200);
    let runs: Vec<Value> = response.json().await.unwrap();
    assert!(runs.is_empty());

    // Another shop, or another staff user.
    let other = ctx
        .request_as(Method::GET, "/api/runs", "other.myshopify.com", "42")
        .send()
        .await
        .unwrap();
    assert_eq!(other.status(), 200);
}
```

The builders are `reqwest::RequestBuilder` values: finish them with
`.json(&body)`, `.send()`, and the usual `reqwest` response methods. For custom
claims, `ctx.session_token_for(shop, sub)` returns the raw JWT.

## Sign other requests

`openshop::testing` exports the signing primitives for requests the context
does not build:

| Function | Produces |
| --- | --- |
| `create_session_token(shop, secret, api_key, sub)` | A session token as issued by App Bridge, valid for one minute. |
| `sign_session_token(&claims, secret)` | An HS256 JWT with the claims you choose. |
| `sign_webhook_body(body, secret)` | The `X-Shopify-Hmac-Sha256` value for a webhook body. |
| `sign_query(&[(key, value)], secret)` | The query map with the `hmac` parameter Shopify adds to OAuth and app-launch URLs. |
| `proxy_signature(&query, secret)` | The `signature` parameter of an app proxy query. |
| `to_query_string(&query)` | A query map serialized for a URL. |

```rust,ignore
use openshop::testing::{TestContext, sign_webhook_body};

async fn delivers_a_webhook(ctx: &TestContext) {
    let body = r##"{"id":1,"name":"#1001"}"##;
    let response = ctx
        .http
        .post(ctx.url_for("/webhooks"))
        .header("X-Shopify-Topic", "orders/create")
        .header("X-Shopify-Shop-Domain", &ctx.shop)
        .header("X-Shopify-Hmac-Sha256", sign_webhook_body(body, &ctx.secret))
        .body(body)
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
}
```

## Use factories and automatic cleanup

Factories create Shopify resources through the context's Shopify client and
remember how to delete them:

```rust,ignore
use openshop::testing::{Factory, define_factory};
use openshop::{Value, bail, json};

#[derive(Clone)]
pub struct Customer {
    pub id: String,
    pub email: String,
}

const CREATE_CUSTOMER: &str = "#graphql
  mutation CreateCustomer($input: CustomerInput!) {
    customerCreate(input: $input) {
      customer { id email }
      userErrors { message }
    }
  }
";

const DELETE_CUSTOMER: &str = "#graphql
  mutation DeleteCustomer($id: ID!) {
    customerDelete(input: { id: $id }) {
      deletedCustomerId
      userErrors { message }
    }
  }
";

pub fn customer_factory() -> Factory<Customer> {
    define_factory(
        |shopify, overrides: Value| async move {
            let email = overrides["email"].as_str().unwrap_or("test@example.com").to_string();
            let data = shopify
                .graphql(CREATE_CUSTOMER, Some(json!({ "input": { "email": email } })))
                .await?;
            if let Some(error) = data["customerCreate"]["userErrors"].get(0) {
                bail!("{}", error["message"]);
            }
            let id = data["customerCreate"]["customer"]["id"].as_str().unwrap_or_default().to_string();
            Ok(Customer { id, email })
        },
        |shopify, customer: Customer| async move {
            shopify.graphql(DELETE_CUSTOMER, Some(json!({ "id": customer.id }))).await?;
            Ok(())
        },
    )
}
```

Create and track a resource:

```rust,ignore
use openshop::json;
use openshop::testing::TestContext;

async fn creates_a_customer(ctx: &mut TestContext) {
    let customer = ctx
        .create(&customer_factory(), json!({ "email": "flow-test@example.com" }))
        .await
        .unwrap();
    assert!(!customer.email.is_empty());
}
```

`ctx.cleanup()` destroys tracked resources in last-in, first-out order. Cleanup
continues after a destroy error and logs the failure. `ctx.shutdown()` calls
cleanup again safely before closing the server.

Without an installation that has an access token, `ctx.create()` fails with
`No Shopify client available. Is the test shop installed?`.

By default a `TestContext` talks to its `MockTransport`, so its factories
exercise your scripted responses, not a real shop. Set
`TestOptions::live_shopify` to `true`, with the `shop` and `access_token` of a
development shop, to create real resources. Prefer a dedicated development
shop and never run destructive factories against a production shop.

For manual lifecycle control, `openshop::testing` also exports `FactoryScope`.
Construct it with a `ShopifyClient`, call `scope.create(&factory, overrides)`,
inspect `scope.size()`, and call `scope.cleanup()` in teardown.

## Query the database

`ctx.db` is the `sqlx` pool of the context's schema:

```rust,ignore
use openshop::sqlx;
use openshop::testing::TestContext;

async fn runs_belong_to_the_test_shop(ctx: &TestContext) {
    let shops: Vec<(String,)> = sqlx::query_as("SELECT shop FROM flow_runs")
        .fetch_all(&ctx.db)
        .await
        .unwrap();

    assert!(shops.iter().all(|(shop,)| shop == "test.myshopify.com"));
}
```

The schema starts empty and is dropped by `shutdown()`, so tests do not need to
delete their rows. `TestDb::create()` and `TestDb::create_with_models(&models)`
give the same isolated schema (`pool`, `schema`, `cleanup()`) without a
server.

## Test admin functions

Call a custom admin page loader or action without HTTP:

```rust,ignore
use my_app::admin::reviews::ReviewsPage;
use openshop::testing::{
    AdminFunctionTestOptions, TestContext, create_admin_function_test_context, invoke_admin_function,
};
use openshop::{Value, json};

async fn saves_a_review(ctx: &TestContext) {
    let config = ctx.app.config();
    let page = config
        .admin_pages
        .iter()
        .find(|page| page.path == ReviewsPage::PATH)
        .expect("the reviews page");
    let context = create_admin_function_test_context(&ctx.app, AdminFunctionTestOptions::default())
        .await
        .unwrap();

    let output: Value = invoke_admin_function(&page.functions["saveReview"], context, json!({ "title": "Great" }))
        .await
        .unwrap();

    assert_eq!(output["title"], "Great");
}
```

`config.admin_pages` holds the pages of the application, in the order of the
`admin_pages` list of `app!`. The `functions` of a page are its loaders and
actions, by the name the page view calls them with: `saveReview` for a function
`save_review` marked `#[action]`.

`AdminFunctionTestOptions` has the fields `shop` (`test.myshopify.com`),
`shopify_app` (`default`), `actor_id` (`test-actor`), `params` (empty), and
`request_id` (`test-request`). `invoke_admin_function` applies the function's
own `authorize` rule and input schema, then deserializes the output. It does
not evaluate page access rules (`#[access]`); cover those with HTTP tests
through `ctx.post("/api/pages/custom/…")`.

## Test runner integration

Tests run with `cargo test` (or `openshop test`). They need a reachable
PostgreSQL database and an async test attribute such as `#[tokio::test]`.

The context does not start a worker or the scheduler, and it does not make real
Shopify requests unless `live_shopify` is set. `shutdown()` closes the HTTP
server, cleans up factory resources, and drops the context's schema.
