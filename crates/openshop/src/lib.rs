//! OpenShop: build Shopify integrations in Rust with background flows,
//! configurable service providers, and an embedded admin UI.
//!
//! An application is described once with [`OpenShopConfig::builder`] and run
//! through [`cli::run`], which provides the `dev`, `start`, `worker` and
//! `migrate` commands:
//!
//! ```no_run
//! use openshop::{Flow, OpenShopConfig};
//!
//! fn config() -> openshop::Result<OpenShopConfig> {
//!     OpenShopConfig::builder()
//!         .flow(Flow::new("hello", |ctx| async move {
//!             let greeting: String = ctx.step.run("greet", || async { Ok("hello".to_string()) }).await?;
//!             ctx.logger.info(serde_json::json!({ "greeting": greeting }), "Greeted");
//!             Ok(())
//!         }))
//!         .build()
//! }
//!
//! #[tokio::main]
//! async fn main() -> openshop::Result<()> {
//!     openshop::cli::run(config).await
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
/// `for_shop` and `all_shops`, without one it gets `rows`. Register the model
/// with `.model_of::<Review>()` on the configuration builder.
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
/// // Registered with `.provider(Warehouse::provider())`, then, in a flow:
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
/// - `Struct::provider()`, the [`Provider`] to register with `.provider(…)`;
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
