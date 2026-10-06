//! Shopify Admin API access.

pub mod client;
pub mod operation;

pub use client::{SHOPIFY_API_VERSION, ShopifyClient, create_shopify_client, is_api_version};
pub use operation::{GraphqlOperation, IntoVariable};
