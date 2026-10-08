//! The lower level: the builder of an application, and the definitions it takes.
//!
//! An application declares its building blocks with attributes and lists
//! them in [`app!`](crate::app!). Underneath, each attribute writes one of
//! the definitions of this module, and `app!` chains the methods of
//! [`OpenShopBuilder`]. Use them directly for what an attribute cannot say:
//! a block made up in a test, or one only known when the program runs.
//!
//! ```
//! use openshop::builder::{Field, Flow, Provider};
//! use openshop::{OpenShopConfig, json};
//!
//! # fn main() -> openshop::Result<()> {
//! let config = OpenShopConfig::builder()
//!     .provider(
//!         Provider::new("warehouse")
//!             .field("apiUrl", Field::text("API URL"))
//!             .method("push", |_config, orders| async move { Ok(json!({ "accepted": orders })) }),
//!     )
//!     .flow(Flow::new("syncOrders", |ctx| async move {
//!         ctx.connectors.call("warehouse", "push", json!([])).await?;
//!         Ok(())
//!     }))
//!     .build()?;
//! assert!(config.flows.contains_key("syncOrders"));
//! # Ok(())
//! # }
//! ```
//!
//! A declared block is added to a builder with
//! [`add`](OpenShopBuilder::add): `OpenShopConfig::builder().add::<Warehouse>()`.
//! `APP.builder()` is the builder of a declared application, to add to.

pub use crate::admin::{AdminFunction, AdminPage};
pub use crate::app::OpenShopBuilder;
pub use crate::db::Table;
#[doc(hidden)]
pub use crate::types::provider_field_value;
pub use crate::types::{
    CombinesWith, CustomPagesConfig, DiscountMode, ExperimentalConfig, Field, FieldOption, FieldType, FieldValidator,
    Fields, Flow, FlowRunFn, Function, FunctionOwner, FunctionTitle, FunctionType, InputSchema, McpResource,
    McpResourceFn, McpTool, McpToolFn, MethodResult, OnErrorFn, Provider, ProviderCall, ProviderChecker, ProviderField,
    ProviderMethod, ProviderTransformer, Proxy, ProxyHandler, ProxyResponseType, ROUTE_METHODS, RequiredPermission,
    Route, RouteAuth, RouteAuthFn, RouteAuthValue, RouteHandler, RouteMethod, Webhook, WebhookFn,
};
