//! Shopify Admin GraphQL client for an installed shop.

use std::sync::Arc;

use anyhow::{Result, bail};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use super::client_credentials::Grant;
use crate::app::OpenShop;
use crate::runtime::{OutboundRequest, OutboundResponse};
use crate::server::shop_domain::strict_shop_domain;

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
/// The shop is checked at each request: `<name>.myshopify.com` or `<name>`,
/// nothing else, and the address the token goes to is built from it.
///
/// For the shop of `SHOPIFY_CLIENT_CREDENTIALS_SHOP`, a shop without a token
/// gets one with the client credentials grant; a token this process obtained
/// that way is renewed five minutes before it expires, and a token the Admin
/// API refuses with a `401` is renewed once and the request sent again once.
/// The client of an Admin API request does so for the shop of its verified
/// session token, whatever it is.
#[derive(Clone)]
pub struct ShopifyClient {
    pub shop: String,
    pub shopify_app: String,
    pub api_version: String,
    app: OpenShop,
    /// What lets this client ask Shopify for a token: none in a webhook.
    grant: Option<Grant>,
    /// `None` until the token is read; then the token, if the shop has one.
    access_token: Arc<Mutex<Option<Option<String>>>>,
}

impl ShopifyClient {
    /// The client of a shop installed for a Shopify app of the application.
    pub fn new(app: &OpenShop, shop: impl Into<String>, shopify_app: impl Into<String>) -> Self {
        Self {
            shop: shop.into(),
            shopify_app: shopify_app.into(),
            api_version: SHOPIFY_API_VERSION.to_string(),
            app: app.clone(),
            grant: Some(Grant::Configured),
            access_token: Arc::new(Mutex::new(None)),
        }
    }

    /// Calls another version of the Admin API than [`SHOPIFY_API_VERSION`].
    pub fn with_api_version(mut self, api_version: impl Into<String>) -> Self {
        self.api_version = api_version.into();
        self
    }

    /// What lets this client ask Shopify for a token of its shop.
    pub(crate) fn granting(mut self, grant: Option<Grant>) -> Self {
        self.grant = grant;
        self
    }

    /// The access token of the installation, if there is one: read once and
    /// kept, unless this client may ask Shopify for a token of the shop and
    /// the shop has none, the token is about to expire, or it was `rejected`.
    async fn access_token(&self, shop: &str, rejected: Option<&str>) -> Result<Option<String>> {
        let credentials = self.app.client_credentials();
        let may_grant = credentials.may_grant(self.grant, shop);
        let mut cached = self.access_token.lock().await;
        let reusable = match &*cached {
            None => false,
            Some(None) => !may_grant,
            Some(Some(token)) => {
                rejected != Some(token.as_str()) && !(may_grant && credentials.expiring(&self.shopify_app, shop, token))
            }
        };
        if !reusable {
            let token = credentials
                .token(&self.app, &self.shopify_app, shop, self.grant, rejected)
                .await?;
            *cached = Some(token);
        }
        Ok(cached.clone().flatten())
    }

    async fn post(&self, shop: &str, access_token: &str, body: &Value) -> Result<OutboundResponse> {
        self.app
            .transport()
            .post_json(OutboundRequest {
                url: format!("https://{shop}/admin/api/{}/graphql.json", self.api_version),
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
        let Some(shop) = strict_shop_domain(&self.shop) else {
            bail!(
                "[openshop] Invalid shop domain for app \"{}\": expected <name>.myshopify.com",
                self.shopify_app
            );
        };
        let Some(access_token) = self.access_token(&shop, None).await? else {
            bail!(
                "[openshop] No access token found for app \"{}\" and shop \"{shop}\". Is the app installed?",
                self.shopify_app,
            );
        };

        let mut body = json!({ "query": query });
        if let Some(variables) = variables {
            body["variables"] = variables;
        }

        let mut response = self.post(&shop, &access_token, &body).await?;
        // A refused token is renewed once, where this client may ask for one;
        // when Shopify gives no other token, the `401` is the answer.
        if response.status == 401
            && self.app.client_credentials().may_grant(self.grant, &shop)
            && let Some(renewed) = self.access_token(&shop, Some(&access_token)).await?
            && renewed != access_token
        {
            response = self.post(&shop, &renewed, &body).await?;
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
