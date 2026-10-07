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
//! use openshop::{App, FlowContext, Result, app, flow, json};
//!
//! #[flow]
//! async fn hello(ctx: FlowContext) -> Result<()> {
//!     let greeting: String = ctx.step.run("greet", || async { Ok("hello".to_string()) }).await?;
//!     ctx.logger.info(json!({ "greeting": greeting }), "Greeted");
//!     Ok(())
//! }
//!
//! static APP: App = app! {
//!     flows: [hello],
//! };
//!
//! #[tokio::main]
//! async fn main() -> Result<()> {
//!     APP.run().await
//! }
//! ```
//!
//! A web process (`start`) accepts requests and queues work; workers (`worker`)
//! execute it. PostgreSQL stores installations, credentials, runs and checkpoints.

pub mod admin;
mod app;
pub mod cli;
pub mod config;
pub mod db;
pub mod declare;
pub mod engine;
pub mod response;
pub mod runtime;
pub mod server;
pub mod shopify;
#[cfg(feature = "testing")]
pub mod testing;
pub mod types;
pub mod validators;

pub use anyhow::{Context, Error, Result, anyhow, bail};
pub use chrono;
pub use indexmap::IndexMap;
#[cfg(feature = "schema")]
pub use schemars::{self, JsonSchema};
pub use serde;
pub use serde_json::{self, Value, json};
pub use sqlx::{self, PgPool};
pub use tokio_util::sync::CancellationToken;
pub use uuid::Uuid;

pub use admin::{
    AdminActor, AdminFunction, AdminPage, AdminPublicError, AdminServerContext, define_admin_action,
    define_admin_loader, define_admin_page_access,
};
#[cfg(feature = "schema")]
pub use admin::{define_typed_admin_action, define_typed_admin_loader};
pub use app::{OpenShop, OpenShopBuilder, OpenShopConfig};
pub use db::{Model, Record, define_model};
pub use declare::{App, Declared};
pub use engine::{
    DispatchFlowParams, DispatchFlowResult, FlowCanceledError, FlowConcurrencyError, FlowTimeoutError, RunFlowOptions,
    RunFlowResult, Scheduler, SleepSignal, StepExecutor, StepTimeoutError, Worker, cancel_run, dispatch_flow, run_flow,
};
pub use runtime::{FlowLogger, RuntimeEnv, RuntimeLogger, get_runtime_logger, set_runtime_logger};
pub use server::connectors::{Connector, Connectors};
pub use server::static_ui::UiAssets;
pub use server::{ServerOptions, ShopSession, UiSource, create_server, start_api_server};
pub use shopify::{GraphqlOperation, IntoVariable, ShopifyClient, create_shopify_client};
pub use types::*;

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
/// #[model(table = "reviews", index(shop, product_id))]
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
/// # async fn example(db: openshop::PgPool, shop: &str) -> openshop::Result<()> {
/// let reviews = Review::for_shop(&db, shop);
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
/// get the conventional definitions; with a `shop` field the struct gets
/// `for_shop` and `all_shops`, without one it gets `rows`. List the model in
/// the `models` of [`app!`].
///
/// Struct options, in `#[model(…)]`: `table = "…"` (required),
/// `index(field, …)`, `unique(field, …)`, each with an optional `name = "…"`,
/// and `renamed_from = "…"`. Field options: `default = "<SQL>"`,
/// `sql_type = "…"`, `column = "…"`, `renamed_from = "…"`, `index`, `unique`,
/// and `references(OtherModel)` with an optional `on_delete = "cascade"` for a
/// foreign key.
///
/// The derive implements [`Record`] and `sqlx::FromRow`, and declares
/// `New<Struct>`, the values an insertion takes.
pub use openshop_macros::Model;

/// Makes a struct the configuration of a provider: its fields are the fields
/// of the form a merchant fills, and the struct is what the methods of the
/// provider receive.
///
/// ```
/// use openshop::{ProviderConfig, Result, provider, validators};
///
/// #[derive(ProviderConfig)]
/// pub struct Warehouse {
///     #[field(label = "API URL", validate = validators::url())]
///     api_url: String,
///     #[field(label = "API key", password)]
///     api_key: String,
///     #[field(label = "Batch size")]
///     batch_size: Option<u32>,
/// }
///
/// #[provider("warehouse")]
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
/// `#[field(…)]`: `label = "…"` (required), `placeholder = "…"`, `password`,
/// `select("Label" = "value", …)`, `validate = <validator>` and `name = "…"`
/// for another key.
///
/// The derive also implements `Deserialize` from the saved configuration: do
/// not derive it yourself.
pub use openshop_macros::ProviderConfig;

