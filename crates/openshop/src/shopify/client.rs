//! Shopify Admin GraphQL client for an installed shop.

use std::sync::Arc;

use anyhow::{Result, bail};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::app::OpenShop;
use crate::runtime::{HttpTransport, OutboundRequest};

pub const SHOPIFY_API_VERSION: &str = "2026-04";

/// Whether `value` is written like a dated Shopify API version: `2026-04`.
pub fn is_api_version(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 7 && bytes[4] == b'-' && bytes[..4].iter().chain(&bytes[5..]).all(u8::is_ascii_digit)
}

#[derive(Clone)]
pub struct ShopifyClient {
    pub shop: String,
    pub shopify_app: String,
    pub api_version: String,
    access_token: Option<String>,
    transport: Arc<dyn HttpTransport>,
}

impl ShopifyClient {
    /// Runs a query or mutation and returns the `data` object.
    pub async fn graphql(&self, query: &str, variables: Option<Value>) -> Result<Value> {
        let Some(access_token) = &self.access_token else {
            bail!(
                "[openshop] No access token found for app \"{}\" and shop \"{}\". Is the app installed?",
                self.shopify_app,
                self.shop
            );
        };

        let shop_domain = if self.shop.contains('.') {
            self.shop.clone()
        } else {
            format!("{}.myshopify.com", self.shop)
        };

        let mut body = json!({ "query": query });
        if let Some(variables) = variables {
            body["variables"] = variables;
        }

        let response = self
            .transport
            .post_json(OutboundRequest {
                url: format!("https://{shop_domain}/admin/api/{}/graphql.json", self.api_version),
                headers: vec![
                    ("Content-Type".to_string(), "application/json".to_string()),
                    ("X-Shopify-Access-Token".to_string(), access_token.clone()),
                ],
                body,
            })
            .await?;

        if !response.ok() {
            bail!(
                "[openshop] Shopify GraphQL error ({}): {}",
                response.status,
                response.body
            );
        }

        let mut payload = response.json()?;
        if let Some(errors) = payload.get("errors").and_then(Value::as_array)
            && !errors.is_empty()
        {
            let messages: Vec<&str> = errors
                .iter()
                .filter_map(|error| error.get("message").and_then(Value::as_str))
                .collect();
            bail!("[openshop] GraphQL errors: {}", messages.join("; "));
        }

        Ok(payload.get_mut("data").map(Value::take).unwrap_or(Value::Null))
    }

    /// Runs a query and deserializes the `data` object.
    pub async fn graphql_as<T: DeserializeOwned>(&self, query: &str, variables: Option<Value>) -> Result<T> {
        Ok(serde_json::from_value(self.graphql(query, variables).await?)?)
    }
}

/// Creates a Shopify Admin API client for a specific shop.
/// Reads the access token from the installations table.
pub async fn create_shopify_client(
    app: &OpenShop,
    shop: &str,
    shopify_app: &str,
    api_version: Option<&str>,
) -> Result<ShopifyClient> {
    let installation: Option<(Option<String>,)> =
        sqlx::query_as("SELECT access_token FROM installations WHERE app_handle = $1 AND shop = $2 LIMIT 1")
            .bind(shopify_app)
            .bind(shop)
            .fetch_optional(app.db())
            .await?;

    let stored_token = installation.and_then(|(token,)| token);
    let access_token = app.crypto().decrypt_string(stored_token.as_deref())?;

    Ok(ShopifyClient {
        shop: shop.to_string(),
        shopify_app: shopify_app.to_string(),
        api_version: api_version.unwrap_or(SHOPIFY_API_VERSION).to_string(),
        access_token,
        transport: app.transport().clone(),
    })
}
