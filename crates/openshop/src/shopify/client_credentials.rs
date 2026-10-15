//! Access tokens obtained with Shopify's client credentials grant, for the one
//! shop named by `SHOPIFY_CLIENT_CREDENTIALS_SHOP`.
//!
//! Shopify gives such a token with no browser and no merchant, but only when
//! the app and the store belong to the same Shopify organization, as the
//! development stores of the organization that owns the app do. It lasts about
//! 24 hours and comes without a refresh token: it is asked again a few minutes
//! before it expires, and once when the Admin API refuses it.

use anyhow::{Context, Result, bail};
use chrono::{DateTime, TimeDelta, Utc};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::app::OpenShop;
use crate::runtime::{FORM_CONTENT_TYPE, OutboundRequest, logger};
use crate::server::shop_domain::normalize_shop_domain;

/// The environment variable that turns the mode on.
pub const CLIENT_CREDENTIALS_SHOP_VAR: &str = "SHOPIFY_CLIENT_CREDENTIALS_SHOP";

/// A token that expires is renewed this long before it does.
const RENEWAL_MARGIN: TimeDelta = TimeDelta::minutes(5);

/// The access token of an installation, with when it expires, if it does:
/// the tokens of OAuth and of the token exchange do not.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct AccessToken {
    pub value: String,
    pub expires_at: Option<DateTime<Utc>>,
}

impl std::fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccessToken")
            .field("value", &"[redacted]")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

impl AccessToken {
    /// Whether it can still be sent: it does not expire, or not within the margin.
    pub fn fresh(&self) -> bool {
        self.expires_at
            .is_none_or(|expires_at| expires_at - RENEWAL_MARGIN > Utc::now())
    }
}

/// The token stored for an installation, if there is one.
async fn stored_token(app: &OpenShop, shopify_app: &str, shop: &str) -> Result<Option<AccessToken>> {
    let row: Option<(Option<String>, Option<DateTime<Utc>>)> = sqlx::query_as(
        "SELECT access_token, access_token_expires_at FROM installations \
         WHERE app_handle = $1 AND shop = $2 LIMIT 1",
    )
    .bind(shopify_app)
    .bind(shop)
    .fetch_optional(app.db())
    .await?;
    let Some((stored, expires_at)) = row else {
        return Ok(None);
    };
    let value = app.crypto().decrypt_string(stored.as_deref())?;
    Ok(value.map(|value| AccessToken { value, expires_at }))
}

/// The client credentials mode: the shop it serves, and the lock that lets one
/// request at a time ask Shopify for its token.
pub(crate) struct ClientCredentials {
    shop: String,
    renewing: Mutex<()>,
}

impl ClientCredentials {
    /// The mode set by `SHOPIFY_CLIENT_CREDENTIALS_SHOP`, off without it.
    pub fn from_env(value: Option<&str>) -> Result<Option<Self>> {
        let Some(value) = value else {
            return Ok(None);
        };
        let Some(shop) = normalize_shop_domain(value) else {
            bail!("[openshop] {CLIENT_CREDENTIALS_SHOP_VAR} must be a *.myshopify.com domain");
        };
        Ok(Some(Self {
            shop,
            renewing: Mutex::new(()),
        }))
    }

    /// Whether the mode serves `shop`, given as a domain or, as a
    /// [`ShopifyClient`](crate::ShopifyClient) also takes it, by its name alone.
    pub fn covers(&self, shop: &str) -> bool {
        let domain = if shop.contains('.') {
            normalize_shop_domain(shop)
        } else {
            normalize_shop_domain(&format!("{shop}.myshopify.com"))
        };
        domain.as_deref() == Some(self.shop.as_str())
    }

    /// A token of the shop for the Shopify app `shopify_app`: the stored one
    /// while it is fresh and is not `rejected`, the token the Admin API just
    /// refused; otherwise a new one, asked with the client credentials grant
    /// and stored encrypted.
    pub async fn token(&self, app: &OpenShop, shopify_app: &str, rejected: Option<&str>) -> Result<AccessToken> {
        let usable = |token: &AccessToken| token.fresh() && rejected != Some(token.value.as_str());
        if let Some(token) = stored_token(app, shopify_app, &self.shop).await?.filter(usable) {
            return Ok(token);
        }

        let _renewing = self.renewing.lock().await;
        // Another request may have renewed it while this one waited.
        if let Some(token) = stored_token(app, shopify_app, &self.shop).await?.filter(usable) {
            return Ok(token);
        }
        let (token, scopes) = self.request(app, shopify_app).await?;

        sqlx::query(
            "INSERT INTO installations (app_handle, shop, access_token, access_token_expires_at, scopes) \
             VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (app_handle, shop) DO UPDATE SET \
               access_token = EXCLUDED.access_token, access_token_expires_at = EXCLUDED.access_token_expires_at, \
               scopes = EXCLUDED.scopes, uninstalled_at = NULL",
        )
        .bind(shopify_app)
        .bind(&self.shop)
        .bind(app.crypto().encrypt_string(&token.value)?)
        .bind(token.expires_at)
        .bind(scopes)
        .execute(app.db())
        .await?;

        logger::info(format!(
            "[openshop] Client credentials token obtained for {} (app \"{shopify_app}\")",
            self.shop
        ));
        Ok(token)
    }