/// Makes the `impl` block of a [`ProviderConfig`](macro@ProviderConfig) struct
/// a provider: `#[provider("warehouse")]`.
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
/// - `Struct::provider()`, the [`Provider`] the struct describes; the struct
///   itself is what the `providers` of [`app!`] list;
/// - `Struct::on(&connectors)`, whose methods mirror the public ones and run
///   them with the configuration of the shop: `Warehouse::on(&ctx.connectors)
///   .push(&orders)`. An argument is taken by value or by reference;
/// - one constant per method, `Warehouse::PUSH`, a [`ProviderCall`] for
///   [`Connectors::invoke`] and for the fakes of a test.
///
/// See [`ProviderConfig`](macro@ProviderConfig) for an example.
pub use openshop_macros::provider;

/// Builds a typed Shopify Admin GraphQL operation, checked at compile time.
///
/// The operation is checked against Shopify's schema while the crate
/// compiles: an unknown field, a missing argument or a variable of the wrong
/// type is a compile error. The value it returns carries the type of the
/// operation's `data`, which [`ShopifyClient::execute`] returns.
///
/// ```
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
/// Variables follow the operation as `name = value`. A value is converted to
/// the Rust type of the variable ([`IntoVariable`]); a variable holding an
/// input object accepts any serializable value.
///
/// To name the generated types, for an operation used in several places or
/// with input objects, give the operation a module:
///
/// ```
/// openshop::graphql! {
///     pub mod product_title = "query($id: ID!) { product(id: $id) { title } }";
/// }
///
/// # async fn example(shopify: openshop::ShopifyClient) -> openshop::Result<Option<String>> {
/// let data: product_title::Data = shopify
///     .execute(product_title::operation(product_title::Variables {
///         id: "gid://shopify/Product/1".to_string(),
///     }))
///     .await?;
/// Ok(data.product.map(|product| product.title))
/// # }
/// ```
///
/// The schema is the one of [`shopify::SHOPIFY_API_VERSION`], bundled with the
/// framework. `openshop codegen --api-version=<version>` stores another one in
/// `graphql/admin.schema.json`, which takes precedence.
pub use openshop_macros::graphql;

/// Declares an application: the building blocks it is made of, and its settings.
///
/// ```
/// use openshop::{App, CronEntry, FlowContext, Result, WebhookContext, app, flow, json, webhook};
///
/// #[flow("syncOrders")]
/// async fn sync_orders(ctx: FlowContext) -> Result<()> {
///     ctx.logger.info(json!({}), "Syncing");
///     Ok(())
/// }
///
/// #[webhook("orders/create")]
/// async fn orders_create(ctx: WebhookContext) -> Result<()> {
///     println!("New order on {}", ctx.shop);
///     Ok(())
/// }
///
/// pub static APP: App = app! {
///     flows: [sync_orders],
///     webhooks: [orders_create],
///     crons: [CronEntry::new("0 2 * * *", sync_orders::NAME).shops("all")],
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
/// method of that name of [`OpenShopBuilder`]: `crons`, `on_error`, `mcp`,
/// `custom_pages`, `shopify`, `worker`, `retry_policy`, `pages`, `admin_ui`,
/// `max_request_body_bytes`… A value can fail with `?`, such as one read from
/// the environment (`shopify: shopify_apps()?`): the error is the one
/// `APP.config()` and `APP.run()` return.
///
/// The macro is an expression of type [`App`], usually kept in a `static`
/// that the binary runs (`APP.run().await`) and the tests read
/// (`APP.config()`). Nothing is evaluated before that: in `dev`, the `.env`
/// file is loaded first.
pub use openshop_macros::app;

