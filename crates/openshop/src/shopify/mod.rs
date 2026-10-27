//! Shopify Admin API access, through `shopify_api`.

pub mod client;
pub(crate) mod client_credentials;
pub(crate) mod token_exchange;

pub use client::ShopifyClient;
pub use shopify_api::{ApiVersion, IntoVariable, Operation, Page};
