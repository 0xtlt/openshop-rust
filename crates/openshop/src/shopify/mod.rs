//! Shopify Admin API access.

pub mod client;
pub mod operation;

pub use client::{SHOPIFY_API_VERSION, ShopifyClient, create_shopify_client};
pub use operation::{GraphqlOperation, IntoVariable};
