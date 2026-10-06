//! Shopify Admin API access.

pub mod client;

pub use client::{SHOPIFY_API_VERSION, ShopifyClient, create_shopify_client};
