# Public SDK

The code of an application imports what it uses with the prelude:

```rust,ignore
use openshop::prelude::*;
```

The root of the `openshop` crate holds what an application names, the prelude
included: the attributes, `app!`, the contexts of the handlers, the settings,
and the values they exchange.

```rust,ignore
use openshop::prelude::*;
use openshop::{DispatchOptions, OpenShop, logger, set_logger};
```

What is not about writing an application is in the module it belongs to: a
block built by hand in `openshop::builder`, workers and the scheduler in
`openshop::engine`, the HTTP server in `openshop::server`.

```rust,ignore
use openshop::builder::{Field, Flow, Provider};
use openshop::engine::{Scheduler, Worker};
```

The items documented in the reference pages are public. Other public items
of the modules, such as `openshop::server::*` internals, exist to support the
framework and its tests and can change without notice.

## Crate entry points

| Path | Purpose | Detailed contract |
| --- | --- | --- |
| `openshop` | `app!` and the attributes that declare building blocks, the contexts, the settings, the `OpenShop` handle, dispatch, the Shopify client, and runtime logging | This page |
| `openshop::prelude` | What the code of an application uses, in one import | [The prelude](#the-prelude) |
| `openshop::builder` | `OpenShopBuilder` and the definitions it takes: `Flow`, `Provider`, `Field`, `Proxy`, `Route`, `Webhook`, `Function`, `McpTool`, `McpResource`, `AdminPage`, `Table`, `InputSchema` | [`OpenShopConfig::builder()`](#openshopconfigbuilder) |
| `openshop::cli` | `run`, `run_with_args`, `init_logging`, and `env::load_env_file` | [CLI commands](cli.md) |
| `openshop::db` | `connect`, `connect_to`, `pool_options`, models, and the `migrate` functions | [Database](database.md) |
| `openshop::engine` | `Worker`, `Scheduler`, `run_flow`, `cancel_run`, `StepExecutor`, default policies, `SleepSignal`, and `LeaseLostError` | [Flows](flows.md), [Errors](errors.md) |
| `openshop::admin` | Server side of experimental custom admin pages | [Custom admin pages](custom-admin-pages.md) |
| `openshop::response` | Helpers and re-exports for building HTTP responses | [Public server routes](server-routes.md) |
| `openshop::server` | `router`, `listen`, and `serve`, to embed the HTTP server | [Server, worker, and scheduler](#server-worker-and-scheduler) |
| `openshop::validators` | Ready-made field validators | [Providers](providers.md#validators) |
| `openshop::runtime` | `RuntimeEnv`, the runtime logger, and `Transport`, what Shopify requests go through | [Logging](logging.md) |
| `openshop::shopify` | `ShopifyClient`, the `shopify_api` types it takes: `ApiVersion`, `Operation`, `IntoVariable`, `Page`, and `input!` | [Typed GraphQL](graphql.md) |
| `openshop::shopify_api` | The `shopify_api` crate, which the code of `graphql!` names: `Error`, `Shopify`… | [Typed GraphQL](graphql.md) |
| `openshop::testing` | Contexts, provider fakes, and signing helpers (cargo feature `testing`) | [Testing](testing.md) |

`openshop::ui` (cargo feature `ui`) is what the view of a custom admin page
uses: `use_loader`, `use_action`, and the App Bridge helpers. The modules of
such pages are also compiled to WebAssembly, into the admin UI bundle of the
application. See [Custom admin pages](custom-admin-pages.md#views).

## The prelude

`use openshop::prelude::*;` imports what the handlers and the declaration of
an application use:

| Kind | Items |
| --- | --- |
| The declaration | `app!`, `App`, `Cron`, `McpConfig`, `RetryPolicy`, `WorkerConfig`, `ErrorContext` |
| Attributes | `flow`, `webhook`, `proxy`, `route`, `admin_page`, `mcp_tool`, `mcp_resource`, `provider`, `input` |
| Derives, with the trait of the same name | `Model`, `ProviderConfig`, `FunctionConfig` |
| Contexts | `FlowContext`, `WebhookContext`, `ProxyContext`, `RouteContext`, `RouteAuthContext`, `AdminContext`, `McpContext`, and `ShopScope`, the trait of the contexts that belong to a shop |
| Errors | `Result`, `Context`, `anyhow!`, `bail!`, and `InvalidInput`, which a proxy or a route answers with `422` |
| Types | `Value`, `Uuid`, `Liquid`, `RouteAuthOutcome`, `McpPermission`, `McpRiskLevel`, `RunOutcome`, `QueuedRun` |
| Macros | `graphql!`, `liquid!`, `json!` |
| Modules | `validators`, `response` |

`#[openshop::test]` is not part of it:
its name would hide `#[test]`, so it is written with its path. The samples of
this documentation import the prelude, then name what it lacks, such as
`use openshop::testing::TestContext;` or `use serde::Deserialize;`.

## Cargo features

| Feature | Default | Effect |
| --- | --- | --- |
| `embedded-ui` | Yes | Embeds the prebuilt admin UI bundle in the binary. Without it, `UiSource::Embedded` serves nothing. |
| `sentry` | Yes | Compiles the [Sentry](sentry.md) integration: the `sentry` setting of `app!`, `SentryConfig`, and the SDK re-exported as `openshop::sentry`. Without it the Sentry SDK is not a dependency. |
| `testing` | No | Compiles `openshop::testing` and `#[openshop::test]`. Enable it under `[dev-dependencies]`. |

## Re-exports

The crate root re-exports the third-party items that appear in its API, so an
app does not need to depend on them directly:

| Item | Origin |
| --- | --- |
| `Result`, `Error`, `Context`, `anyhow!`, `bail!` | `anyhow` |
| `Value`, `json!`, `serde_json` | `serde_json` |
| `JsonMap` | `serde_json::Map<String, Value>` |
| `IndexMap` | `indexmap` |
| `PgPool`, `sqlx` | `sqlx` |
| `Uuid` | `uuid` |
| `CancellationToken` | `tokio_util` |
| `chrono` | `chrono` |
| `serde` | `serde` |
| `JsonSchema`, `schemars` | `schemars` |
| `sentry` | The Sentry SDK (feature `sentry`); see [Report from your own code](sentry.md#report-from-your-own-code) |

[`#[input]`](flows.md#input) derives `Deserialize`, `Serialize` and
`JsonSchema` through these re-exports, and `APP.main()` starts the runtime: an
application needs neither `schemars` nor `tokio` among its dependencies.
`#[derive(Deserialize)]` and `#[derive(Serialize)]` written in the application
expand to paths that start with `serde`, so an app that derives them, for
example on the payload of a webhook, declares `serde` in its own `Cargo.toml`.

## `app!` and `App`

`app!` declares an application: it lists the building blocks, each declared
where it is written, and sets the rest. The `App` it declares is a value that
the binary runs and the tests read.

```rust,ignore
use std::sync::Arc;

use openshop::prelude::*;

use flows::sync_orders::sync_orders;
use providers::warehouse::Warehouse;

async fn report_failure(error: Arc<openshop::Error>, context: ErrorContext) {
    eprintln!("{context:?}: {error:#}");
}

pub static APP: App = app! {
    providers: [Warehouse],
    flows: [sync_orders],
    worker: WorkerConfig::new().concurrency(5),
    retry_policy: RetryPolicy::new().max_attempts(3),
    on_error: report_failure,
};

fn main() -> Result<()> {
    APP.main()
}
```

`APP.main()` starts the runtime and runs the command given on the command
line. `APP.run().await` runs it in a binary that has its own runtime.
`APP.config()` returns the validated `OpenShopConfig`, and `APP.builder()`
returns the `OpenShopBuilder` of the declaration, to add to it. Each returns a
`Result`: a setting of the declaration can fail.
[Configuration](configuration.md) describes every list and setting. The blocks
are declared with:

- `#[flow]` on a function, and `#[input]` on the type of its input — see [Flows](flows.md);
- `#[derive(ProviderConfig)]` on a struct and `#[provider]` on its `impl` block — see [Providers](providers.md);
- `#[derive(FunctionConfig)]` on a struct — see [Shopify Functions](shopify-functions.md);
- `#[proxy]` on a function — see [Proxy routes](proxy-routes.md);
- `#[route]` on a function — see [Public server routes](server-routes.md);
- `#[webhook]` on a function — see [Webhooks](webhooks.md);
- `#[mcp_tool]` and `#[mcp_resource]` on a function, with a `const McpPermission` for each permission they require — see [MCP](mcp.md);
- `#[admin_page]` on an `impl` block — see [Custom admin pages](custom-admin-pages.md);
- `#[derive(Model)]` on a struct — see [Database](database.md#app-models).

Crons are data: `Cron::new(schedule, flow)` — see
[Crons](configuration.md#crons).

### `OpenShopConfig::builder()`

The attributes are built on definitions that can be created by hand:
`Flow::new(name, run)`, `Provider::new(name)`,
`Function::new(function_type, handle)`, `Proxy::new(response_type)` and
`Proxy::default()`, `Route::public()` and
`Route::authenticated(auth)`, `Webhook::new(run)`,
`McpTool::new(description, run)`,
`McpResource::new(name, read)` and `AdminPage::new(path)`. They are in `openshop::builder`, with
`OpenShopBuilder`. `OpenShopConfig::builder()` collects them, along with
declared blocks, and `build()` validates the whole. Use it for tests and for
blocks built at run time. See [Builder methods](configuration.md#builder-methods).

## `#[provider]` and `Provider::new(name)`

A provider is a struct: `#[derive(ProviderConfig)]` makes its fields the
configuration form, and `#[provider]` makes the public `async` methods of its
`impl` block the methods flows call. The provider is named after the struct,
here `warehouse`; `#[provider("name")]` states another name.

```rust,ignore
use openshop::prelude::*;

#[derive(ProviderConfig)]
pub struct Warehouse {
    #[field(url)]
    endpoint: String,
    #[field(password, non_empty)]
    token: String,
}

#[provider]
impl Warehouse {
    #[checker]
    async fn check(&self) -> Result<bool> {
        // Call the service's health endpoint with your HTTP client.
        Ok(!self.token.is_empty())
    }

    pub async fn push(&self, order_id: String) -> Result<bool> {
        // POST the order to `{endpoint}/orders` with the stored token.
        println!("{order_id} to {}", self.endpoint);
        Ok(true)
    }
}
```

List `Warehouse` in the `providers` of `app!`. In a method, `self` is the
configuration the shop saved; flows call it with
`Warehouse::on(&ctx.connectors).push("gid://shopify/Order/1".to_string()).await?`.
`Provider::new(name)` builds a provider without a struct, with `.field(…)`,
`.method(…)` and `.checker(…)`; `ProviderCall<A, R>` is the typed reference to
a method that both forms share. See [Providers](providers.md).

OpenShop does not ship an HTTP client for your own services. Add the one you
prefer to your `Cargo.toml`.

## Cron schedules

`Cron::new(schedule, flow)` accepts a standard five-field cron expression,
a six-field expression with seconds, or a nickname. The expression is validated
when the configuration is built, and so is the `input` of the cron, against
the input of its flow.

```rust,ignore
use openshop::prelude::*;

use flows::sync_orders::sync_orders;

pub static APP: App = app! {
    flows: [sync_orders],
    crons: [
        Cron::new("0 3 * * *", sync_orders::NAME)
            .name("Nightly sync")
            .input(json!({ "limit": 1000 }))
            .all_shops(),
        Cron::new("@hourly", sync_orders::NAME)
            .input(json!({ "limit": 10 }))
            .shop("example.myshopify.com"),
    ],
};
```

Supported nicknames are `@yearly`, `@annually`, `@monthly`, `@weekly`, `@daily`,
and `@hourly`. `.all_shops()`, `.shop(…)` and `.shops([…])` choose the shops a
cron runs for; see [Cron shop targeting](configuration.md#cron-shop-targeting).

## `OpenShop`

A running application: the validated configuration plus the database pool and
the captured environment. It is cheap to clone, and every handler context
carries one as `ctx.app`.

| Constructor | Behavior |
| --- | --- |
| `OpenShop::new(config, db, env)` | Uses an existing `PgPool` and `RuntimeEnv`. |
| `OpenShop::with_transport(config, db, env, transport)` | Same, with another `shopify_api` `Transport` for every exchange with Shopify, such as `testing::MockTransport`. It must not follow redirections: every call carries a secret, a token or a code, and a `3xx` is returned as the answer, as the default `ReqwestTransport` does. |

Both validate the configuration and resolve the Shopify apps, and fail
when either is invalid. `APP.main()` and `APP.run()` create the handle for
you.

| Method | Returns |
| --- | --- |
| `config()` | `&OpenShopConfig` |
| `db()` | `&PgPool` |
| `env()` | `&RuntimeEnv` |
| `dispatch_flow(options).await` | `Result<QueuedRun>` |
| `connectors(shop, shopify_app)` | `Connectors`: the providers of the shop, bound to the configuration it saved. The configuration is read at the first call. |
| `shopify_client(shop, shopify_app)` | `ShopifyClient` of the shop. The access token is read at the first request. |
| `mcp_registry()` | Core and application MCP permissions, tools, and resources |

In a handler, use `ctx.shopify` and `ctx.connectors`: every context that
belongs to a shop has them. `connectors` and `shopify_client` are for code
without a context.

## `sync_orders::dispatch(&ctx, input)`

`#[flow]` adds `dispatch` to the type of the flow. It queues a run for the
shop and the Shopify app of a context, and returns a `QueuedRun`.

```rust,ignore
use openshop::prelude::*;

use crate::flows::sync_orders::{SyncOrdersInput, sync_orders};

#[webhook("orders/create")]
pub async fn orders_create(ctx: WebhookContext) -> Result<()> {
    let queued = sync_orders::dispatch(&ctx, SyncOrdersInput { limit: 50 }).await?;
    println!("queued run {}", queued.run_id);
    Ok(())
}
```

`ctx` is any context that belongs to a shop: the context of a flow, of a
webhook, of an app proxy, of an admin function, of an MCP tool or resource, or
what `ctx.for_shop(…)` returns in a server route. These implement the
`ShopScope` trait. The second argument is the input type of the flow, which
implements `Serialize`; [`#[input]`](flows.md#input) derives it. A flow without
input takes the context alone: `nightly_cleanup::dispatch(&ctx)`.

The run is queued as by `app.dispatch_flow(…)` below, and fails in the same
cases.

## `app.dispatch_flow(options)`

Adds a flow run to PostgreSQL. A worker must be running to execute it. Use it
for a flow only known by name, for code without a context, or to set a delay
or a retry policy for the run.

```rust,ignore
use std::time::Duration;

use openshop::prelude::*;
use openshop::{DispatchOptions, OpenShop};

async fn queue_sync(app: &OpenShop) -> Result<()> {
    let queued = app
        .dispatch_flow(
            DispatchOptions::new("syncOrders", "example.myshopify.com")
                .input(json!({ "limit": 50 }))
                .shopify_app("default")
                .delay(Duration::from_secs(5))
                .retry_policy(RetryPolicy::new().max_attempts(5)),
        )
        .await?;
    println!("queued run {}", queued.run_id);
    Ok(())
}
```

`params` is a `DispatchOptions`:

| Field | Set with | Type | Notes |
| --- | --- | --- | --- |
| `flow_name` | `DispatchOptions::new` | `String` | Registered flow key, such as `sync_orders::NAME`. |
| `shop` | `DispatchOptions::new` | `String` | Target `*.myshopify.com` domain, or the cron global sentinel when used internally. |
| `input` | `.input(…)` | `serde_json::Value` | Defaults to `{}`. A dispatch fails with `InvalidFlowInput` when it fails the input schema of the flow, and with `UnknownFlowError` for a flow that is not registered; no run is queued. |
| `shopify_app` | `.shopify_app(…)` | `String` | Defaults to the `default` app handle. |
| `parent_run_id` | `.parent_run_id(…)` | `String` | Optional parent run relationship. |
| `delay` | `.delay(…)` | `Duration` | Delays the initial `available_at`. |
| `retry_policy` | `.retry_policy(…)` | `RetryPolicy` | Highest-priority retry override for this run. |

`DispatchOptions::for_scope(flow, &ctx)` starts from the shop and the Shopify
app of a context instead, with the same methods:
`DispatchOptions::for_scope(sync_orders::NAME, &ctx).delay(Duration::from_secs(5))`.

Returns `QueuedRun { run_id, status }`, where `run_id` is a `Uuid` and
`status` is `"pending"`. It fails when the flow is not registered
(`Flow "<name>" not found. Available: …`) or, with a `FlowConcurrencyError`,
when a flow with `Concurrency::Reject` already has an active run for the same
app and shop. See [Errors](errors.md#flow-errors).

The free function `openshop::engine::dispatch_flow(db, config, params)` does the same
without an `OpenShop` handle.

## `ShopifyClient`

A Shopify Admin GraphQL client for one installed shop, on the
[`shopify_api`](https://docs.rs/shopify_api) client. It reads the offline
access token from the installations table and decrypts it, at the first
request. Every context that belongs to a shop has one as `ctx.shopify`.

```rust,ignore
use openshop::prelude::*;

const PRODUCT_TITLE: &str = "#graphql
  query ProductTitle($id: ID!) {
    product(id: $id) {
      id
      title
    }
  }
";

async fn product_title(ctx: &WebhookContext, id: &str) -> Result<Option<String>> {
    // The `data` object as a `serde_json::Value`.
    let data = ctx.shopify.graphql(PRODUCT_TITLE, json!({ "id": id })).await?;
    Ok(data["product"]["title"].as_str().map(str::to_string))
}
```

| Item | Contract |
| --- | --- |
| `ctx.shopify` | The client of the shop of a context: a flow, a webhook, an app proxy, an admin function, an MCP tool or resource, and what `ctx.for_shop(…)` returns in a server route. |
| `app.shopify_client(shop, shopify_app)` | The client of a shop, for code without a context. Returns the `ShopifyClient` itself: nothing is read before the first request. |
| `ShopifyClient::new(&app, shop, shopify_app)` | Same, from an `&OpenShop`. |
| `client.with_api_version(version)` | Sends every request to `version`, a `shopify_api::ApiVersion`, instead of the version each operation was checked against. |
| `client.graphql(query, variables)` | `variables`: anything that serializes to an object, or `()` for none. Returns Shopify's `data` object as a `Value`. |
| `client.execute(graphql!(…))` | An operation checked against Shopify's schema at compile time, with typed variables and a typed result. See [Typed GraphQL](graphql.md). |
| `client.api().await?` | The `shopify_api::Shopify` client of the shop: `paginate`, bulk operations, webhook subscriptions, `execute_response`. |
| `client.shop`, `client.shopify_app` | Public fields. |

`graphql()`:

- fails with `[openshop] Invalid shop domain for app "<app>": expected <name>.myshopify.com`,
  and sends nothing, when `client.shop` is not `<name>.myshopify.com` or a bare
  `<name>` (case and blanks aside): no scheme, path, port or user;
- fails when no installation access token exists, with
  `[openshop] No access token found for app "<app>" and shop "<shop>". Is the app installed?`.
  Creating the client does not fail; the first call does. For the shop of
  the `client_credentials_shop` setting, and for the shop and app of an admin
  request's session, the client first asks Shopify for one with the
  [client credentials grant](authentication.md#client-credentials-grant);
- renews a token the Admin API refuses with `401` once, with the same grant,
  and sends the request again once, when the shop's installation row holds
  that token. The token is read and kept per Shopify app
  and shop: a clone whose `shop` or `shopify_app` was changed reads the token
  of its own shop, and does not carry the right of an admin request's session
  to another shop or app;
- POSTs to `https://<shop>/admin/api/<version>/graphql.json` with the
  `X-Shopify-Access-Token` header, `<version>` the one of the schema of the
  crate (`[package.metadata.shopify_api]`), or the one the client pins;
- waits and retries when Shopify throttles the request, and retries a query
  after a server error; a mutation never runs twice;
- fails on non-2xx HTTP responses, and when the top-level GraphQL `errors`
  array is non-empty, with a `shopify_api::Error` (`Http`, `Graphql`) that
  `error.downcast_ref::<openshop::shopify_api::Error>()` reads;
- returns the contents of `data` otherwise. Do not read `data["data"]`.

Mutation-specific `userErrors` live inside `data` and are not raised
automatically. Inspect them in application code.

`graphql()` takes any string and is not checked when the crate compiles. For
an operation known in advance, use
[`graphql!`](graphql.md), which checks it against Shopify's schema and types
its result.

Requests use a 10-second connect timeout and a 60-second total timeout.

## Database access

`ctx.db` in every handler context, and `app.db()` everywhere else, give the
shared `sqlx` PostgreSQL pool. `openshop::sqlx` re-exports the `sqlx` crate.

```rust,ignore
use openshop::prelude::*;
use openshop::{PgPool, sqlx};

async fn five_star_reviews(db: &PgPool, shop: &str) -> Result<Vec<(String, i32)>> {
    // `shop` must come from a verified runtime context.
    Ok(
        sqlx::query_as("SELECT product_id, rating FROM reviews WHERE shop = $1 AND rating = $2")
            .bind(shop)
            .bind(5)
            .fetch_all(db)
            .await?,
    )
}
```

The pool is created once per process. See [Database](database.md).

The rows of the framework are read without SQL: `FlowRun::of(&ctx)` for the
runs of a shop, `FlowRun::all_shops(&db)` and `Installation::installed(&db)`
across shops, `StepResult::of_run(&db, id)` and `LogRow::of_run(&db, id)` for
a run. See [Read the framework tables](database.md#read-the-framework-tables).

## `#[derive(Model)]` and `Table`

`#[derive(Model)]` maps a struct to an application-owned PostgreSQL table: its
fields are the columns, it gets typed rows limited to a shop, and
`migrate generate` writes the migrations when it changes. The table is the
first argument of `#[model(…)]`.

```rust,ignore
use openshop::chrono::{DateTime, Utc};
use openshop::prelude::*;

#[derive(Debug, Clone, Model)]
#[model("reviews", index(shop, product_id))]
pub struct Review {
    pub id: Uuid,
    pub shop: String,
    pub created_at: DateTime<Utc>,
    pub product_id: String,
    pub rating: i32,
}
```

List it in the `models` of `app!`, then read and write through
`Review::of(&ctx)`, the rows of the shop of a context. `ctx` is any context
that belongs to a shop. `Review::for_shop(&ctx.db, &ctx.shop)` gives the same
rows with the database and the shop given. The derive implements the `Model`
trait, and `Review::table()` returns the `Table` the struct describes. See
[App models](database.md#app-models).

## Runtime logging

`set_logger(logger)` replaces OpenShop's process-wide runtime logger and
returns the previous logger. `logger()` returns the current logger.

```rust,ignore
use std::sync::Arc;

use openshop::prelude::*;
use openshop::{RuntimeLogger, logger, set_logger};

struct JsonLines;

impl RuntimeLogger for JsonLines {
    fn info(&self, message: &str, context: Option<&Value>) {
        println!("{}", json!({ "level": "info", "message": message, "context": context }));
    }

    fn warn(&self, message: &str, context: Option<&Value>) {
        eprintln!("{}", json!({ "level": "warn", "message": message, "context": context }));
    }

    fn error(&self, message: &str, context: Option<&Value>) {
        eprintln!("{}", json!({ "level": "error", "message": message, "context": context }));
    }
}

fn install_logger() {
    let previous = set_logger(Arc::new(JsonLines));
    logger().info("OpenShop logger configured", None);

    // Restore in a test or during shutdown.
    set_logger(previous);
}
```

This logger handles framework and process messages. Flow code should normally
use `ctx.logger`, which persists records in the run log. See
[Logging](logging.md).

## Server, worker, and scheduler

`APP.main()` and `APP.run()` cover the standard processes. To embed OpenShop in
a process you manage yourself, the same pieces are exported, and
`APP.config()?` gives them the configuration:

```rust,ignore
use std::future::Future;

use openshop::prelude::*;
use openshop::server::listen;
use openshop::engine::{Scheduler, Worker};
use openshop::runtime::RuntimeEnv;
use openshop::server::ServerOptions;
use openshop::{OpenShop, OpenShopConfig};

async fn serve(config: OpenShopConfig, shutdown: impl Future<Output = ()>) -> Result<()> {
    let db = openshop::db::connect_to(&std::env::var("DATABASE_URL")?).await?;
    let app = OpenShop::new(config, db, RuntimeEnv::from_env())?;

    let server = listen(&app, 3000, ServerOptions::production()).await?;
    let worker = Worker::new(app.clone(), WorkerConfig::new());
    worker.start().await;
    let mut scheduler = Scheduler::start(&app)?;

    shutdown.await;

    scheduler.stop();
    worker.stop().await;
    server.shutdown().await
}
```

| Item | Contract |
| --- | --- |
| `openshop::server::router(&app, options)` | Builds the `axum::Router` without listening. |
| `openshop::server::listen(&app, port, options)` | Binds `0.0.0.0:<port>` (`0` picks a free port) and returns a handle with `addr` and `shutdown()`. Keep the handle for as long as the server should run: dropping it stops accepting connections and lets requests in flight finish. `shutdown()` also waits for those requests, up to five seconds. After that it returns an error, and the requests still in flight keep running until they finish or the process exits. |
| `ServerOptions::production()` | Embedded UI behind the signed-launch check. |
| `ServerOptions::development()` | Embedded UI without the signed-launch check, and CORS on `/api`. The shell reloads the page when the server is started again or its bundle rebuilt: see [Development process](cli.md#development-process). |
| `ServerOptions::default()` | API only: no UI is served, and CORS is enabled on `/api`. |
| `.ui(UiSource)` | `UiSource::Embedded`, `UiSource::Dir(path)`, `UiSource::assets(lookup)`, or `UiSource::None`. See [Serve the bundle](custom-admin-pages.md#serve-the-bundle). |
| `openshop::server::serve(listener, router)` | Serves a router built with `router` on an existing `tokio::net::TcpListener`. Returns the same handle as `listen`: keep it for as long as the server should run. |
| `openshop::cli::shutdown_signal()` | Resolves when the process receives `SIGINT` or `SIGTERM`: the `shutdown` of the example above, for a process that stops as `dev` and `start` do. |
| `Worker::new(app, overrides)` | Worker with `start()`, `stop()`, `is_running()`, `active_count()`, and `worker_id()`. `overrides` is a `WorkerConfig`: what it sets replaces the `worker` setting of the application. |
| `Scheduler::start(&app)` | Registers every configured cron; `stop()` or dropping the value stops them. |

`listen` and `serve` do not read `OPENSHOP_UI_DIR` or the
configuration's `admin_ui`; pass the `UiSource` you want. None of these push
or migrate the database schema.

In development, a page of the admin UI keeps a request to
`/__openshop/reload` open for as long as it is displayed. Dropping the handle
or calling `shutdown()` ends those requests. A router you serve with a server
of your own does not: stop that server without waiting for the requests in
flight.

## Public type exports

The crate root exports what an application names: the contexts of the
handlers, the settings of `app!` (crons, retry policy, workers, Shopify apps,
MCP, admin pages), dispatch options and outcomes, the flow errors, the Shopify
client, the records of the framework tables (`Installation`, `FlowRun`,
`StepResult`, `LogRow`, and the trait `Record`), and runtime logging. [The prelude](#the-prelude) holds the ones most
code uses.

| What | Where |
| --- | --- |
| A block built by hand: `Flow`, `Provider`, `Field`, `Proxy`, `Route`, `Webhook`, `Function`, `McpTool`, `McpResource`, `AdminPage`, `Table`, `InputSchema`, and `OpenShopBuilder` | `openshop::builder` |
| `Worker`, `Scheduler`, `run_flow`, `RunFlowOptions`, `cancel_run`, `dispatch_flow`, `StepExecutor`, `RunOwner`, `SleepSignal`, `LeaseLostError` | `openshop::engine` |
| `router`, `listen`, `serve`, `ServerOptions`, `ShopSession` | `openshop::server` |
| `RuntimeEnv`, `Transport`, `ReqwestTransport` | `openshop::runtime` |
| `ApiVersion`, `IntoVariable`, `Operation`, `Page` | `openshop::shopify` |
| `Declared`, the trait of a declared block | `openshop::declare` |