/// Makes an `async` function a flow: `#[flow("syncOrders")]`.
///
/// ```
/// use openshop::{FlowContext, JsonSchema, Result, flow, serde::Deserialize};
///
/// #[derive(Debug, Deserialize, JsonSchema)]
/// # #[serde(crate = "openshop::serde")]
/// pub struct SyncOrdersInput {
///     pub limit: u32,
/// }
///
/// #[flow("syncOrders")]
/// pub async fn sync_orders(ctx: FlowContext, input: SyncOrdersInput) -> Result<()> {
///     let fetched: u32 = ctx.step.run("fetch", || async { Ok(input.limit) }).await?;
///     ctx.logger.info(openshop::json!({ "fetched": fetched }), "Done");
///     Ok(())
/// }
///
/// assert_eq!(sync_orders::NAME, "syncOrders");
/// ```
///
/// The function takes the [`FlowContext`] and, optionally, the input of a
/// run: a type that implements `Deserialize` and `JsonSchema`, whose schema
/// validates the input and describes it to the admin and to MCP. Without a
/// name, the flow is named after the function in camel case.
///
/// Options follow the name: `timeout = <Duration>`, `step_timeout =
/// <Duration>`, `concurrency = <Concurrency>` and `retry_policy =
/// <PartialRetryPolicy>`.
///
/// The function stays an ordinary function. The attribute adds a type of the
/// same name, which is what the `flows` of [`app!`] list, with the constant
/// `NAME`. Because of that type, do not re-export the function next to a
/// module of the same name: import it where the application is declared.
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
/// read into a type that implements `Deserialize`. It is listed in the
/// `webhooks` of [`app!`]; its topics are in the constant `TOPICS`.
pub use openshop_macros::webhook;

/// Makes an `async` function the handler of an app proxy path for one HTTP
/// method: `#[proxy(GET, "/reviews")]`.
///
/// ```
/// use openshop::{ProxyContext, Result, Value, json, proxy};
///
/// #[proxy(GET, "/api/reviews")]
/// pub async fn list_reviews(ctx: ProxyContext) -> Result<Value> {
///     Ok(json!({ "shop": ctx.shop, "reviews": [] }))
/// }
///
/// #[proxy(GET, "/reviews", liquid)]
/// pub async fn reviews(ctx: ProxyContext) -> Result<String> {
///     Ok(format!("<h2>Reviews of {{{{ shop.name }}}} via {}</h2>", ctx.shop))
/// }
/// ```
///
/// The method is `GET`, `POST`, `PUT`, `DELETE` or `PATCH`. The response type
/// follows the path: `json` (the default), `liquid` or `html`. The function
/// returns anything that serializes: a string is the body of a Liquid or HTML
/// response, everything else is sent as JSON.
///
/// The functions of one path are declared separately, one per method, and
/// answer with the same response type. They are listed in the `proxies` of
/// [`app!`].
pub use openshop_macros::proxy;

/// Makes an `async` function the handler of a public server route for one
/// HTTP method: `#[route(GET, "/ping")]`.
///
/// ```
/// use openshop::response::{self, Response};
/// use openshop::{Result, RouteAuthOutcome, ServerRouteContext, ServerRouteRequestContext, json, route};
///
/// #[route(GET, "/ping")]
/// pub async fn ping(_ctx: ServerRouteContext) -> Result<Response> {
///     Ok(response::json(json!({ "ok": true })))
/// }
///
/// async fn partner(ctx: ServerRouteRequestContext) -> Result<RouteAuthOutcome> {
///     Ok(match ctx.request.header("x-partner-key") {
///         Some(key) => RouteAuthOutcome::Authenticated(json!({ "key": key })),
///         None => RouteAuthOutcome::Unauthorized,
///     })
/// }
///
/// #[route(POST, "/orders/:id", auth = partner)]
/// pub async fn update_order(ctx: ServerRouteContext) -> Result<Response> {
///     Ok(response::json(json!({ "order": ctx.params["id"], "by": ctx.auth["key"] })))
/// }
/// ```
///
/// The method is `GET`, `HEAD`, `POST`, `PUT`, `PATCH`, `DELETE` or
/// `OPTIONS`. Without `auth`, the route is public; with `auth = <function>`,
/// the function decides before the handler runs and its value is `ctx.auth`.
/// The handler returns anything that is a response
/// ([`response::IntoResponse`]).
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
/// assert_eq!(VolumeDiscount::KEY, "volumeDiscount");
/// ```
///
/// `#[function(…)]` states the type of the function and the handle of its
/// extension, as one of `discount`, `cart_transform`, `delivery_customization`,
/// `payment_customization`, `checkout_validation` or `fulfillment_constraints`
/// `= "<handle>"`. The key the function is registered under comes first; left
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

