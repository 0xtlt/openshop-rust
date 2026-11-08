//! OpenShop: build Shopify integrations in Rust with background flows,
//! configurable service providers, and an embedded admin UI.
//!
//! The building blocks of an application are declared where they are
//! written: `#[flow]` on a function, `#[provider]` on the `impl` block of a
//! struct, `#[derive(Model)]` on a struct… [`app!`] lists them, and the
//! [`App`] it declares runs the `dev`, `start`, `worker` and `migrate`
//! commands:
//!
//! ```no_run
//! use openshop::prelude::*;
//!
//! #[flow]
//! async fn hello(ctx: FlowContext) -> Result<()> {
//!     let greeting: String = ctx.step.run("greet", async || Ok("hello".to_string())).await?;
//!     ctx.logger.info_with("Greeted", json!({ "greeting": greeting }));
//!     Ok(())
//! }
//!
//! static APP: App = app! {
//!     flows: [hello],
//! };
//!
//! fn main() -> Result<()> {
//!     APP.main()
//! }
//! ```
//!
//! A web process (`start`) accepts requests and queues work; workers (`worker`)
//! execute it. PostgreSQL stores installations, credentials, runs and checkpoints.
//!
//! The root of the crate holds what an application names: the attributes,
//! [`app!`], the contexts of the handlers, the settings and the values they
//! exchange. The rest is in its module:
//!
//! - [`builder`]: the builder of an application and the definitions it
//!   takes, for a block made by hand;
//! - [`engine`]: workers, the scheduler, running and canceling a flow;
//! - [`server`]: the HTTP server and its parts;
//! - [`db`]: the pool, models, tables and migrations;
//! - [`admin`]: the server side of custom admin pages;
//! - [`billing`]: the subscriptions of a shop to the app, with Shopify's
//!   managed pricing, and the guard of an admin page that needs a plan;
//! - [`runtime`]: the environment, the loggers, the reports to Sentry and the outbound transport;
//! - [`shopify`]: the Admin API client and typed operations;
//! - [`testing`] (feature `testing`): the test harness;
//! - `ui` (feature `ui`): what the view of a custom admin page is written with.

// The crate is also compiled to WebAssembly, into the admin UI bundle of an
// application whose admin pages have a view (feature `ui`). The bundle is
// made of the pages: the server is left out of it.
macro_rules! not_wasm {
    ($($item:item)*) => {
        $(
            #[cfg(not(target_arch = "wasm32"))]
            $item
        )*
    };
}

not_wasm! {
    pub mod admin;
    mod app;
    pub mod billing;
    pub mod builder;
    pub mod cli;
    mod config;
    pub mod db;
    pub mod declare;
    pub mod engine;
    pub mod response;
    pub mod runtime;
    pub mod server;
    pub mod shopify;
    #[cfg(feature = "testing")]
    pub mod testing;
    mod types;
    pub mod validators;
}
// Declared outside the macro: `liquid!`, which it exports, is named by its path.
#[cfg(not(target_arch = "wasm32"))]
mod template;
#[cfg(feature = "ui")]
pub mod ui;

#[cfg(all(target_arch = "wasm32", not(feature = "ui")))]
compile_error!(
    "`openshop` compiles to WebAssembly for the views of admin pages: enable its `ui` feature, `openshop = { …, features = [\"ui\"] }`"
);

/// What `#[admin_page]` writes for a `#[view]`, which needs the feature `ui`.
#[cfg(feature = "ui")]
#[doc(hidden)]
#[macro_export]
macro_rules! __view {
    ($($generated:tt)*) => {
        $($generated)*
    };
}

#[cfg(not(feature = "ui"))]
#[doc(hidden)]
#[macro_export]
macro_rules! __view {
    ($($generated:tt)*) => {
        ::core::compile_error!(
            "a `#[view]` needs the `ui` feature of `openshop`: `openshop = { …, features = [\"ui\"] }`"
        );
    };
}

