//! Shopify Admin GraphQL client for an installed shop.

use std::collections::HashMap;
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

/// The access tokens a client and its clones read, per Shopify app and shop:
/// `None` when the shop has none.
type Tokens = Arc<Mutex<HashMap<(String, String), Option<String>>>>;

/// The Shopify Admin API of one shop.
///
/// Creating a client reads nothing: the access token of the installation is
/// read from the database at the first request, then kept for that Shopify
/// app and shop. A clone whose `shop` or `shopify_app` was changed reads the
/// token of its own app and shop.
///
/// The shop is checked at each request: `<name>.myshopify.com` or `<name>`,
/// nothing else, and the address the token goes to is built from it.
///
/// For the shop of `SHOPIFY_CLIENT_CREDENTIALS_SHOP`, a shop without a token
/// gets one with the client credentials grant. A token this process obtained
/// that way is renewed five minutes before it expires, and a token the Admin
/// API refuses with a `401` is renewed once and the request sent again once,
/// for any shop whose installation row holds it. The client of an Admin API
/// request also asks for the shop and app of its verified session token when
/// it has no token, and for those only: a clone pointed at another shop or
/// app does not carry that right.
#[derive(Clone)]
pub struct ShopifyClient {
    pub shop: String,
    pub shopify_app: String,
    pub api_version: String,
    app: OpenShop,
    /// What lets this client ask Shopify for a token: none in a webhook.
    grant: Option<Grant>,
    tokens: Tokens,
}

impl ShopifyClient {
    /// The client of a shop installed for a Shopify app of the application.
    pub fn new(app: &OpenShop, shop: impl Into<String>, shopify_app: impl Into<String>) -> Self {
        Self {
            shop: shop.into(),
            shopify_app: shopify_app.into(),
            api_version: SHOPIFY_API_VERSION.to_string(),
            app: app.clone(),
            grant: Some(Grant::Background),
            tokens: Arc::new(Mutex::new(HashMap::new())),
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
    /// kept for this app and shop, unless this client may ask Shopify for a
    /// token of the shop and the shop has none, the token is about to expire,
    /// or it was `rejected`.
    async fn access_token(&self, shop: &str, rejected: Option<&str>) -> Result<Option<String>> {
        let credentials = self.app.client_credentials();
        let grant = self.grant.as_ref();
        let key = (self.shopify_app.clone(), shop.to_string());
        let mut tokens = self.tokens.lock().await;
        let reusable = match tokens.get(&key) {
            None => false,
            Some(None) => !credentials.may_ask_without_token(grant, &self.shopify_app, shop),
            Some(Some(token)) => {
                rejected != Some(token.as_str())
                    && !(grant.is_some() && credentials.expiring(&self.shopify_app, shop, token))
            }
        };
        if !reusable {
            let token = credentials
                .token(&self.app, &self.shopify_app, shop, grant, rejected)
                .await?;
            tokens.insert(key.clone(), token);
        }
        Ok(tokens.get(&key).cloned().flatten())
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
            && self.grant.is_some()
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
