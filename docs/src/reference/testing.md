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
use my_app::APP;
use openshop::testing::TestContext;

#[openshop::test(APP)]
async fn serves_health(ctx: &TestContext) {
    let response = ctx.http.get(ctx.url_for("/health")).send().await.unwrap();
    assert_eq!(response.status(), 200);
}
```

`#[openshop::test(APP)]` makes an `async` function a test of the application
declared with `app!`. It starts the application on an isolated database with
its shop installed, passes the `TestContext` to the function, and shuts the
context down when the function returns.

| Part | Contract |
| --- | --- |
| `#[openshop::test(APP)]` | Names the application under test: `APP`, or a path such as `my_app::APP`. It can also be an expression that starts a context: see below. |
| `ctx: &TestContext` | The only argument. Take `&mut TestContext` to [create resources with a factory](#use-factories-and-automatic-cleanup). |
| Return type | Nothing, or a `Result` whose error fails the test. |
| Other attributes of the test, such as `#[ignore]` | They go below `#[openshop::test]`. |

The attribute runs the test on a runtime of its own: there is no
`#[tokio::test]` to add.

The argument of the attribute is what the test starts from. Besides an
application, it is any expression that resolves to a `TestContext`, or to a
`Result` of one, such as a constructor or a function of the test file:

```rust,ignore
use openshop::builder::Flow;
use openshop::testing::{TestContext, TestOptions};
use openshop::{OpenShopConfig, json};

fn config() -> OpenShopConfig {
    OpenShopConfig::builder()
        .flow(Flow::new("noop", |_ctx| async { Ok(()) }))
        .build()
        .expect("valid config")
}

/// A context for a shop that has not installed the application.
async fn uninstalled() -> TestContext {
    TestContext::new(config()).await.expect("test context")
}

#[openshop::test(uninstalled())]
async fn lists_the_flow(ctx: &TestContext) {
    let response = ctx.get("/api/flows").send().await.unwrap();
    assert_eq!(response.status(), 200);
}

#[openshop::test(TestContext::with_options(config(), TestOptions::installed()))]
async fn runs_the_flow(ctx: &TestContext) {
    ctx.run_flow("noop", json!({})).await.unwrap();
}
```

A test whose context depends on values it makes itself, such as a recorder
its configuration captures, starts the context by hand.

A context can also be started by hand:

| Constructor | Configuration | Options |
| --- | --- | --- |
| `TestContext::for_app(&APP).await`, or `APP.test().await` | `APP.config()` | [`TestOptions::installed()`](#testoptions). What `#[openshop::test]` calls. |
| `TestContext::new(config).await` | Given | `TestOptions::default()`: the shop is not installed. |
| `TestContext::with_options(config, options).await` | Given | Given |

`APP.config()` is the validated configuration of the application declared with
`app!`; each call builds a new one. A context started by hand ends with
`ctx.shutdown().await`. Creating a context:

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
the schema. `#[openshop::test]` calls it when the function returns. A context
that is dropped without `shutdown()`, for example because an assertion panicked
first, still drops its schema on a best-effort basis: the attempt is limited to
a few seconds and gives up when the schema is locked. Factory-created resources
are only destroyed by `shutdown()` or `cleanup()`.

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
use openshop::testing::{TestContext, TestOptions};
use openshop::builder::Flow;
use openshop::OpenShopConfig;

async fn context() -> TestContext {
    let config = OpenShopConfig::builder()
        .add::<Warehouse>()
        .add::<sync_orders>()
        .flow(Flow::new("noop", |_ctx| async { Ok(()) }))
        .build()
        .expect("valid config");

    TestContext::with_options(config, TestOptions::installed())
        .await
        .expect("test context")
}
```

`build()` refuses a key registered twice, for example with
`[openshop] Invalid config: flows.syncOrders is registered twice`.

Such a configuration is not a declared `App`: its context is started by hand,
in a test that ends with `ctx.shutdown().await`.

## `TestOptions`

| Field | Default | Purpose |
| --- | --- | --- |
| `shop` | `test.myshopify.com` | Default shop for flows, proxy signatures, tokens, and factories. |
| `secret` | `test-secret` | Shopify API secret of the single-app setup. Signs proxy requests and session tokens. |
| `api_key` | `test-app` | Shopify API key of the single-app setup. Session-token audience. |
| `access_token` | `None`; `test-access-token` with `TestOptions::installed()` | Inserts an installation so Shopify client calls and factories can run. |
| `encryption_key` | `None` | `ENCRYPTION_KEY` of the application. |
| `client_credentials_shop` | `None` | The `client_credentials_shop` setting of the application, in place of the one its configuration gives: the shop background work may get a token for with the [client credentials grant](authentication.md#client-credentials-grant), from the mock transport. A token stored by `access_token` is used as it is. |
| `host` | `None` | `HOST` of the application. |
| `production` | `false` | Production mode, for example to test encryption enforcement. |
| `cwd` | A directory without Shopify TOML files | Project root used to resolve Shopify TOML files. |
| `server` | `ServerOptions::default()` | API only: no admin UI is served. |
| `transport` | A new `MockTransport` | Transport shared with other contexts or prepared in advance. |
| `live_shopify` | `false` | Sends Shopify requests over the network instead of to the mock transport, for tests that create real resources with factories. |

`TestOptions` implements `Default`; set the fields you need with struct update
syntax. `TestOptions::installed()` is the default options with an
`access_token`: the shop has installed the application, so that flows, proxies
and the admin find a token for it. `#[openshop::test]` uses it.

```rust,ignore
use openshop::testing::TestOptions;

fn another_shop() -> TestOptions {
    TestOptions {
        shop: "another-shop.myshopify.com".to_string(),
        ..TestOptions::installed()
    }
}
```

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
| `shutdown()` | Cleans up and drops the schema. `#[openshop::test]` calls it. |

## Test a flow

```rust,ignore
use my_app::APP;
use my_app::flows::sync_orders::sync_orders;
use my_app::providers::warehouse::Warehouse;
use openshop::prelude::*;
use openshop::testing::{MockTransport, TestContext};

#[openshop::test(APP)]
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
    // The fake takes what `push` returns, and gives back what it received.
    let push = ctx.fakes.for_call(Warehouse::PUSH);
    push.returns(1);

    let outcome = ctx.run_flow(sync_orders::NAME, json!({ "limit": 10 })).await.unwrap();

    assert_eq!(outcome, RunOutcome::Completed);
    assert_eq!(push.call_count(), 1);
    let orders = push.last_call().unwrap().args;
    assert_eq!(orders[0].id, "gid://shopify/Order/1");
}
```

`sync_orders::NAME` is the name of the flow declared by `#[flow]` on
`sync_orders`, here `"syncOrders"`; the string works as well.
`run_flow(flow, input)` inserts a running row and executes the flow
immediately with fake provider connectors. It does not require a worker, and
the run is not retried: a failing flow returns
`RunOutcome::Failed { error, will_retry: false }`.

| `RunOutcome` | Meaning |
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
use my_app::APP;
use my_app::flows::sync_orders::sync_orders;
use openshop::prelude::*;
use openshop::sqlx;
use openshop::testing::TestContext;

#[openshop::test(APP)]
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

Every configured provider method gets a fake.
`ctx.fakes.for_call(Warehouse::PUSH)` returns the fake of a method of a
[provider struct](providers.md#the-methods), or of any
[typed call](providers.md#typed-calls), as a `TypedFake`: it takes what the
method returns, and gives back the arguments as the method receives them.

```rust,ignore
use my_app::providers::warehouse::{Order, Warehouse};
use openshop::testing::TestContext;

// `Warehouse::push(&self, orders: Vec<Order>) -> Result<usize>`
fn configure(ctx: &TestContext) {
    let push = ctx.fakes.for_call(Warehouse::PUSH);

    push.returns(2);
    push.rejects("warehouse offline");
    push.on_call_returns(0, 2);
    push.on_call_rejects(1, "rate limited");

    println!("{} {}", push.called(), push.call_count());
    let last: Option<Vec<Order>> = push.last_call().map(|call| call.args);
    let all: Vec<Vec<Order>> = push.calls().into_iter().map(|call| call.args).collect();
    println!("{} {}", last.is_some(), all.len());

    push.reset();
}
```

| `TypedFake` | Contract |
| --- | --- |
| `returns(value)` | Answers every call without a more specific rule. `value` has the type the method returns. |
| `rejects(error)` | Makes every call without a more specific rule fail. |
| `on_call_returns(n, value)`, `on_call_rejects(n, error)` | The answer of the `n`-th call, zero-based. |
| `called()`, `call_count()` | Whether, and how many times, the method was called. |
| `calls()`, `last_call()` | The recorded calls, oldest first, as `TypedCall`s: `args` has the type of the arguments of the method. A method with several arguments gives them as a tuple, one without argument as `()`. |
| `reset()` | Forgets the recorded calls and every rule. |
| `untyped()` | The `FakeMethod` behind it, to answer with something the method could not return, or to replace its body. |

`ctx.fakes.method("warehouse", "push")` returns the same fake without the
types, a `FakeMethod`. It takes any serializable value, records the arguments
as JSON, and can replace the body of the method:

```rust,ignore
use openshop::prelude::*;
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
fake without any rule returns `null`. `ctx.fakes.for_call(…)` and
`ctx.fakes.method(…)` panic for an unknown provider or method;
`ctx.fakes.get(…)` returns an `Option` instead.

Give the fake of a typed method a result with `returns(…)`:
`Warehouse::on(..).push(..)` refuses the default `null` with
`Invalid result from "warehouse.push": …`, unless the method returns `()` or
an `Option`.

Each `FakeCall` contains `args`, `returned_value`, `thrown_error`, and
`timestamp`; a `TypedCall` has the same fields, with typed `args`. `reset()`
clears behavior and history for one method; `ctx.reset_fakes()` resets every
provider fake.

The module also exports `FakeProviders`: `FakeProviders::new(&config.providers)`
builds the fakes for tests that do not need an HTTP context.
`FakeProviders::connectors()` returns the `Connectors` to pass to
`RunFlowOptions::connectors`.

## Test a proxy route

The proxy client signs app proxy parameters like Shopify does:

```rust,ignore
use my_app::APP;
use openshop::testing::TestContext;
use serde::Deserialize;

#[derive(Deserialize)]
struct Reviews {
    reviews: Vec<Review>,
}

#[derive(Deserialize)]
struct Review {
    id: u64,
    rating: u8,
}

#[openshop::test(APP)]
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

A proxy with a typed input answers `422` to a query or a body that does not
fit it, before its handler runs: `qs(&[("page", "two")])` on a handler that
takes `page: Option<u32>` gives `response.status == 422`. The answer of a
`liquid` proxy is in `response.text`: the variables its handler assigns, then
the template file as it is written. See [Proxy routes](proxy-routes.md).

## Test the Admin API

The context's request builders add a signed Shopify session token:

```rust,ignore
use my_app::APP;
use openshop::prelude::*;
use openshop::response::Method;
use openshop::testing::TestContext;

#[openshop::test(APP)]
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
| `session_token(shop, secret, api_key, sub)` | A session token as issued by App Bridge, valid for one minute. |
| `sign_session_token(&claims, secret)` | An HS256 JWT with the claims you choose. |
| `sign_webhook_body(body, secret)` | The `X-Shopify-Hmac-Sha256` value for a webhook body. |
| `sign_query(&[(key, value)], secret)` | The query map with the `hmac` parameter Shopify adds to OAuth and app-launch URLs. |
| `proxy_signature(&query, secret)` | The `signature` parameter of an app proxy query. The query needs a `timestamp` within five minutes of now to be accepted. |
| `to_query_string(&query)` | A query map serialized for a URL. |

```rust,ignore
use my_app::APP;
use openshop::testing::{TestContext, sign_webhook_body};

#[openshop::test(APP)]
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
use openshop::prelude::*;
use openshop::testing::Factory;

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
    Factory::new(
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

`Factory::new(create, destroy)` takes what creates a resource, from the
client of the shop and the overrides of a test, and what destroys it
afterwards.

Create and track a resource, in a test that takes `&mut TestContext`:

```rust,ignore
use my_app::APP;
use openshop::prelude::*;
use openshop::testing::TestContext;

#[openshop::test(APP)]
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
cleanup again safely before closing the server, and `#[openshop::test]` calls
`shutdown()` when the function returns.

Without an installation that has an access token, `ctx.create()` fails with
`No Shopify client available. Is the test shop installed?`. The context of
`#[openshop::test]` has one.

By default a `TestContext` talks to its `MockTransport`, so its factories
exercise your scripted responses, not a real shop. Set
`TestOptions::live_shopify` to `true`, with the `shop` and `access_token` of a
development shop, to create real resources, in a context started with
`TestContext::with_options(…)`. Prefer a dedicated development
shop and never run destructive factories against a production shop.

For manual lifecycle control, `openshop::testing` also exports `FactoryScope`.
Construct it with a `ShopifyClient`, call `scope.create(&factory, overrides)`,
inspect `scope.size()`, and call `scope.cleanup()` in teardown.

## Query the database

`ctx.db` is the `sqlx` pool of the context's schema:

```rust,ignore
use my_app::APP;
use openshop::sqlx;
use openshop::testing::TestContext;

#[openshop::test(APP)]
async fn runs_belong_to_the_test_shop(ctx: &TestContext) {
    let shops: Vec<(String,)> = sqlx::query_as("SELECT shop FROM flow_runs")
        .fetch_all(&ctx.db)
        .await
        .unwrap();

    assert!(shops.iter().all(|(shop,)| shop == "test.myshopify.com"));
}
```

A test context stands for its shop, like the context of a handler: it is a
`ShopScope`, for the Shopify app `default`. The rows of a
[model](database.md#app-models) are read and written with `Review::of(ctx)`,
and `sync_orders::dispatch(ctx, input)` queues a run for the shop of the test.
`Review::for_shop(&ctx.db, "other.myshopify.com")` reads another shop.

The schema starts empty and is dropped by `shutdown()`, so tests do not need to
delete their rows. `TestDb::create()` and `TestDb::create_with_models(&models)`
give the same isolated schema (`pool`, `schema`, `cleanup()`) without a
server. `models` are `Table`s, such as `Review::table()`.

## Test admin functions

Call a custom admin page loader or action without HTTP:

```rust,ignore
use my_app::APP;
use my_app::admin::reviews::ReviewsPage;
use openshop::prelude::*;
use openshop::testing::{
    AdminTestOptions, TestContext, admin_context, invoke_admin_function,
};

#[openshop::test(APP)]
async fn saves_a_review(ctx: &TestContext) {
    let config = ctx.app.config();
    let page = config
        .admin_pages
        .iter()
        .find(|page| page.path == ReviewsPage::PATH)
        .expect("the reviews page");
    let context = admin_context(&ctx.app, AdminTestOptions::default())
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

`admin_context(&app, options)` builds the `AdminContext` a function receives,
with `ctx.db`, `ctx.shopify` and `ctx.connectors` for the shop of the options.
`ctx.shopify` is the client of an admin request, as in production: for that
shop and app, a token the Admin API refuses is renewed once with the
[client credentials grant](authentication.md#client-credentials-grant), from
the mock transport.
`AdminTestOptions` has the fields `shop` (`test.myshopify.com`),
`shopify_app` (`default`), `actor_id` (`test-actor`), `params` (empty), and
`request_id` (`test-request`). `invoke_admin_function` applies the function's
own `authorize` rule and input schema, then deserializes the output. It does
not evaluate page access rules (`#[access]`); cover those with HTTP tests
through `ctx.post("/api/pages/custom/…")`.

## Test runner integration

Tests run with `cargo test` (or `openshop test`). They need a reachable
PostgreSQL database. `#[openshop::test]` runs a test on a runtime of its own; a
test that starts its context by hand needs an async test attribute such as
`#[tokio::test]`.

The context does not start a worker or the scheduler, and it does not make real
Shopify requests unless `live_shopify` is set. `shutdown()` closes the HTTP
server, cleans up factory resources, and drops the context's schema.