not_wasm! {
    /// Locks `mutex`, even when a thread panicked while holding it: what it
    /// guards stays usable.
    pub(crate) fn locked<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

// The crates whose types appear in the API of an application.
pub use anyhow::{Context, Error, Result, anyhow, bail};
pub use schemars::{self, JsonSchema};
pub use serde;
pub use serde_json::{self, Value, json};
not_wasm! {
    pub use chrono;
    pub use indexmap::IndexMap;
    pub use sqlx::{self, PgPool};
    pub use tokio_util::sync::CancellationToken;
    /// The `tracing` crate the framework logs with, whose subscriber
    /// `APP.main()` installs: `tracing::info!(shop = %ctx.shop, "Synced")`
    /// without a dependency of the application's own. A flow, a webhook and
    /// an admin page log for their run or their shop with `ctx.logger`.
    pub use tracing;
    pub use uuid::Uuid;
    /// The Sentry SDK the framework reports with. What an application
    /// captures itself goes through it: another version of the crate would
    /// have a client of its own, which nothing starts.
    #[cfg(feature = "sentry")]
    pub use ::sentry;
}

// The application, and what its handlers receive and return. What the engine
// and the server are made of stays in their modules, and the definitions a
// block is built from by hand are in `builder`.
not_wasm! {
    pub use admin::{AdminActor, AdminContext, AdminPublicError};
    pub use app::{OpenShop, OpenShopConfig};
    pub use db::{FlowRun, Installation, LogRow, Model, Record, StepResult};
    pub use declare::{App, InvalidInput};
    pub use engine::{
        DispatchOptions, FlowCanceledError, FlowConcurrencyError, FlowTimeoutError, InvalidFlowInput, QueuedRun,
        RunOutcome, StepTimeoutError, UnknownFlowError,
    };
    pub use runtime::logger::{logger, set_logger};
    #[cfg(feature = "sentry")]
    pub use runtime::SentryConfig;
    pub use runtime::{ContextLogger, FlowLogger, RuntimeLogger};
    pub use server::UiSource;
    pub use server::http::PublicError;
    pub use server::connectors::Connectors;
    pub use shopify::{ShopifyClient, ShopifyEndpoints};
    pub use template::Liquid;
    pub use types::{
        AdminMenuItem, AdminPageId, AdminPageMode, AdminPagesConfig, Concurrency, Cron, CronShops, ErrorContext,
        FlowContext, FunctionConfig, JsonMap, McpConfig, McpContext, McpPermission, McpResourceContent, McpRiskLevel,
        ProviderConfig, ProxyAuthKind, ProxyContext, RetryPolicy, RouteAuthContext, RouteAuthOutcome, RouteContext,
        RouteRequest, RouteShopContext, RouteShopInput, ShopScope, ShopifyAppConfig, ShopifyConfig, StepOptions,
        WebhookContext, WorkerConfig,
    };
}

/// What the code of an application uses, in one import.
///
/// ```
/// use openshop::prelude::*;
///
/// #[flow]
/// async fn hello(ctx: FlowContext) -> Result<()> {
///     ctx.logger.info("Hello");
///     Ok(())
/// }
///
/// static APP: App = app! {
///     flows: [hello],
/// };
/// # let _ = &APP;
/// ```
///
/// The attributes and derives that declare building blocks, [`app!`], the
/// contexts of the handlers, and what their bodies reach for: [`Result`],
/// [`json!`], [`bail!`], [`graphql!`], [`liquid!`], the [`tracing`] crate… The attribute of tests,
/// `#[openshop::test]`, is not part of it: it would hide `#[test]`.
pub mod prelude {
    pub use crate::input;
    pub use crate::{Context, Result, Value, admin_page, anyhow, app, bail, json};
    not_wasm! {
        pub use crate::{
            AdminContext, App, Cron, ErrorContext, FlowContext, FunctionConfig, InvalidInput, Liquid, McpConfig,
            McpContext, McpPermission, McpRiskLevel, Model, ProviderConfig, ProxyContext, PublicError, QueuedRun, RetryPolicy,
            RouteAuthContext, RouteAuthOutcome, RouteContext, RunOutcome, ShopScope, ShopifyEndpoints, Uuid,
            WebhookContext, WorkerConfig, flow, graphql, liquid, mcp_resource, mcp_tool, provider, proxy, response,
            route, tracing, validators, webhook,
        };
        #[cfg(feature = "sentry")]
        pub use crate::SentryConfig;
    }
}

/// Maps a struct to an application table.
///
/// The struct is the single description of the table: its fields are the
/// columns, used for the schema push in development, for the migrations that
/// `migrate generate` writes, and for typed reads and writes.
///
/// ```
/// use openshop::chrono::{DateTime, Utc};
/// use openshop::{Model, Uuid};
///
/// #[derive(Debug, Clone, Model)]
/// #[model("reviews", index(shop, product_id))]
/// pub struct Review {
///     pub id: Uuid,
///     pub shop: String,
///     pub created_at: DateTime<Utc>,
///     pub updated_at: DateTime<Utc>,
///     pub product_id: String,
///     pub rating: i32,
///     pub note: Option<String>,
///     #[model(default = "false")]
///     pub approved: bool,
/// }
///
/// # async fn example(ctx: openshop::FlowContext) -> openshop::Result<()> {
/// // The rows of the shop a handler runs for.
/// let reviews = Review::of(&ctx);
/// let review = reviews
///     .insert(NewReview {
///         product_id: "gid://shopify/Product/1".to_string(),
///         rating: 5,
///         note: None,
///         approved: None,
///     })
///     .await?;
///
/// let best = reviews
///     .filter(Review::RATING.gte(4))
///     .order_by(Review::CREATED_AT.desc())
///     .limit(10)
///     .all()
///     .await?;
/// # let _ = (review, best);
/// # Ok(())
/// # }
/// ```
///
/// `id` is required. `shop`, `created_at` and `updated_at` are optional and
/// get the conventional definitions. With a `shop` field the struct gets
/// `of(&ctx)`, the rows of the shop a context runs for, `for_shop(&db, shop)`
/// for code without a context, and `all_shops(&db)`; without one it gets
/// `rows(&db)`. List the model in the `models` of [`app!`].
///
/// The table comes first in `#[model(…)]`, then the options of the struct:
/// `index(field, …)`, `unique(field, …)`, each with an optional `name = "…"`,
/// and `renamed_from = "…"`. Field options: `default = "<SQL>"`,
/// `sql_type = "…"`, `column = "…"`, `renamed_from = "…"`, `index`, `unique`,
/// and `references(OtherModel)` with an optional `on_delete = "cascade"` for a
/// foreign key.
///
/// The derive implements the trait [`Model`](trait@Model) and `sqlx::FromRow`,
/// and declares `New<Struct>`, the values an insertion takes.
pub use openshop_macros::Model;

/// Makes a struct the configuration of a provider: its fields are the fields
/// of the form a merchant fills, and the struct is what the methods of the
/// provider receive.
///
/// ```
/// use openshop::{ProviderConfig, Result, provider};
///
/// #[derive(ProviderConfig)]
/// pub struct Warehouse {
///     #[field(label = "API URL", url)]
///     api_url: String,
///     #[field(label = "API key", password, non_empty)]
///     api_key: String,
///     #[field(range(min = 1, max = 1000))]
///     batch_size: Option<u32>,
/// }
///
/// #[provider]
/// impl Warehouse {
///     /// Sends orders and returns how many were accepted.
///     pub async fn push(&self, orders: Vec<String>) -> Result<usize> {
///         // Call `self.api_url` with `self.api_key`.
///         Ok(orders.len().min(self.batch_size.unwrap_or(100) as usize))
///     }
/// }
///
/// # async fn example(ctx: openshop::FlowContext, orders: Vec<String>) -> Result<()> {
/// // Listed in the `providers` of `app!`, then, in a flow:
/// let accepted: usize = Warehouse::on(&ctx.connectors).push(&orders).await?;
/// # let _ = accepted;
/// # Ok(())
/// # }
/// ```
///
/// A field is saved under its name in camel case (`api_url` as `apiUrl`). Its
/// type picks the input: `String` for text, `bool` for a checkbox, a number
/// type for a number, and an `Option` makes it optional. Options go in
/// `#[field(…)]`:
///
/// - `label = "…"`. Without it the field is labeled after its name:
///   `batch_size` is "Batch size";
/// - `placeholder = "…"`, `description = "…"` for help text shown below the
///   input, `password`, `select("Label" = "value", …)`, and `name = "…"` for
///   another key;
/// - what a value must satisfy: `url`, `non_empty`, `range(min = …, max = …)`
///   for a number, `length(min = …, max = …)` for a text, a bound being
///   optional, and `validate = <validator>` for any other rule: one of
///   [`validators`], or a function of your own. The named ones run in the
///   order they are written, then `validate`. A field of an integer type
///   refuses a fraction, and an unsigned one a negative number.
///
/// The derive also implements `Deserialize` from the saved configuration: do
/// not derive it yourself.
pub use openshop_macros::ProviderConfig;

/// Makes the `impl` block of a [`ProviderConfig`](macro@ProviderConfig) struct
/// a provider: `#[provider]`.
///
/// The provider is named after the struct, with a lowercase initial:
/// `Warehouse` is `warehouse`. `#[provider("wms")]` states another name.
///
/// Every public `async fn(&self, …) -> Result<…>` becomes a method of the
/// provider. `&self` is the configuration the shop saved; the arguments and
/// the result travel as JSON, so their types implement `Serialize` and
/// `Deserialize`. Private methods and associated functions stay ordinary
/// helpers. A method marked `#[checker]`, `async fn(&self) -> Result<bool>`,
/// is the connection check of the admin.
///
/// The block gains:
///
/// - `Struct::NAME`, the name of the provider;
/// - `Struct::provider()`, the [`Provider`](builder::Provider) the struct describes; the struct
///   itself is what the `providers` of [`app!`] list;
/// - `Struct::on(&connectors)`, whose methods mirror the public ones and run
///   them with the configuration of the shop: `Warehouse::on(&ctx.connectors)
///   .push(&orders)`. An argument is taken by value or by reference;
/// - one constant per method, `Warehouse::PUSH`, a [`ProviderCall`](builder::ProviderCall) that names
///   the fake of the method in a test: `ctx.fakes.for_call(Warehouse::PUSH)`.
///
/// See [`ProviderConfig`](macro@ProviderConfig) for an example.
pub use openshop_macros::provider;

/// `shopify_api`, the Shopify client OpenShop runs on: what `graphql!`
/// generates names it here, so that an application depends on `openshop`
/// alone.
#[cfg(not(target_arch = "wasm32"))]
pub use shopify_api;

/// A Shopify Admin GraphQL operation, checked against Shopify's schema while
/// the crate compiles: `shopify_api`'s [`graphql!`](shopify_api::graphql),
/// with the code it generates pointed at [`openshop::shopify_api`](mod@shopify_api).
///
/// An unknown field, a missing argument or a variable of the wrong type is a
/// compile error. The value it returns carries the type of the operation's
/// `data`, which [`ShopifyClient::execute`] returns.
///
/// ```no_run
/// # async fn example(shopify: openshop::ShopifyClient, limit: u32) -> openshop::Result<()> {
/// let data = shopify
///     .execute(openshop::graphql!(
///         "query($first: Int!) { orders(first: $first) { nodes { id name } } }",
///         first = limit
///     ))
///     .await?;
///
/// for order in &data.orders.nodes {
///     println!("{} {}", order.id, order.name);
/// }
/// # Ok(())
/// # }
/// ```
///
/// The schema is the one of `graphql/admin.schema.json`, next to the
/// `Cargo.toml` of the crate that writes the operation, for the version of
/// `[package.metadata.shopify_api]` `api-version`; each operation is sent to
/// that version. The application
/// names the crate `openshop` in its `Cargo.toml`, not another name. See the
/// [documentation of `shopify_api`](https://docs.rs/shopify_api) for the
/// module form, input objects, bulk operations and other versions.
#[cfg(not(target_arch = "wasm32"))]
#[macro_export]
macro_rules! graphql {
    ($($input:tt)*) => {
        $crate::shopify_api::graphql! { crate = ::openshop::shopify_api, $($input)* }
    };
}

/// [`shopify::input!`](crate::shopify::input), declared here: a module of
/// the server cannot export a macro by its path.
#[cfg(not(target_arch = "wasm32"))]
#[doc(hidden)]
#[macro_export]
macro_rules! __shopify_input {
    ($($input:tt)*) => {
        $crate::shopify_api::input! { crate = ::openshop::shopify_api, $($input)* }
    };
}

/// Declares an application: the building blocks it is made of, and its settings.
///
/// ```
/// use openshop::{App, Cron, FlowContext, Result, WebhookContext, app, flow, webhook};
///
/// #[flow("syncOrders")]
/// async fn sync_orders(ctx: FlowContext) -> Result<()> {
///     ctx.logger.info("Syncing");
///     Ok(())
/// }
///
/// #[webhook("orders/create")]
/// async fn orders_create(ctx: WebhookContext) -> Result<()> {
///     // A run of the flow for the shop that sent the event.
///     sync_orders::dispatch(&ctx).await?;
///     Ok(())
/// }
///
/// pub static APP: App = app! {
///     flows: [sync_orders],
///     webhooks: [orders_create],
///     crons: [Cron::new("0 2 * * *", sync_orders::NAME).all_shops()],
/// };
///
/// # fn main() -> Result<()> {
/// let config = APP.config()?;
/// assert!(config.flows.contains_key("syncOrders"));
/// # Ok(())
/// # }
/// ```
///
/// Each list takes the blocks of one kind, by name: `providers`
/// ([`#[provider]`](macro@provider)), `flows` ([`#[flow]`](macro@flow)),
/// `functions` ([`FunctionConfig`](macro@FunctionConfig)), `models`
/// ([`Model`](macro@Model)), `admin_pages`
/// ([`#[admin_page]`](macro@admin_page)), `webhooks`
/// ([`#[webhook]`](macro@webhook)), `proxies` ([`#[proxy]`](macro@proxy)),
/// `routes` ([`#[route]`](macro@route)), `mcp_tools`
/// ([`#[mcp_tool]`](macro@mcp_tool)) and `mcp_resources`
/// ([`#[mcp_resource]`](macro@mcp_resource)). A block listed under another
/// kind, or a function without its attribute, does not compile.
///
/// Every other entry is a setting, written `name: value` and passed to the
/// method of that name of [`OpenShopBuilder`](builder::OpenShopBuilder): `crons`, `on_error`, `mcp`,
/// `shopify`, `worker`, `retry_policy`, `builtin_pages`, `admin_menu`, `admin_ui`,
/// `max_request_body_bytes`, `request_timeout`, `max_concurrent_requests`,
/// `ops_password`, `log_retention`, `sentry`… The application reads a secret
/// from the environment itself: `ops_password: std::env::var("OPS_PASSWORD").ok()`.
/// A value can fail with `?`: the error is the one `APP.config()` and
/// `APP.main()` return.
///
/// The macro is an expression of type [`App`], usually kept in a `static`
/// that the binary runs (`fn main() -> Result<()> { APP.main() }`) and the
/// tests start (`#[openshop::test(APP)]`). Nothing is evaluated before that:
/// in `dev`, the `.env` file is loaded first.
pub use openshop_macros::app;

/// Makes an `async` function a flow: `#[flow("syncOrders")]`.
///
/// ```
/// use openshop::{FlowContext, Result, WebhookContext, flow, input, json};
///
/// #[input]
/// #[derive(Debug)]
/// pub struct SyncOrdersInput {
///     pub limit: u32,
/// }
///
/// #[flow("syncOrders")]
/// pub async fn sync_orders(ctx: FlowContext, input: SyncOrdersInput) -> Result<()> {
///     let fetched: u32 = ctx.step.run("fetch", async || Ok(input.limit)).await?;
///     ctx.logger.info_with("Done", json!({ "fetched": fetched }));
///     Ok(())
/// }
///
/// assert_eq!(sync_orders::NAME, "syncOrders");
///
/// # async fn example(ctx: WebhookContext) -> Result<()> {
/// // From a webhook, a proxy, an admin action or another flow:
/// // a run for the shop of the context.
/// sync_orders::dispatch(&ctx, SyncOrdersInput { limit: 10 }).await?;
/// # Ok(())
/// # }
/// ```
///
/// The function takes the [`FlowContext`] and, optionally, the input of a
/// run: an [`#[input]`](macro@input) type, or any type that implements
/// `Deserialize` and `JsonSchema`. Its schema validates the input and
/// describes it to the admin and to MCP. Without a name, the flow is named
/// after the function in camel case.
///
/// Options follow the name: `timeout = "10m"` and `step_timeout = "30s"`,
/// durations written as text and checked when the crate compiles (`ms`, `s`,
/// `m`, `h`, `d`, as in `"1h30m"`) or any expression of type `Duration`,
/// `concurrency = allow | reject` and `retry_policy = <RetryPolicy>`.
///
/// The function stays an ordinary function. The attribute adds a type of the
/// same name, which is what the `flows` of [`app!`] list. It carries the
/// constant `NAME`, and `dispatch`, which queues a run for the shop and the
/// Shopify app of a context; dispatching needs the input to implement
/// `Serialize`, which `#[input]` derives. Because of that type, do not
/// re-export the function next to a module of the same name: import it where
/// the application is declared.
pub use openshop_macros::flow;

/// Makes an `async` function the handler of webhook topics:
/// `#[webhook("orders/create")]`.
///
/// ```
/// use openshop::{Result, WebhookContext, serde::Deserialize, webhook};
///
/// #[derive(Deserialize)]
/// # #[serde(crate = "openshop::serde")]
/// pub struct Order {
///     pub name: String,
/// }
///
/// #[webhook("orders/create", "orders/updated")]
/// pub async fn order_changed(ctx: WebhookContext, order: Order) -> Result<()> {
///     println!("{} changed on {}", order.name, ctx.shop);
///     Ok(())
/// }
/// ```
///
/// The function takes the [`WebhookContext`] and, optionally, the payload
/// read into a type that implements `Deserialize`. Shopify names the fields
/// of a payload in snake case: a plain `Deserialize` reads it, not
/// [`#[input]`](macro@input).
///
/// The context gives `ctx.db`, `ctx.shopify` and `ctx.connectors` for the
/// shop that sent the event. Nothing is read from the database before one of
/// them is used.
///
/// The function is listed in the `webhooks` of [`app!`]; its topics are in
/// the constant `TOPICS`.
pub use openshop_macros::webhook;

/// Makes an `async` function the handler of an app proxy path for one HTTP
/// method: `#[proxy(GET, "/reviews")]`.
///
/// ```
/// use openshop::serde::Deserialize;
/// use openshop::{Liquid, ProxyContext, Result, Value, json, proxy};
///
/// #[derive(Deserialize)]
/// # #[serde(crate = "openshop::serde")]
/// pub struct ListReviews {
///     pub page: Option<u32>,
/// }
///
/// #[proxy(GET, "/api/reviews")]
/// pub async fn list_reviews(ctx: ProxyContext, query: ListReviews) -> Result<Value> {
///     Ok(json!({ "shop": ctx.shop, "page": query.page.unwrap_or(1), "reviews": [] }))
/// }
///
/// #[proxy(GET, "/reviews", liquid)]
/// pub async fn reviews(_ctx: ProxyContext) -> Result<Liquid> {
///     // In an application, the template is a file next to this one:
///     // `liquid!("reviews.liquid")`.
///     let template = Liquid::new("<h2>{{ review_count }} reviews of {{ shop.name }}</h2>");
///     Ok(template.assign("review_count", 12))
/// }
/// ```
///
/// The method is `GET`, `POST`, `PUT`, `DELETE` or `PATCH`. The response type
/// follows the path: `json` (the default), `liquid` or `html`. The function
/// returns anything that serializes: a string is the body of a Liquid or HTML
/// response, everything else is sent as JSON.
///
/// A `liquid` answer is rendered by Shopify with the objects of the
/// storefront. Keep its template in a file next to the handler and return it
/// with [`liquid!`]: the [`Liquid`] it gives takes what only the server knows
/// as variables, with [`assign`](Liquid::assign).
///
/// A second argument is the typed input of the request, a type that
/// implements `Deserialize`: read from the query string for `GET` and
/// `DELETE`, from the JSON body for `POST`, `PUT` and `PATCH`. A request that
/// does not fit it is answered with `422` before the function runs.
///
/// The functions of one path are declared separately, one per method, and
/// answer with the same response type. They are listed in the `proxies` of
/// [`app!`].
pub use openshop_macros::proxy;

/// Makes an `async` function the handler of a public server route for one
/// HTTP method: `#[route(GET, "/ping")]`.
///
/// ```
/// use openshop::{Result, RouteAuthContext, RouteAuthOutcome, RouteContext, Value, json, route};
///
/// #[route(GET, "/ping", json)]
/// pub async fn ping(_ctx: RouteContext) -> Result<Value> {
///     Ok(json!({ "ok": true }))
/// }
///
/// /// What an authenticated request is known by.
/// pub struct Partner {
///     pub key: String,
/// }
///
/// async fn partner(ctx: RouteAuthContext) -> Result<RouteAuthOutcome<Partner>> {
///     Ok(match ctx.request.header("x-partner-key") {
///         Some(key) => RouteAuthOutcome::Authenticated(Partner { key: key.to_string() }),
///         None => RouteAuthOutcome::Unauthorized,
///     })
/// }
///
/// #[route(POST, "/orders/:id", json, auth = partner)]
/// pub async fn update_order(ctx: RouteContext<Partner>) -> Result<Value> {
///     Ok(json!({ "order": ctx.params["id"], "by": ctx.auth.key }))
/// }
/// ```
///
/// The method is `GET`, `HEAD`, `POST`, `PUT`, `PATCH`, `DELETE` or
/// `OPTIONS`. Without `auth`, the route is public; with `auth = <function>`,
/// the function decides before the handler runs. What it returns for an
/// authenticated request is `ctx.auth`, with its type: the handler takes a
/// `RouteContext<Partner>`, and the compiler checks that the two agree. A
/// plain [`RouteContext`] carries a `Value`.
///
/// With `json`, the handler returns anything that serializes, sent as JSON.
/// Without it, the handler returns anything that is a response
/// ([`response::IntoResponse`]): a [`response::Response`], a status with a
/// body…
///
/// A second argument is the typed input of the request, a type that
/// implements `Deserialize`: read from the query string for `GET`, `HEAD`,
/// `DELETE` and `OPTIONS`, from the JSON body for `POST`, `PUT` and `PATCH`.
/// A request that does not fit it is answered with `422` before the function
/// runs.
///
/// A route does not belong to a shop. `ctx.for_shop("demo.myshopify.com")`
/// resolves an installed one and gives its `db`, `shopify` and `connectors`.
///
/// The functions of one path are declared separately, one per method, each
/// with its own authentication. They are listed in the `routes` of [`app!`].
pub use openshop_macros::route;

/// Makes a struct the configuration of a Shopify Function: its fields are the
/// form of an instance, and `#[function(…)]` describes the function.
///
/// ```
/// use openshop::{FunctionConfig, validators};
///
/// #[derive(FunctionConfig)]
/// #[function(
///     "volumeDiscount",
///     discount = "volume-discount",
///     modes(automatic, code),
///     title = Self::title,
///     starts_at,
///     ends_at,
///     combines_with(product_discounts, shipping_discounts)
/// )]
/// pub struct VolumeDiscount {
///     #[field(label = "Minimum quantity", validate = validators::integer_min(1))]
///     pub min_quantity: u32,
///     #[field(label = "Discount percentage")]
///     pub percentage: f64,
///     #[field(label = "Excluded product tags", placeholder = "gift, sample")]
///     pub excluded_tags: Option<String>,
/// }
///
/// impl VolumeDiscount {
///     fn title(&self) -> String {
///         format!("{}% off {}+ items", self.percentage, self.min_quantity)
///     }
/// }
///
/// assert_eq!(VolumeDiscount::NAME, "volumeDiscount");
/// ```
///
/// `#[function(…)]` states the type of the function and the handle of its
/// extension, as one of `discount`, `cart_transform`, `delivery_customization`,
/// `payment_customization`, `checkout_validation` or `fulfillment_constraints`
/// `= "<handle>"`. The name the function is registered under comes first; left
/// out, it is the name of the struct with a lowercase initial. The other
/// options: `label = "…"`, `modes(automatic, code)` for a discount, and, for
/// the Shopify owner of an instance, `title = "…"` or `title = <function of
/// &Self>`, the flags `starts_at`, `ends_at`, `usage_limit`,
/// `applies_on_each_item`, `enabled` (alone, or `= false`), and
/// `combines_with(product_discounts, order_discounts, shipping_discounts)`.
///
/// The fields take the options of [`ProviderConfig`](macro@ProviderConfig).
/// The derive implements the trait [`FunctionConfig`]; the struct is listed
/// in the `functions` of [`app!`].
pub use openshop_macros::FunctionConfig;

/// Makes the `impl` block of a type a custom admin page:
/// `#[admin_page("/reviews")]`.
///
/// ```
/// use openshop::{AdminContext, Result, Value, admin_page, input, json};
///
/// #[input]
/// pub struct SaveReviewInput {
///     pub title: String,
/// }
///
/// pub struct ReviewsPage;
///
/// #[admin_page("/reviews", nav = "Reviews")]
/// impl ReviewsPage {
///     #[access]
///     async fn signed_in(ctx: AdminContext) -> Result<bool> {
///         Ok(!ctx.actor.id.is_empty())
///     }
///
///     #[loader]
///     async fn list_reviews(ctx: AdminContext) -> Result<Value> {
///         Ok(json!({ "reviews": [], "shop": ctx.shop }))
///     }
///
///     #[action(authorize = Self::signed_in)]
///     async fn save_review(ctx: AdminContext, input: SaveReviewInput) -> Result<Value> {
///         Ok(json!({ "ok": true, "title": input.title, "savedBy": ctx.actor.id }))
///     }
/// }
///
/// assert_eq!(ReviewsPage::PATH, "/reviews");
/// ```
///
/// `nav = "…"` gives the page an entry in the menu of the admin, under that
/// label. A page with a dynamic segment (`/reviews/:id`) has none.
///
/// The functions of the block are marked with what they are. The first three
/// take the [`AdminContext`] and run on the server:
///
/// - `#[access]`, `async fn(ctx) -> Result<bool>`: the gate of the page and of
///   the pages nested under it;
/// - `#[loader]`, a read, and `#[action]`, a write: `async fn(ctx) ->
///   Result<…>`, or `async fn(ctx, input: Input) -> Result<…>` where `Input`
///   is an [`#[input]`](macro@input) type, which validates what the page
///   sends. The result is anything that serializes;
/// - `#[view]`, what the page shows: `fn() -> impl IntoView`, or
///   `fn(params: RouteParams) -> impl IntoView` for a page with dynamic
///   segments. It needs the feature `ui`: see the `ui` module.
///
/// A loader or an action is called by the name of its function in camel case
/// (`listReviews`), or by the name given to the marker (`#[loader("list")]`).
/// `authorize = <function>` adds a rule of its own. Unmarked functions stay
/// ordinary helpers.
///
/// With a view, the block gains a constant per loader and action, named after
/// the function: `ReviewsPage::LIST_REVIEWS`. It carries what the function
/// takes and what it returns, for `use_loader` and `use_action`; the function
/// then states what it returns as `Result<Output>`.
///
/// The type is listed in the `admin_pages` of [`app!`], which is enough for
/// the page to be served; its path is the constant `PATH`.
pub use openshop_macros::admin_page;

/// Makes an `async` function an MCP tool: `#[mcp_tool("reviews.count")]`.
///
/// ```
/// use openshop::{McpContext, McpPermission, McpRiskLevel, Result, Value, input, json, mcp_tool};
///
/// /// Declared once: the tools that list it register it.
/// pub const READ_REVIEWS: McpPermission =
///     McpPermission::new("reviews:read", "Read reviews").risk(McpRiskLevel::Low);
///
/// #[input]
/// pub struct CountInput {
///     pub rating: u8,
/// }
///
/// /// Counts the reviews of a rating.
/// #[mcp_tool("reviews.count", permissions(READ_REVIEWS), risk = low)]
/// pub async fn count_reviews(ctx: McpContext, input: CountInput) -> Result<Value> {
///     Ok(json!({ "shop": ctx.shop, "rating": input.rating, "count": 0 }))
/// }
/// ```
///
/// The function takes the [`McpContext`] and, optionally, the arguments of
/// the call: an [`#[input]`](macro@input) type, whose schema is the input
/// schema of the tool. Without it the tool takes no argument. The result is
/// anything that serializes.
///
/// The description of the tool is its documentation comment, or
/// `description = "…"`. The other options: `permissions(…)`, the permissions
/// a token needs, `risk = low | medium | high` and `confirmation_hint = "…"`.
///
/// A permission is a [`McpPermission`] constant, or a key (`"reviews:read"`).
/// A constant comes with its label: listing it registers the permission with
/// the application, and nothing else declares it.
///
/// The function is listed in the `mcp_tools` of [`app!`]; the name of the
/// tool is the constant `NAME`.
pub use openshop_macros::mcp_tool;

/// Makes an `async` function an MCP resource:
/// `#[mcp_resource("app://guide", name = "Guide")]`.
///
/// ```
/// use openshop::{McpContext, Result, mcp_resource};
///
/// /// How reviews are moderated.
/// #[mcp_resource("app://reviews/guide", name = "Review guide", mime_type = "text/markdown")]
/// pub async fn review_guide(ctx: McpContext) -> Result<String> {
///     Ok(format!("# Reviews of {}", ctx.shop))
/// }
/// ```
///
/// The function takes the [`McpContext`] and returns the content: a
/// string, or a [`McpResourceContent`]. `name = "…"` is required. The
/// description is the documentation comment, or `description = "…"`; the
/// other options are `mime_type = "…"`, `permissions(…)` and `risk = low |
/// medium | high`.
///
/// The function is listed in the `mcp_resources` of [`app!`]; the URI of the
/// resource is the constant `URI`.
pub use openshop_macros::mcp_resource;

/// Makes a type the input of a flow, of an MCP tool or of a loader or action
/// of an admin page.
///
/// ```
/// use openshop::{FlowContext, Result, flow, input};
///
/// #[input]
/// #[derive(Debug)]
/// pub struct CountVariantsInput {
///     #[schemars(length(min = 1))]
///     pub product_id: String,
/// }
///
/// #[flow]
/// pub async fn count_variants(ctx: FlowContext, input: CountVariantsInput) -> Result<()> {
///     ctx.logger.info(format!("Counting the variants of {}", input.product_id));
///     Ok(())
/// }
///
/// let input: CountVariantsInput = openshop::serde_json::from_str(r#"{ "productId": "gid://shopify/Product/1" }"#).unwrap();
/// assert_eq!(input.product_id, "gid://shopify/Product/1");
/// ```
///
/// The attribute derives `Deserialize`, `Serialize` and `JsonSchema`, without
/// `serde` or `schemars` among the dependencies of the application: the type
/// is read from what a run receives, written where a run is dispatched
/// (`count_variants::dispatch(&ctx, input)`), and its schema validates the
/// input and describes it to the admin and to MCP.
///
/// Fields are named in camel case outside Rust (`product_id` is `productId`),
/// like the names of flows and the fields of a provider. A `#[serde(rename_all
/// = "…")]` on the type states another rule. The attributes of `serde` and of
/// `schemars` work on the type and on its fields; put `#[input]` above the
/// other attributes of the type.
///
/// The payload of a webhook is not an input: Shopify names its fields in
/// snake case, and a plain `#[derive(Deserialize)]` reads it.
pub use openshop_macros::input;

/// Makes an `async` function a test of the application:
/// `#[openshop::test(APP)]`.
///
/// ```no_run
/// use openshop::testing::TestContext;
/// use openshop::{App, RunOutcome, app, json};
///
/// static APP: App = app! {};
///
/// #[openshop::test(APP)]
/// async fn the_flow_completes(ctx: &TestContext) {
///     let outcome = ctx.run_flow("syncOrders", json!({ "limit": 5 })).await.unwrap();
///     assert_eq!(outcome, RunOutcome::Completed);
/// }
/// ```
///
/// The test receives a [`TestContext`](testing::TestContext) of the
/// application, started on an isolated database with its shop installed, and
/// the context is shut down when the function returns. The function returns
/// nothing, or a `Result` whose error fails the test.
///
/// The argument is what the test starts from: an application, or any
/// expression that resolves to a test context or to a `Result` of one, such
/// as a function of the test file. `#[openshop::test(context())]` starts the
/// context `async fn context() -> TestContext` builds, for a configuration
/// made by hand or other [`TestOptions`](testing::TestOptions).
///
/// The attributes of a test, such as `#[ignore]`, go below this one.
#[cfg(feature = "testing")]
pub use openshop_macros::test;