    /// Asks Shopify for a token. An error names the HTTP status and the error
    /// code Shopify gives, never the request or the rest of the answer.
    async fn request(&self, app: &OpenShop, shopify_app: &str) -> Result<(AccessToken, Option<String>)> {
        let shop = &self.shop;
        let resolved = app.apps().by_handle(Some(shopify_app))?;
        if resolved.api_key.is_empty() || resolved.api_secret.is_empty() {
            bail!("[openshop] Client credentials for {shop} need the API key and secret of app \"{shopify_app}\"");
        }

        let response = app
            .transport()
            .post_json(OutboundRequest {
                url: format!("https://{shop}/admin/oauth/access_token"),
                headers: vec![("Content-Type".to_string(), FORM_CONTENT_TYPE.to_string())],
                body: json!({
                    "grant_type": "client_credentials",
                    "client_id": resolved.api_key,
                    "client_secret": resolved.api_secret,
                }),
            })
            .await
            .with_context(|| format!("[openshop] Client credentials request for {shop} failed"))?;

        let data: Option<Value> = response.json().ok();
        if !response.ok() {
            let code = data.as_ref().and_then(error_code);
            let code = code.map(|code| format!(": {code}")).unwrap_or_default();
            bail!(
                "[openshop] Shopify refused the client credentials of app \"{shopify_app}\" for {shop} ({}{code})",
                response.status
            );
        }

        let Some(value) = data
            .as_ref()
            .and_then(|data| data.get("access_token"))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        else {
            bail!("[openshop] Shopify answered the client credentials for {shop} without an access token");
        };
        let expires_at = data
            .as_ref()
            .and_then(|data| data.get("expires_in"))
            .and_then(Value::as_i64)
            .and_then(TimeDelta::try_seconds)
            .map(|expires_in| Utc::now() + expires_in);
        let scopes = data
            .as_ref()
            .and_then(|data| data.get("scope"))
            .and_then(Value::as_str)
            .map(str::to_string);
        Ok((
            AccessToken {
                value: value.to_string(),
                expires_at,
            },
            scopes,
        ))
    }
}

/// The error code of a refusal, such as `shop_not_permitted`: a short word
/// of letters, digits and underscores, or nothing. A sentence is left out.
fn error_code(data: &Value) -> Option<&str> {
    ["error", "errors"]
        .into_iter()
        .filter_map(|key| data.get(key).and_then(Value::as_str))
        .find(|code| {
            !code.is_empty()
                && code.len() <= 64
                && code
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_shop_of_the_mode() {
        assert!(ClientCredentials::from_env(None).unwrap().is_none());
        let mode = ClientCredentials::from_env(Some(" Dev-Store-1.MyShopify.com ")).unwrap().unwrap();
        assert!(mode.covers("dev-store-1.myshopify.com"));
        assert!(mode.covers("dev-store-1"));
        assert!(!mode.covers("dev-store-2.myshopify.com"));
        assert!(!mode.covers("dev-store-1.myshopify.com.evil.com"));

        for value in ["example.com", "dev-store.myshopify.com.evil.com", "not a shop"] {
            let error = ClientCredentials::from_env(Some(value)).err().unwrap().to_string();
            assert_eq!(
                error,
                "[openshop] SHOPIFY_CLIENT_CREDENTIALS_SHOP must be a *.myshopify.com domain"
            );
        }
    }

    #[test]
    fn a_token_is_fresh_until_five_minutes_before_it_expires() {
        let token = |expires_at| AccessToken {
            value: "token".to_string(),
            expires_at,
        };
        assert!(token(None).fresh());
        assert!(token(Some(Utc::now() + TimeDelta::minutes(6))).fresh());
        assert!(!token(Some(Utc::now() + TimeDelta::minutes(4))).fresh());
        assert!(!token(Some(Utc::now() - TimeDelta::minutes(1))).fresh());
        assert!(!format!("{:?}", token(None)).contains("\"token\""));
    }

    #[test]
    fn keeps_only_the_code_of_a_refusal() {
        assert_eq!(
            error_code(&json!({ "error": "shop_not_permitted", "error_description": "secret details" })),
            Some("shop_not_permitted")
        );
        assert_eq!(error_code(&json!({ "errors": "invalid_client" })), Some("invalid_client"));
        assert_eq!(error_code(&json!({ "error": "a sentence with spaces" })), None);
        assert_eq!(error_code(&json!({ "errors": { "field": "x" } })), None);
        assert_eq!(error_code(&json!({})), None);
    }
}
