//! Shopify Admin API access.

pub mod client;
pub(crate) mod client_credentials;
pub mod operation;
pub(crate) mod token_exchange;

pub use client::{SHOPIFY_API_VERSION, ShopifyClient, is_api_version};
pub use operation::{GraphqlOperation, IntoVariable};
