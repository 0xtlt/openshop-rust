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
are answered by a scripted `MockTransport`: the GraphQL operations of the
Admin API by their name, through [`ctx.shopify`](#answer-the-admin-api), and
provider methods are replaced with recording fakes when a flow is run through
the context.

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

The contexts a `#[openshop::test]` starts read the Shopify TOML files of the
crate of the test, in its `CARGO_MANIFEST_DIR`: the `handle` and the `name` of
`shopify.app.toml`, and its scopes. So `billing::pricing_url(ctx)?` and the
links of a route name the app as Shopify does. When the file has a
`client_id`, the test is that app: its `client_id` is the API key of the
context (`ctx.api_key`), unless the options give one, or a
`shopify.app*.toml` there is the one of `test-app`. A `cwd` in the
[options](#testoptions) reads another directory.

| Part | Contract |
| --- | --- |
| `#[openshop::test(APP)]` | Names the application under test: `APP`, or a path such as `my_app::APP`. It can also be an expression that starts a context: see below. |
| `ctx: &TestContext` | The only argument. |
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
| `TestContext::for_app(&APP).await` | `APP.config()` | [`TestOptions::installed()`](#testoptions). What `#[openshop::test]` calls. |
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

The test database is `TEST_DATABASE_URL`, then `DATABASE_URL` when it points at
this machine, then the local development database. A `DATABASE_URL` of another
host is refused: set `TEST_DATABASE_URL` to the database the tests may use.
Contexts are independent, so tests can run in parallel
against one database.

`shutdown()` closes the server and drops the schema. `#[openshop::test]` calls
it when the function returns. A context that is dropped without `shutdown()`,
for example because an assertion panicked first, still drops its schema on a
best-effort basis: the attempt is limited to a few seconds and gives up when
the schema is locked.

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
| `shop` | `test.myshopify.com` | Default shop for flows, proxy signatures, and tokens. |
| `secret` | `test-secret` | Shopify API secret of the single-app setup. Signs proxy requests and session tokens. |
| `api_key` | `test-app` (`TEST_API_KEY`) | Shopify API key of the single-app setup. Session-token audience. With `test-app` and a `shopify.app.toml` in `cwd` whose `client_id` is another, the `client_id`. |
| `access_token` | `None`; `test-access-token` with `TestOptions::installed()` | Inserts an installation so Shopify client calls can run. |
| `encryption_key` | `None` | `ENCRYPTION_KEY` of the application. |
| `development` | `true` | A development environment: without an `encryption_key`, secrets are stored in plaintext. With `false` (or `production: true`) and no key, the context does not start. |
| `client_credentials_shop` | `None` | The `client_credentials_shop` setting of the application, in place of the one its configuration gives: the shop background work may get a token for with the [client credentials grant](authentication.md#client-credentials-grant), from the mock transport. A token stored by `access_token` is used as it is. |
| `host` | `None` | `HOST` of the application. |
| `production` | `false` | Production mode, for example to test the checks it turns on. |
| `cwd` | Under `#[openshop::test]`, the directory of the crate of the test (`CARGO_MANIFEST_DIR`); otherwise a directory without Shopify TOML files | Project root used to resolve Shopify TOML files: the `handle`, the `name` and the scopes of the app. |
| `shopify_retry` | `None`: the retries of the application without waiting | How the [Shopify client retries](configuration.md#shopify-client-retries) what Shopify did not run. `None` takes the `shopify_retry` of the application, else `shopify_api`'s four retries, with the same number of attempts and no wait between them: a test that answers `503` runs the code that retries, in no time. `Some(policy)` is that policy as it is, waits included. A `Retry-After` of a `429` is still waited for: script it short. |
| `server` | `ServerOptions::default()` | API only: no admin UI is served. |
| `transport` | A new `MockTransport` | Transport shared with other contexts or prepared in advance. |
| `shopify_endpoints` | `ShopifyEndpoints::new()` | The [Shopify endpoints](../guides/custom-shopify-endpoints.md) of the application, field by field over the ones its configuration sets: the Admin API and OAuth at another origin, another App Bridge or Polaris in the admin UI. The requests still go to the mock transport. `SHOPIFY_ADMIN_ORIGIN` and the other variables of the endpoints are not read in a test. |
| `partner_api` | `PartnerApi::new()` | The [Partner API](billing.md#the-partner-api) of the application, field by field over the one its configuration sets. With a Partner ID or a token, billing reads the plans from the Partner API, which `ctx.shopify` answers; the one that is missing is `TEST_PARTNER_ID` (`1234567`) or `TEST_PARTNER_API_TOKEN` (`prtapi_test`). `SHOPIFY_PARTNER_ID` and `SHOPIFY_PARTNER_API_TOKEN` are not read in a test. `plan_ttl(Duration::ZERO)` keeps no plan, for a test that changes it. |

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
| `shopify` | The `FakeShopify` that answers the GraphQL operations the application sends, by name. See [Answer the Admin API](#answer-the-admin-api). |
| `transport` | The `MockTransport` behind every Shopify HTTP call. |
| `http` | A `reqwest::Client` that does not follow redirects, for unauthenticated requests. |
| `run_flow(flow, input)` / `run_flow_for(flow, input, shop)` | Executes a flow immediately with the fakes. |
| `dispatch_flow(flow, input)` / `dispatch_flow_for(flow, input, shop)` | Queues a run and returns its `Uuid`. |
| `proxy()` / `proxy_for(shop, secret)` | App proxy client that signs requests. |
| `call_loader(path, name, input)` / `call_loader_as(path, name, input, shop, sub)` | Calls a loader of the admin page at `path` as its view does, for staff user `1` of the shop of the test or `sub` of `shop`. See [Test admin functions](#test-admin-functions). |
| `call_action(path, name, input)` / `call_action_as(path, name, input, shop, sub)` | The same for an action. |
| `webhook(topic, payload)` | A delivery of a webhook to `/webhooks`, signed with the secret of the test and with the headers Shopify sends. See [Deliver a webhook](#deliver-a-webhook). |
| `get`, `post`, `put`, `patch`, `delete`, `request(method, path)` | Request builders authenticated with a session token for the default shop. |
| `request_as(method, path, shop, sub)` | Request authenticated as staff user `sub` of `shop`. |
| `session_token()` / `session_token_for(shop, sub)` | A signed session token, valid for one minute. |
| `authorization_header()` / `authorization_header_for(shop, sub)` | `Bearer <session token>`. |
| `reset_fakes()` | Resets every provider fake. |
| `shutdown()` | Closes the server and drops the schema. `#[openshop::test]` calls it. |

## Test a flow

```rust,ignore
use my_app::APP;
use my_app::flows::sync_orders::sync_orders;
use my_app::providers::warehouse::Warehouse;
use openshop::prelude::*;
use openshop::testing::TestContext;

#[openshop::test(APP)]
async fn syncs_orders(ctx: &TestContext) {
    // `query RecentOrders { orders(…) { … } }`, the operation the flow sends.
    ctx.shopify.on(
        "RecentOrders",
        json!({ "orders": { "edges": [
            { "node": { "id": "gid://shopify/Order/1", "name": "#1001" } },
        ] } }),
    );
    // The fake takes what `push` returns, and gives back what it received.
    let push = ctx.fakes.for_call(Warehouse::PUSH);
    push.returns(1);

    let outcome = ctx.run_flow(sync_orders::NAME, json!({ "limit": 10 })).await.unwrap();

    assert_eq!(outcome, RunOutcome::Completed);
    assert_eq!(ctx.shopify.operations(), ["RecentOrders"]);
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

Shopify GraphQL calls go to `ctx.transport`, and the operations it knows by
name are answered by [`ctx.shopify`](#answer-the-admin-api). What it does not
answer goes to the responder of the transport:

| `MockTransport` | Contract |
| --- | --- |
| `respond_with(responder)` | Sets the responder, a closure that receives the `OutboundRequest` (`method`, `url`, `headers`, `body`: the JSON of the request, or the fields of a form as an object of strings) and returns a `Result<OutboundResponse>`. An `Err` is a request that did not reach Shopify. It answers what `ctx.shopify` does not. |
| `respond_json(body)` | Answers every request `ctx.shopify` does not with status `200` and this JSON body. |
| `shopify()` | The `FakeShopify` of the transport: `ctx.shopify`. |
| `MockTransport::json(status, body)` | Builds a JSON `OutboundResponse`. |
| `delay_responses(delay)` | Makes every answer wait, as a slow Shopify would. |
| `requests()`, `last_request()` | The recorded requests, every one of them. |
| `clear()` | Forgets the recorded requests. |

Without a responder, a GraphQL operation nothing answers gets a GraphQL error
that names it (see [below](#answer-the-admin-api)), and any other request is
answered with `404` and the body `not mocked`. The transport also receives the
OAuth token requests made by the server. `MockTransport` is a `shopify_api`
`Transport`: every request of the `shopify_api` client goes through it.

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

## Answer the Admin API

`ctx.shopify` is the Admin API of the test: a `FakeShopify` that answers each
GraphQL operation the application sends by its name, the one of
`query Name { … }` or `mutation Name(…) { … }` (or the `operationName` of the
request), and records what it was sent. There is nothing to match in the text
of a query.

```rust,ignore
use my_app::APP;
use openshop::prelude::*;
use openshop::testing::{TestContext, subscription};

#[openshop::test(APP)]
async fn hides_paypal_for_a_paying_shop(ctx: &TestContext) {
    ctx.shopify
        // What `billing::require_plan` and `active_subscriptions()` ask.
        .active_subscriptions([subscription("Monthly")])
        .on("CurrentCustomization", json!({ "paymentCustomizations": { "nodes": [] } }))
        .on_with("CreateCustomization", |call| {
            assert_eq!(call.variables["input"]["enabled"], true);
            Ok(json!({ "paymentCustomizationCreate": { "userErrors": [] } }))
        });

    let outcome = ctx.run_flow("hidePaypal", json!({})).await.unwrap();

    assert_eq!(outcome, RunOutcome::Completed);
    assert_eq!(
        ctx.shopify.operations(),
        ["OpenShopActiveSubscriptions", "CurrentCustomization", "CreateCustomization"]
    );
}
```

| `FakeShopify` | Contract |
| --- | --- |
| `on(operation, data)` | Answers the operation with `{ "data": data }`, status `200`. A later answer of the same operation replaces it. |
| `on_variables(operation, variables, data)` | The same, for a request whose variables contain `variables`: each key given, with the same value, an object compared key by key. Chosen before an answer without variables. |
| `on_with(operation, answer)` | Answers with what `answer` makes of the `GraphqlCall`: `Ok(data)` is `{ "data": data }`, an `Err` a GraphQL error with its message. For a shop whose state the mutations change: the closure keeps it, in an `Arc<Mutex<…>>`. |
| `respond(operation, response)` | Answers with an `OutboundResponse` as it is: `MockTransport::json(503, json!({}))` for a Shopify that fails, `json!({ "errors": … })` for a refusal. |
| `active_subscriptions(subscriptions)` | Answers `OpenShopActiveSubscriptions`, what `ctx.shopify.active_subscriptions()` asks, and `billing::require_plan`, `billing::any_plan` and `active_plans()` when they read the Admin API, with these `AppSubscription`s. `[]` is a shop without a plan. |
| `active_subscription(subscription)` | Answers `OpenShopPartnerSubscription`, the `activeSubscription` of the Partner API that the billing helpers read when the Partner API is set, with this `AppPricingSubscription`; `None` is a shop without a plan. Also answers `OpenShopPartnerIds` with `TEST_APP_GID` and `TEST_SHOP_GID`, unless the test answers it. |
| `calls()`, `calls_to(operation)` | The requests, oldest first, as `GraphqlCall`s: `operation` (`None` for an anonymous one), `query`, `variables` (`{}` without), `url`. |
| `operations()` | The names of the operations sent, in order; `(anonymous)` for one without a name. |
| `unanswered()` | The operations nothing answered, once each. |
| `assert_answered()` | Panics when an operation was not answered, naming it: for code that swallows the error it got. |
| `reset()` | Forgets the answers and the calls. |

The methods that answer return the fake, to chain them. Each answer applies to
every call of the operation until another replaces it.

An operation `ctx.shopify` does not answer goes to the responder of the
transport, when the test set one. Without a responder, Shopify answers it with
a GraphQL error, which fails the call that sent it:

```text
[openshop] The test does not answer the Admin API operation `CurrentCustomization`: answer it with `ctx.shopify.on("CurrentCustomization", data)`. Answered: OpenShopActiveSubscriptions.
```

`ctx.shopify` answers the Partner API too, by the same names: a call to it
has the address of the Partner API in its `url`,
`https://partners.shopify.com/{partner_id}/api/2026-10/graphql.json`, and an
operation it does not answer is a "Partner API operation" in the error.

`openshop::testing::subscription(name)` is an active test subscription to the
plan `name`, as `activeSubscriptions` lists one
(`gid://shopify/AppSubscription/1`, no trial, created now, no line). Its
fields are public:

```rust,ignore
use openshop::billing::{AppSubscription, SubscriptionStatus};
use openshop::testing::subscription;

// A frozen shop: `require_plan` sends it to the pricing page.
ctx.shopify.active_subscriptions([AppSubscription {
    status: SubscriptionStatus::Frozen,
    ..subscription("Monthly")
}]);
```

`openshop::testing::app_pricing_subscription(handle)` is the same for the
Partner API: a subscription to the plan `handle`, every 30 days, out of its
trial, with one item of that handle and description and a flat rate of
`10.00` USD. Its fields are public too:

```rust,ignore
use openshop::billing::AppPricingSubscription;
use openshop::testing::app_pricing_subscription;

// A shop in the trial of the Pro plan.
ctx.shopify.active_subscription(AppPricingSubscription {
    trial_ends_at: Some(chrono::Utc::now() + chrono::Duration::days(7)),
    ..app_pricing_subscription("pro")
});
```

The plan of a shop read from the Partner API is kept for the `plan_ttl` of the
Partner API, as in production: a test that changes it sets
`plan_ttl(Duration::ZERO)` in [`TestOptions::partner_api`](#testoptions).

A `5xx`, a network failure or a throttled answer is retried as in
production, with the [retry policy](#testoptions) of the test, which does not
wait: `ctx.shopify.respond("Orders", MockTransport::json(503, json!({})))`
makes the query `Orders` fail after its five attempts, at once (a mutation is
sent once, as in production).

### The operations OpenShop sends

The operations of the application are the ones it writes. OpenShop sends
these of its own, and a test that reaches them answers them by these names:

| Operation | Sent by | Answer |
| --- | --- | --- |
| `OpenShopActiveSubscriptions` (`billing::ACTIVE_SUBSCRIPTIONS_OPERATION`) | `ShopifyClient::active_subscriptions()`; `active_plans()`, `billing::require_plan` and `billing::any_plan` on the Admin API | `ctx.shopify.active_subscriptions([…])` |
| `OpenShopPartnerIds` (`billing::PARTNER_IDS_OPERATION`), to the Admin API | The first plan read of a shop from the Partner API, for the GIDs of the app and of the shop | `ctx.shopify.active_subscription(…)` answers it, or `on(…)` with `shop { id }` and `currentAppInstallation { app { id } }` |
| `OpenShopPartnerSubscription` (`billing::PARTNER_SUBSCRIPTION_OPERATION`), to the Partner API | `active_plans()`, `refresh_plans()`, `billing::require_plan` and `billing::any_plan` on the Partner API | `ctx.shopify.active_subscription(…)` |
| `ListShopifyFunctions`, `ListCartTransformInstances` | The built-in Functions page, listing the instances of a cart transform | `on(…)`, with the `shopifyFunctions` and `cartTransforms` connections |
| `ListDiscountInstances` | The same page, for a discount Function | `on(…)`, with `discountNodes` |
| `ListInstances` | The same page, for the other Functions | `on(…)`, with the connection of the type: `paymentCustomizations`, `deliveryCustomizations`, `validations` or `fulfillmentConstraintRules` |
| `CreateAutoDiscount`, `CreateCodeDiscount`, `CreateCartTransform`, `CreateFulfillmentConstraint`, `CreateDeliveryCustomization`, `CreatePaymentCustomization`, `CreateValidation` | The same page, creating an instance | `on(…)`, with the payload of the mutation and its `userErrors` |
| `UpdateAutoDiscount`, `UpdateCodeDiscount`, `UpdateDeliveryCustomization`, `UpdatePaymentCustomization`, `UpdateValidation` | The same page, updating one | The same |
| `DeleteFunctionInstance` | The same page, deleting one | The same |

The `openshop.admin.graphql` tool of [MCP](mcp.md) sends the operation its caller
wrote. The token requests (OAuth, the token exchange, the client credentials
grant) are not GraphQL: they go to the responder of the transport.

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

    println!("{} {}", push.called(), push.call_count());
    let last: Option<Vec<Order>> = push.last_call().map(|call| call.args);
    let all: Vec<Vec<Order>> = push.calls().into_iter().map(|call| call.args).collect();
    println!("{} {}", last.is_some(), all.len());

    push.reset();
}
```

| `TypedFake` | Contract |
| --- | --- |
| `returns(value)` | Answers every call with `value`, of the type the method returns. |
| `rejects(error)` | Makes every call fail, whatever `returns` gave. |
| `called()`, `call_count()` | Whether, and how many times, the method was called. |
| `calls()`, `last_call()` | The recorded calls, oldest first, as `TypedCall`s: `args` has the type of the arguments of the method. A method with several arguments gives them as a tuple, one without argument as `()`. |
| `reset()` | Forgets the recorded calls, `returns` and `rejects`. |

`ctx.fakes.method("warehouse", "push")` returns the same fake without the
types, a `FakeMethod`. It takes any serializable value and records the
arguments as JSON:

```rust,ignore
use openshop::prelude::*;
use openshop::testing::TestContext;

fn configure(ctx: &TestContext) {
    let push = ctx.fakes.method("warehouse", "push");

    push.returns(json!({ "accepted": true }));
    push.rejects("warehouse offline");

    println!("{} {}", push.called(), push.call_count());
    println!("{:?} {:?}", push.calls(), push.last_call());

    push.reset();
    ctx.reset_fakes();
}
```

A call fails with the error of `rejects` when there is one, and returns the
value of `returns` otherwise; a fake given neither returns `null`.
`ctx.fakes.for_call(…)` and `ctx.fakes.method(…)` panic for an unknown provider
or method.

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

## Deliver a webhook

`ctx.webhook(topic, payload)` delivers a webhook to the application as Shopify
delivers it: `payload` serialized as the JSON body, signed with the secret of
the test, without a session token.

```rust,ignore
use my_app::APP;
use openshop::prelude::*;
use openshop::testing::TestContext;

#[openshop::test(APP)]
async fn an_order_is_received(ctx: &TestContext) {
    let response = ctx
        .webhook("orders/create", json!({ "id": 1, "name": "#1001" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);

    // What Shopify did not send is refused.
    let forged = ctx
        .webhook("orders/create", json!({ "id": 1 }))
        .signature("forged")
        .send()
        .await
        .unwrap();
    assert_eq!(forged.status(), 401);
}
```

| Header | Value |
| --- | --- |
| `Content-Type` | `application/json` |
| `X-Shopify-Topic` | `topic`, as Shopify writes it: `orders/create`. |
| `X-Shopify-Shop-Domain` | The shop of the test. |
| `X-Shopify-Hmac-Sha256` | The signature of the body with the secret of the test. |
| `X-Shopify-Webhook-Id`, `X-Shopify-Event-Id` | New UUIDs, for each delivery: two deliveries of one payload both run. |
| `X-Shopify-API-Version` | The version of the Admin API the application calls, `ApiVersion::DEFAULT` (`2026-10`). |
| `X-Shopify-Triggered-At` | Now, in RFC 3339. |

The `WebhookRequestBuilder` it returns takes, before `send()`:

| Method | Changes |
| --- | --- |
| `path(path)` | The route the delivery goes to, `/webhooks` by default: `/auth/webhooks/app-uninstalled`, or a [server route](server-routes.md) under `/routes/`. |
| `shop(shop)` | `X-Shopify-Shop-Domain`. |
| `secret(secret)` | The secret the body is signed with: the one of another app. A secret the application does not have is answered `401`. |
| `signature(value)` | `X-Shopify-Hmac-Sha256`, as given: a forged signature, answered `401`. |
| `webhook_id(id)` | `X-Shopify-Webhook-Id`: the same id and body sent again is a delivery Shopify retries, which runs once. |
| `header(name, value)` | Any header, in place of the one of the same name: `header("X-Shopify-API-Version", "2026-07")`. |

`send()` returns the `reqwest::Response` of the application. The
[webhooks reference](webhooks.md#response-behavior) lists what it answers.

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

`sign_webhook_body` signs a body `ctx.webhook` does not make from a value,
such as a delivery recorded byte for byte:

```rust,ignore
use my_app::APP;
use openshop::testing::{TestContext, sign_webhook_body};

#[openshop::test(APP)]
async fn replays_a_recorded_delivery(ctx: &TestContext) {
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

`ctx.call_loader` and `ctx.call_action` call a loader or an action of a
custom admin page as the view of the page does, without HTTP, and return what
it returns, typed, or the error the page receives:

```rust,ignore
use my_app::APP;
use my_app::admin::reviews::{Review, ReviewPage};
use openshop::billing::PLAN_REQUIRED;
use openshop::prelude::*;
use openshop::testing::{TestContext, subscription};

#[openshop::test(APP)]
async fn saves_a_review_with_a_plan(ctx: &TestContext) {
    ctx.shopify.active_subscriptions([subscription("Pro")]);
    // The page `/reviews/:id`, at the path of the review 42.
    let review: Review = ctx.call_loader("/reviews/42", "review", ()).await.unwrap();
    assert_eq!(review.id, 42);

    let saved: Review = ctx
        .call_action("/reviews/42", "saveReview", json!({ "title": "Great" }))
        .await
        .unwrap();
    assert_eq!(saved.title, "Great");

    // `#[action(authorize = billing::any_plan)]`, without a plan.
    ctx.shopify.active_subscriptions([]);
    let error = ctx
        .call_action::<Review>("/reviews/42", "saveReview", json!({ "title": "Great" }))
        .await
        .unwrap_err();
    assert_eq!((error.code.as_str(), error.status), (PLAN_REQUIRED, 402));
    assert!(error.details.unwrap().contains_key("pricingLink"));
}
```

| Method | Calls |
| --- | --- |
| `call_loader::<T>(path, name, input)` | The loader `name` of the page at `path`, as staff user `1` of the shop of the test. |
| `call_loader_as::<T>(path, name, input, shop, sub)` | The same, as staff user `sub` of `shop`. |
| `call_action::<T>(path, name, input)`, `call_action_as::<T>(path, name, input, shop, sub)` | The action `name`, the same way. |

`path` is a path of the admin, as the address bar shows it: `/` for the home,
`ReviewsPage::PATH` for a static page, `/reviews/42` for the page
`/reviews/:id`, whose `ctx.params["id"]` is then `42`. `name` is the name the
view calls the function by: `saveReview` for a function `save_review` marked
`#[action]`. `input` is any serializable value; `()` for none. The call is
the one of the RPC of the page: for the Shopify app `default`, the access
rules (`#[access]`) of the page and of the pages it is nested under, then the
`authorize` rule of the function, its input schema, then the function, whose
output is deserialized into `T`.

The error is an `AdminPublicError`, the one the page receives, with its
`code`, `status`, `message`, `field_errors` and `details`:

| Error | When |
| --- | --- |
| The function's own | It, or a rule, returned an `AdminPublicError`: `402 PLAN_REQUIRED` of `billing::require_plan` and `billing::any_plan`. |
| `403 FORBIDDEN` | An access rule or the `authorize` rule answered `false`. |
| `422 INVALID_INPUT` | The input does not fit the schema of the function. |
| `500 INTERNAL_ERROR` | Any other error. Its `message` is the error, for the test; the page receives `Internal server error`. |

A path that matches no page, a page without a loader (or an action) of that
name, or an output that is not a `T` panics, with the pages or the functions
there are.

### Without a context

`admin_context(&app, options)` builds the `AdminContext` a function receives,
to call a function, or code that takes a context, directly:

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
`admin_pages` list of `app!`, and the `functions` of a page are its loaders
and actions by name. The context has `ctx.db`, `ctx.shopify` and
`ctx.connectors` for the shop of the options. `ctx.shopify` is the client of
an admin request, as in production: for that shop and app, a token the Admin
API refuses is renewed once with the
[client credentials grant](authentication.md#client-credentials-grant), from
the mock transport.
`AdminTestOptions` has the fields `shop` (`test.myshopify.com`),
`shopify_app` (`default`), `actor_id` (`test-actor`), `params` (empty), and
`request_id` (`test-request`). `invoke_admin_function` applies the function's
own `authorize` rule and input schema, then deserializes the output: its
errors are `anyhow` errors, not the ones the page receives, and it does not
evaluate page access rules (`#[access]`), which `ctx.call_loader` does.

## Test runner integration

Tests run with `cargo test` (or `openshop test`). They need a reachable
PostgreSQL database. `#[openshop::test]` runs a test on a runtime of its own; a
test that starts its context by hand needs an async test attribute such as
`#[tokio::test]`.

The context does not start a worker or the scheduler, and it does not make real
Shopify requests: they go to its `MockTransport`. `shutdown()` closes the HTTP
server and drops the context's schema.

To run a flow or a cron by hand against the development database, outside the
tests, see [`openshop run`](cli.md#run-a-flow-or-a-cron-by-hand).
