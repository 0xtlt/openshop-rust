# Public SDK

Import the supported application API from the root of the `openshop` crate:

```rust,ignore
use openshop::{
    CronEntry, FieldDef, Flow, OpenShop, OpenShopConfig, Provider, Result, define_model,
    get_runtime_logger, json, set_runtime_logger, validators,
};
```

The items documented in the reference pages are public. Other public modules
of the crate, such as `openshop::server::*` internals, exist to support the
framework and its tests and can change without notice.

## Crate entry points

| Path | Purpose | Detailed contract |
| --- | --- | --- |
| `openshop` | App definitions, the configuration builder, the `OpenShop` handle, dispatch, the Shopify client, and runtime logging | This page |
| `openshop::cli` | `run`, `run_with_args`, `init_logging`, and `env::load_env_file` | [CLI commands](cli.md) |
| `openshop::db` | `connect`, `connect_to`, `pool_options`, models, and the `migrate` functions | [Database](database.md) |
| `openshop::engine` | `Worker`, `Scheduler`, `run_flow`, `RunFlowResult`, default policies, and typed errors | [Flows](flows.md), [Errors](errors.md) |
| `openshop::admin` | Server side of experimental custom admin pages | [Custom admin pages](custom-admin-pages.md) |
| `openshop::response` | Helpers and re-exports for building HTTP responses | [Public server routes](server-routes.md) |
| `openshop::validators` | Ready-made field validators | [Providers](providers.md#validators) |
| `openshop::runtime` | `RuntimeEnv`, the runtime logger, and the `HttpTransport` trait | [Logging](logging.md) |
| `openshop::testing` | Contexts, provider fakes, factories, and signing helpers (cargo feature `testing`) | [Testing](testing.md) |

The client side of custom admin pages lives in a separate crate,
`openshop-ui`, compiled to WebAssembly. See
[Custom admin pages](custom-admin-pages.md).

## Cargo features

| Feature | Default | Effect |
| --- | --- | --- |
| `embedded-ui` | Yes | Embeds the prebuilt admin UI bundle in the binary. Without it, `UiSource::Embedded` serves nothing. |
| `schema` | Yes | Enables `Flow::input::<T>()`, `InputSchema::of::<T>()`, and the typed admin function helpers, which derive validation from `schemars`. |
| `testing` | No | Compiles `openshop::testing`. Enable it under `[dev-dependencies]`. |

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
| `CancellationToken` | `tokio_util` |
| `chrono` | `chrono` |
| `JsonSchema`, `schemars` | `schemars` (feature `schema`) |

`#[derive(Deserialize)]` and `#[derive(JsonSchema)]` expand to paths that
start with `serde` and `schemars`, so an app that derives typed flow inputs
declares both crates in its own `Cargo.toml`. The same applies to `tokio` for
`#[tokio::main]`.

## `OpenShopConfig::builder()`

Collects the definitions of an app and validates them as a whole.

```rust,ignore
use openshop::{OpenShopConfig, PartialRetryPolicy, PartialWorkerConfig, Result};

fn config() -> Result<OpenShopConfig> {
    OpenShopConfig::builder()
        .provider(providers::warehouse::warehouse())
        .flow(flows::sync_orders::sync_orders())
        .worker(PartialWorkerConfig::new().concurrency(5))
        .retry_policy(PartialRetryPolicy::new().max_attempts(3))
        .on_error(|error, context| async move {
            eprintln!("{context:?}: {error:#}");
        })
        .build()
}
```

[Configuration](configuration.md) lists every builder method. The definitions
it accepts are created with:

- `Flow::new(name, run)` — see [Flows](flows.md);
- `Provider::new(name)` — see [Providers](providers.md);
- `FunctionDefinition::new(function_type, handle)` — see [Shopify Functions](shopify-functions.md);
- `ProxyDefinition::json()`, `::liquid()`, `::html()` — see [Proxy routes](proxy-routes.md);
- `ServerRoute::public()`, `ServerRoute::authenticated(auth)` — see [Public server routes](server-routes.md);
- `Webhook::new(run)` — see [Webhooks](webhooks.md);
- `CronEntry::new(schedule, flow)` — see [Crons](configuration.md#crons);
- `McpConfig::new()` — see [MCP](mcp.md);
- `AdminPage::new(path)` — see [Custom admin pages](custom-admin-pages.md);
- `#[derive(Model)]` and `define_model(table)` — see [Database](database.md#app-models).

## `Provider::new(name)`

A provider is a struct: `#[derive(ProviderConfig)]` makes its fields the
configuration form, and `#[provider("name")]` makes the public `async` methods
of its `impl` block the methods flows call.

```rust,ignore
use openshop::{ProviderConfig, Result, provider, validators};

#[derive(ProviderConfig)]
pub struct Warehouse {
    #[field(label = "Endpoint", validate = validators::url())]
    endpoint: String,
    #[field(label = "Token", password, validate = validators::non_empty())]
    token: String,
}

#[provider("warehouse")]
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

Register `Warehouse::provider()`. In a method, `self` is the configuration
the shop saved; flows call it with
`Warehouse::on(&ctx.connectors).push("gid://shopify/Order/1".to_string()).await?`.
`Provider::new(name)` builds a provider without a struct, with `.field(…)`,
`.method(…)` and `.checker(…)`; `ProviderCall<A, R>` is the typed reference to
a method that both forms share. See [Providers](providers.md).

OpenShop does not ship an HTTP client for your own services. Add the one you
prefer to your `Cargo.toml`.

## Cron schedules

`CronEntry::new(schedule, flow)` accepts a standard five-field cron expression,
a six-field expression with seconds, or a nickname. The expression is validated
when the configuration is built.

```rust,ignore
use openshop::{CronEntry, json};

fn crons() -> Vec<CronEntry> {
    vec![
        CronEntry::new("0 3 * * *", "syncOrders").name("Nightly sync").shops("all"),
        CronEntry::new("@hourly", "syncOrders").input(json!({ "limit": 10 })),
    ]
}
```

Supported nicknames are `@yearly`, `@annually`, `@monthly`, `@weekly`, `@daily`,
and `@hourly`.

## `OpenShop`

A running application: the validated configuration plus the database pool and
the captured environment. It is cheap to clone, and every handler context
carries one as `ctx.app`.

| Constructor | Behavior |
| --- | --- |
| `OpenShop::connect(config).await` | Connects to `DATABASE_URL` and reads settings from the environment. |
| `OpenShop::new(config, db, env)` | Uses an existing `PgPool` and `RuntimeEnv`. |
| `OpenShop::with_transport(config, db, env, transport)` | Same, with a custom `HttpTransport` for Shopify HTTP calls. |

All three validate the configuration and resolve the Shopify apps, and fail
when either is invalid. `openshop::cli::run` creates the handle for you.

| Method | Returns |
| --- | --- |
| `config()` | `&OpenShopConfig` |
| `db()` | `&PgPool` |
| `env()` | `&RuntimeEnv` |
| `dispatch_flow(params).await` | `Result<DispatchFlowResult>` |
| `connectors(shop, shopify_app).await` | `Result<Connectors>` bound to the shop's stored provider configuration |
| `shopify_client(shop, shopify_app).await` | `Result<ShopifyClient>` using the shop's stored access token |
| `mcp_registry()` | Core and application MCP permissions, tools, and resources |

## `app.dispatch_flow(params)`

Adds a flow run to PostgreSQL. A worker must be running to execute it.

```rust,ignore
use openshop::{DispatchFlowParams, DispatchOptions, OpenShop, PartialRetryPolicy, Result, json};

async fn queue_sync(app: &OpenShop) -> Result<()> {
    let queued = app
        .dispatch_flow(
            DispatchFlowParams::new("syncOrders", "example.myshopify.com")
                .input(json!({ "limit": 50 }))
                .shopify_app("default")
                .options(DispatchOptions {
                    delay_ms: Some(5_000),
                    retry_policy: Some(PartialRetryPolicy::new().max_attempts(5)),
                }),
        )
        .await?;
    println!("queued run {}", queued.run_id);
    Ok(())
}
```

| Field | Set with | Type | Notes |
| --- | --- | --- | --- |
| `flow_name` | `DispatchFlowParams::new` | `String` | Registered flow key. |
| `shop` | `DispatchFlowParams::new` | `String` | Target `*.myshopify.com` domain, or the cron global sentinel when used internally. |
| `input` | `.input(…)` | `serde_json::Value` | Defaults to `{}`. Validated by the worker, not at dispatch. |
| `shopify_app` | `.shopify_app(…)` | `String` | Defaults to the `default` app handle. |
| `parent_run_id` | `.parent_run_id(…)` | `String` | Optional parent run relationship. |
| `options.delay_ms` | `.options(…)` | `Option<u64>` | Delays the initial `available_at`. |
| `options.retry_policy` | `.options(…)` | `Option<PartialRetryPolicy>` | Highest-priority retry override for this run. |

Returns `DispatchFlowResult { run_id, status }`, where `run_id` is a `Uuid` and
`status` is `"pending"`. It fails when the flow is not registered
(`Flow "<name>" not found. Available: …`) or, with a `FlowConcurrencyError`,
when a flow with `Concurrency::Reject` already has an active run for the same
app and shop. See [Errors](errors.md#flow-errors).

The free function `openshop::dispatch_flow(db, config, params)` does the same
without an `OpenShop` handle.

## `ShopifyClient`

A Shopify Admin GraphQL client for one installed shop. It reads the offline
access token from the installations table and decrypts it.

```rust,ignore
use openshop::{OpenShop, Result, json};
use serde::Deserialize;

#[derive(Deserialize)]
struct ProductTitle {
    product: Option<Product>,
}

#[derive(Deserialize)]
struct Product {
    title: String,
}

const PRODUCT_TITLE: &str = "#graphql
  query ProductTitle($id: ID!) {
    product(id: $id) {
      id
      title
    }
  }
";

async fn product_title(app: &OpenShop, id: &str) -> Result<Option<String>> {
    let shopify = app.shopify_client("example.myshopify.com", "default").await?;

    // Untyped: the `data` object as a `serde_json::Value`.
    let data = shopify.graphql(PRODUCT_TITLE, Some(json!({ "id": id }))).await?;
    let untyped = data["product"]["title"].as_str().map(str::to_string);

    // Typed: the `data` object deserialized into your own type.
    let typed: ProductTitle = shopify
        .graphql_as(PRODUCT_TITLE, Some(json!({ "id": id })))
        .await?;

    Ok(typed.product.map(|product| product.title).or(untyped))
}
```

| Item | Contract |
| --- | --- |
| `app.shopify_client(shop, shopify_app)` | Client for the default API version. |
| `create_shopify_client(&app, shop, shopify_app, api_version)` | Same, with `api_version: Option<&str>`. `None` selects `SHOPIFY_API_VERSION`, currently `2026-04`. |
| `client.graphql(query, variables)` | `variables: Option<Value>`. Returns Shopify's `data` object as a `Value`. |
| `client.graphql_as::<T>(query, variables)` | Same, deserialized into `T: DeserializeOwned`. |
| `client.execute(graphql!(…))` | An operation checked against Shopify's schema at compile time, with typed variables and a typed result. See [Typed GraphQL](graphql.md). |
| `client.shop`, `client.shopify_app`, `client.api_version` | Public fields. |

In flows and admin functions the client is already available as `ctx.shopify`.

`graphql()`:

- fails when no installation access token exists, with
  `[openshop] No access token found for app "<app>" and shop "<shop>". Is the app installed?`.
  Creating the client does not fail; the first call does;
- POSTs to `https://<shop>/admin/api/<version>/graphql.json` with the
  `X-Shopify-Access-Token` header;
- fails on non-2xx HTTP responses with
  `[openshop] Shopify GraphQL error (<status>): <body>`;
- fails when the top-level GraphQL `errors` array is non-empty, with
  `[openshop] GraphQL errors: <messages>`;
- returns the contents of `data` otherwise. Do not read `data["data"]`.

Mutation-specific `userErrors` live inside `data` and are not raised
automatically. Inspect them in application code.

`graphql()` and `graphql_as()` take any string and are not checked when the
crate compiles: a response that does not match the type passed to `graphql_as`
is an error at run time. For an operation known in advance, use
[`graphql!`](graphql.md), which checks it against Shopify's schema and types
its result.

Requests use a 10-second connect timeout and a 60-second total timeout.

## Database access

`ctx.db` in flows, admin functions, and MCP tools, and `app.db()` everywhere
else, give the shared `sqlx` PostgreSQL pool. `openshop::sqlx` re-exports the
`sqlx` crate.

```rust,ignore
use openshop::{PgPool, Result, sqlx};

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

## `#[derive(Model)]` and `define_model(table)`

`#[derive(Model)]` maps a struct to an application-owned PostgreSQL table: its
fields are the columns, it gets typed rows limited to a shop, and
`migrate generate` writes the migrations when it changes.

```rust,ignore
use openshop::chrono::{DateTime, Utc};
use openshop::{Model, Uuid};

#[derive(Debug, Clone, Model)]
#[model(table = "reviews")]
pub struct Review {
    pub id: Uuid,
    pub shop: String,
    pub created_at: DateTime<Utc>,
    pub product_id: String,
    pub rating: i32,
}
```

Register it with `.model_of::<Review>()`, then read and write through
`Review::for_shop(&ctx.db, &ctx.shop)`. `define_model(table)` describes a table
without a struct, with its columns as SQL definitions, and is registered with
`.model(…)`. See [App models](database.md#app-models).

## Runtime logging

`set_runtime_logger(logger)` replaces OpenShop's process-wide runtime logger and
returns the previous logger. `get_runtime_logger()` returns the current logger.

```rust,ignore
use std::sync::Arc;

use openshop::{RuntimeLogger, Value, get_runtime_logger, json, set_runtime_logger};

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
    let previous = set_runtime_logger(Arc::new(JsonLines));
    get_runtime_logger().info("OpenShop logger configured", None);

    // Restore in a test or during shutdown.
    set_runtime_logger(previous);
}
```

This logger handles framework and process messages. Flow code should normally
use `ctx.logger`, which persists records in the run log. See
[Logging](logging.md).

## Server, worker, and scheduler

`openshop::cli::run` covers the standard processes. To embed OpenShop in a
process you manage yourself, the same pieces are exported:

```rust,ignore
use std::future::Future;

use openshop::{
    OpenShop, OpenShopConfig, PartialWorkerConfig, Result, Scheduler, ServerOptions, Worker,
    start_api_server,
};

async fn serve(config: OpenShopConfig, shutdown: impl Future<Output = ()>) -> Result<()> {
    let app = OpenShop::connect(config).await?;

    let server = start_api_server(&app, 3000, ServerOptions::production()).await?;
    let worker = Worker::new(app.clone(), PartialWorkerConfig::default());
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
| `create_server(&app, options)` | Builds the `axum::Router` without listening. |
| `start_api_server(&app, port, options)` | Binds `0.0.0.0:<port>` (`0` picks a free port) and returns a handle with `addr` and `shutdown()`. `shutdown()` waits up to five seconds for requests in flight, then drops the remaining connections. |
| `ServerOptions::production()` | Embedded UI behind the signed-launch check. |
| `ServerOptions::development()` | Embedded UI without the signed-launch check, and CORS on `/api`. |
| `ServerOptions::default()` | API only: no UI is served, and CORS is enabled on `/api`. |
| `.ui(UiSource)` | `UiSource::Embedded`, `UiSource::Dir(path)`, `UiSource::assets(lookup)`, or `UiSource::None`. See [Serve the bundle](custom-admin-pages.md#serve-the-bundle). |
| `openshop::server::serve(listener, router)` | Serves a router built with `create_server` on an existing `tokio::net::TcpListener`. |
| `Worker::new(app, overrides)` | Worker with `start()`, `stop()`, `is_running()`, `active_count()`, and `worker_id()`. |
| `Scheduler::start(&app)` | Registers every configured cron; `stop()` or dropping the value stops them. |

`start_api_server` and `serve` do not read `OPENSHOP_UI_DIR` or the
configuration's `admin_ui`; pass the `UiSource` you want. None of these push
or migrate the database schema.

## Public type exports

The crate root exports the types for configuration, flows, providers,
webhooks, proxies, server routes, Shopify Functions, MCP, workers, retry and
dispatch options, the Shopify client, and runtime logging. Prefer importing
them from `openshop` instead of module paths.
