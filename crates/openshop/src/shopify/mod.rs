//! Shopify Admin API access, through `shopify_api`, and the Partner API.

pub mod client;
pub(crate) mod client_credentials;
pub mod endpoints;
pub mod partner;
pub(crate) mod token_exchange;

pub use client::ShopifyClient;
pub use endpoints::ShopifyEndpoints;
pub use partner::{PartnerApi, PartnerApiClient};
/// How the Shopify client retries what Shopify did not run: the
/// [`shopify_retry`](crate::builder::OpenShopBuilder::shopify_retry) setting.
/// Not the [`RetryPolicy`](crate::RetryPolicy) of a flow run.
pub use shopify_api::RetryPolicy;
pub use shopify_api::{ApiVersion, IntoVariable, Operation, Page};

/// A Shopify app and a shop, what the process keeps its tokens and clients by.
type AppShop = (String, String);

fn app_shop(shopify_app: &str, shop: &str) -> AppShop {
    (shopify_app.to_string(), shop.to_string())
}

/// An input object of the Admin API built ahead of an operation, in a loop
/// for instance: `shopify_api`'s [`input!`](shopify_api::input), with the
/// code it generates pointed at [`openshop::shopify_api`](mod@crate::shopify_api).
///
/// ```ignore
/// use openshop::shopify::input;
///
/// let metafields: Vec<_> = rows
///     .iter()
///     .map(|row| input!(MetafieldsSetInput {
///         ownerId: &row.product_id, namespace: "custom", key: "color", value: &row.color,
///         type: "single_line_text_field",
///     }))
///     .collect();
/// ```
pub use crate::__shopify_input as input;