/// Makes the `impl` block of a type the server side of a custom admin page:
/// `#[admin_page("/reviews")]`.
///
/// ```
/// use openshop::{AdminServerContext, JsonSchema, Result, Value, admin_page, json, serde::Deserialize};
///
/// #[derive(Deserialize, JsonSchema)]
/// # #[serde(crate = "openshop::serde")]
/// pub struct SaveReviewInput {
///     pub title: String,
/// }
///
/// pub struct ReviewsPage;
///
/// #[admin_page("/reviews")]
/// impl ReviewsPage {
///     #[access]
///     async fn signed_in(ctx: AdminServerContext) -> Result<bool> {
///         Ok(!ctx.actor.id.is_empty())
///     }
///
///     #[loader]
///     async fn list_reviews(ctx: AdminServerContext) -> Result<Value> {
///         Ok(json!({ "reviews": [], "shop": ctx.shop }))
///     }
///
///     #[action(authorize = Self::signed_in)]
///     async fn save_review(ctx: AdminServerContext, input: SaveReviewInput) -> Result<Value> {
///         Ok(json!({ "ok": true, "title": input.title, "savedBy": ctx.actor.id }))
///     }
/// }
///
/// assert_eq!(ReviewsPage::PATH, "/reviews");
/// ```
///
/// The functions of the block take the [`AdminServerContext`] and are marked
/// with what they are:
///
/// - `#[access]`, `async fn(ctx) -> Result<bool>`: the gate of the page and of
///   the pages nested under it;
/// - `#[loader]`, a read, and `#[action]`, a write: `async fn(ctx) ->
///   Result<…>`, or `async fn(ctx, input: Input) -> Result<…>` where `Input`
///   implements `Deserialize` and `JsonSchema` and validates what the page
///   sends. The result is anything that serializes.
///
/// A loader or an action is called by the name of its function in camel case
/// (`listReviews`), or by the name given to the marker (`#[loader("list")]`).
/// `authorize = <function>` adds a rule of its own. Unmarked functions stay
/// ordinary helpers.
///
/// The type is listed in the `admin_pages` of [`app!`]; its path is the
/// constant `PATH`.
pub use openshop_macros::admin_page;

/// Makes an `async` function an MCP tool: `#[mcp_tool("reviews.count")]`.
///
/// ```
/// use openshop::{JsonSchema, McpExecutionContext, Result, Value, json, mcp_tool, serde::Deserialize};
///
/// #[derive(Deserialize, JsonSchema)]
/// # #[serde(crate = "openshop::serde")]
/// pub struct CountInput {
///     pub rating: u8,
/// }
///
/// /// Counts the reviews of a rating.
/// #[mcp_tool("reviews.count", permissions("reviews:read"), risk = low)]
/// pub async fn count_reviews(ctx: McpExecutionContext, input: CountInput) -> Result<Value> {
///     Ok(json!({ "shop": ctx.shop, "rating": input.rating, "count": 0 }))
/// }
/// ```
///
/// The function takes the [`McpExecutionContext`] and, optionally, the
/// arguments of the call: a type that implements `Deserialize` and
/// `JsonSchema`, whose schema is the input schema of the tool. Without it the
/// tool takes no argument. The result is anything that serializes.
///
/// The description of the tool is its documentation comment, or
/// `description = "…"`. The other options: `permissions(…)`, the permissions
/// a token needs, `risk = low | medium | high` and `confirmation_hint = "…"`.
///
/// The function is listed in the `mcp_tools` of [`app!`]; the name of the
/// tool is the constant `NAME`.
pub use openshop_macros::mcp_tool;

/// Makes an `async` function an MCP resource:
/// `#[mcp_resource("app://guide", name = "Guide")]`.
///
/// ```
/// use openshop::{McpExecutionContext, Result, mcp_resource};
///
/// /// How reviews are moderated.
/// #[mcp_resource("app://reviews/guide", name = "Review guide", mime_type = "text/markdown")]
/// pub async fn review_guide(ctx: McpExecutionContext) -> Result<String> {
///     Ok(format!("# Reviews of {}", ctx.shop))
/// }
/// ```
///
/// The function takes the [`McpExecutionContext`] and returns the content: a
/// string, or a [`McpResourceContent`]. `name = "…"` is required. The
/// description is the documentation comment, or `description = "…"`; the
/// other options are `mime_type = "…"`, `permissions(…)` and `risk = low |
/// medium | high`.
///
/// The function is listed in the `mcp_resources` of [`app!`]; the URI of the
/// resource is the constant `URI`.
pub use openshop_macros::mcp_resource;
