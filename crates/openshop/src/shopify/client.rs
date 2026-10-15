//! Shopify Admin GraphQL client for an installed shop.

use std::sync::Arc;

use anyhow::{Result, bail};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use super::client_credentials::{AccessToken, ClientCredentials};
use crate::app::OpenShop;
use crate::runtime::{OutboundRequest, OutboundResponse};

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
///
/// For the shop of `SHOPIFY_CLIENT_CREDENTIALS_SHOP`, a missing or expiring
/// token is asked from Shopify with the client credentials grant, and a token
/// the Admin API refuses with a `401` is renewed once and the request sent again.
#[derive(Clone)]
pub struct ShopifyClient {
    pub shop: String,
    pub shopify_app: String,
    pub api_version: String,
    app: OpenShop,
    /// `None` until the token is read; then the token, if the shop has one.
    access_token: Arc<Mutex<Option<Option<AccessToken>>>>,
}

impl ShopifyClient {
    /// The client of a shop installed for a Shopify app of the application.
    pub fn new(app: &OpenShop, shop: impl Into<String>, shopify_app: impl Into<String>) -> Self {
        Self {
            shop: shop.into(),
            shopify_app: shopify_app.into(),
            api_version: SHOPIFY_API_VERSION.to_string(),
            app: app.clone(),
            access_token: Arc::new(Mutex::new(None)),
        }
    }

    /// Calls another version of the Admin API than [`SHOPIFY_API_VERSION`].
    pub fn with_api_version(mut self, api_version: impl Into<String>) -> Self {
        self.api_version = api_version.into();
        self
    }

    /// The client credentials mode, when it serves this shop.
    fn client_credentials(&self) -> Option<&ClientCredentials> {
        self.app.client_credentials().filter(|mode| mode.covers(&self.shop))
    }

    /// The access token of the installation, if there is one. Without the
    /// client credentials mode, it is read once and kept. With it, it is
    /// renewed when it is about to expire, or when it is `rejected`.
    async fn access_token(&self, rejected: Option<&str>) -> Result<Option<String>> {
        let mut slot = self.access_token.lock().await;
        let client_credentials = self.client_credentials();
        let reusable = match (&*slot, client_credentials) {
            (Some(_), None) => true,
            (Some(Some(token)), Some(_)) => token.fresh() && rejected != Some(token.value.as_str()),
            _ => false,
        };
        if !reusable {
            let token = match client_credentials {
                Some(mode) => Some(mode.token(&self.app, &self.shopify_app, rejected).await?),
                None => self.stored_token().await?,
            };
            *slot = Some(token);
        }
        Ok(slot.clone().flatten().map(|token| token.value))
    }

    async fn stored_token(&self) -> Result<Option<AccessToken>> {
        let installation: Option<(Option<String>,)> =
            sqlx::query_as("SELECT access_token FROM installations WHERE app_handle = $1 AND shop = $2 LIMIT 1")
                .bind(&self.shopify_app)
                .bind(&self.shop)
                .fetch_optional(self.app.db())
                .await?;
        let stored = installation.and_then(|(token,)| token);
        let value = self.app.crypto().decrypt_string(stored.as_deref())?;
        Ok(value.map(|value| AccessToken {
            value,
            expires_at: None,
        }))
    }

    async fn post(&self, shop_domain: &str, access_token: &str, body: &Value) -> Result<OutboundResponse> {
        self.app
            .transport()
            .post_json(OutboundRequest {
                url: format!("https://{shop_domain}/admin/api/{}/graphql.json", self.api_version),
                headers: vec![
                    ("Content-Type".to_string(), "application/json".to_string()),
                    ("X-Shopify-Access-Token".to_string(), access_token.to_string()),
                ],
                body: body.clone(),
            })
            .await
    }

    /// Runs a query or mutation and returns the `data` object.
    pub async fn graphql(&self, query: &str, variables: Option<Value>) -> Result<Value> {
        let Some(access_token) = self.access_token(None).await? else {
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

        let mut response = self.post(&shop_domain, &access_token, &body).await?;
        if response.status == 401
            && self.client_credentials().is_some()
            && let Some(renewed) = self.access_token(Some(&access_token)).await?
        {
            response = self.post(&shop_domain, &renewed, &body).await?;
        }

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
