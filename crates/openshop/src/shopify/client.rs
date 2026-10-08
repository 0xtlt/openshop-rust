//! Shopify Admin GraphQL client for an installed shop.

use std::sync::Arc;

use anyhow::{Result, bail};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::OnceCell;

use crate::app::OpenShop;
use crate::runtime::OutboundRequest;

pub const SHOPIFY_API_VERSION: &str = "2026-04";

/// Whether `value` is written like a dated Shopify API version: `2026-04`.
pub fn is_api_version(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 7 && bytes[4] == b'-' && bytes[..4].iter().chain(&bytes[5..]).all(u8::is_ascii_digit)
}

/// The Shopify Admin API of one shop.
///
/// Creating a client reads nothing: the access token of the installation is
/// read from the database at the first request, then kept.
#[derive(Clone)]
pub struct ShopifyClient {
    pub shop: String,
    pub shopify_app: String,
    pub api_version: String,
    app: OpenShop,
    access_token: Arc<OnceCell<Option<String>>>,
}

impl ShopifyClient {
    /// The client of a shop installed for a Shopify app of the application.
    pub fn new(app: &OpenShop, shop: impl Into<String>, shopify_app: impl Into<String>) -> Self {
        Self {
            shop: shop.into(),
            shopify_app: shopify_app.into(),
            api_version: SHOPIFY_API_VERSION.to_string(),
            app: app.clone(),
            access_token: Arc::new(OnceCell::new()),
        }
    }

    /// Calls another version of the Admin API than [`SHOPIFY_API_VERSION`].
    pub fn with_api_version(mut self, api_version: impl Into<String>) -> Self {
        self.api_version = api_version.into();
        self
    }

    /// The access token stored for the installation, if there is one.
    async fn access_token(&self) -> Result<Option<&str>> {
        let token = self
            .access_token
            .get_or_try_init(|| async {
                let installation: Option<(Option<String>,)> = sqlx::query_as(
                    "SELECT access_token FROM installations WHERE app_handle = $1 AND shop = $2 LIMIT 1",
                )
                .bind(&self.shopify_app)
                .bind(&self.shop)
                .fetch_optional(self.app.db())
                .await?;
                let stored = installation.and_then(|(token,)| token);
                self.app.crypto().decrypt_string(stored.as_deref())
            })
            .await?;
        Ok(token.as_deref())
    }

    /// Runs a query or mutation and returns the `data` object.
    pub async fn graphql(&self, query: &str, variables: Option<Value>) -> Result<Value> {
        let Some(access_token) = self.access_token().await? else {
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
            .app
            .transport()
            .post_json(OutboundRequest {
                url: format!("https://{shop_domain}/admin/api/{}/graphql.json", self.api_version),
                headers: vec![
                    ("Content-Type".to_string(), "application/json".to_string()),
                    ("X-Shopify-Access-Token".to_string(), access_token.to_string()),
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
