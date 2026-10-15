//! Access tokens obtained with Shopify's client credentials grant.
//!
//! Shopify gives such a token with no browser and no merchant, but only when
//! the app and the store belong to the same Shopify organization, as the
//! development stores of the organization that owns the app do. It lasts about
//! 24 hours and comes without a refresh token.
//!
//! A token is only asked for a shop Shopify vouched for, or for the one shop
//! the operator named:
//!
//! - [`Grant::Vouched`]: a launch from Shopify admin whose signed query
//!   covers `shop`, or a verified session token (the shop of its `dest`).
//!   Any shop without a usable token; a success reinstalls a shop marked
//!   uninstalled.
//! - [`Grant::Configured`]: anything else that may ask (flows, crons, routes,
//!   the MCP tool), only for the shop of `SHOPIFY_CLIENT_CREDENTIALS_SHOP`,
//!   and never for a shop marked uninstalled.
//!
//! A refusal is remembered in the process for an hour per app and shop, and
//! the caller falls back to what it did without the grant. Nothing of it is
//! stored in the database but the token, encrypted, in `installations`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex, MutexGuard};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use chrono::{DateTime, TimeDelta, Utc};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::app::OpenShop;
use crate::runtime::{FORM_CONTENT_TYPE, OutboundRequest, logger};
use crate::server::shop_domain::strict_shop_domain;

/// The environment variable that names the shop of background work.
pub const CLIENT_CREDENTIALS_SHOP_VAR: &str = "SHOPIFY_CLIENT_CREDENTIALS_SHOP";

/// A token this process obtained is renewed this long before it expires.
const RENEWAL_MARGIN: TimeDelta = TimeDelta::minutes(5);

/// How long a refusal holds back another request for the same app and shop.
const REFUSAL_MEMORY: Duration = Duration::from_secs(60 * 60);

/// What lets a caller ask Shopify for a token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Grant {
    /// Shopify vouched for the shop: a signed launch, or a verified session token.
    Vouched,
    /// No request vouches for the shop: only the shop of
    /// `SHOPIFY_CLIENT_CREDENTIALS_SHOP` is served.
    Configured,
}

/// What the process knows of one app and shop.
#[derive(Default)]
struct Entry {
    /// Held while Shopify is asked, so that one request at a time does.
    asking: Arc<Mutex<()>>,
    /// When Shopify last refused.
    refused_at: Option<Instant>,
    /// The token this process obtained, as a digest, and when it expires.
    granted: Option<([u8; 32], Option<DateTime<Utc>>)>,
}

/// The client credentials grant of the application, and what this process
/// remembers of it.
pub(crate) struct ClientCredentials {
    configured: Option<String>,
    entries: StdMutex<HashMap<(String, String), Entry>>,
}

/// The installation row of an app and shop.
struct Stored {
    token: Option<String>,
    uninstalled: bool,
}

async fn stored(app: &OpenShop, shopify_app: &str, shop: &str) -> Result<Option<Stored>> {
    let row: Option<(Option<String>, bool)> = sqlx::query_as(
        "SELECT access_token, uninstalled_at IS NOT NULL FROM installations \
         WHERE app_handle = $1 AND shop = $2 LIMIT 1",
    )
    .bind(shopify_app)
    .bind(shop)
    .fetch_optional(app.db())
    .await?;
    let Some((token, uninstalled)) = row else {
        return Ok(None);
    };
    Ok(Some(Stored {
        token: app.crypto().decrypt_string(token.as_deref())?,
        uninstalled,
    }))
}

/// Whether a refusal at `refused_at` still holds back another request.
fn held_back(refused_at: Option<Instant>) -> bool {
    refused_at.is_some_and(|refused_at| refused_at.elapsed() < REFUSAL_MEMORY)
}

fn digest(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

impl ClientCredentials {
    /// The grant of the application. `SHOPIFY_CLIENT_CREDENTIALS_SHOP`, when
    /// set, names the shop background work may ask a token for.
    pub fn from_env(value: Option<&str>) -> Result<Self> {
        let configured = match value {
            None => None,
            Some(value) => match strict_shop_domain(value) {
                Some(shop) => Some(shop),
                None => bail!("[openshop] {CLIENT_CREDENTIALS_SHOP_VAR} must be a *.myshopify.com domain"),
            },
        };
        Ok(Self {
            configured,
            entries: StdMutex::new(HashMap::new()),
        })
    }

    /// The shop of `SHOPIFY_CLIENT_CREDENTIALS_SHOP`, if it is set.
    pub fn configured_shop(&self) -> Option<&str> {
        self.configured.as_deref()
    }

    /// Whether `grant` lets a caller ask a token for `shop`, a strict shop domain.
    pub fn may_grant(&self, grant: Option<Grant>, shop: &str) -> bool {
        match grant {
            None => false,
            Some(Grant::Vouched) => strict_shop_domain(shop).is_some(),
            Some(Grant::Configured) => self.configured.as_deref() == Some(shop),
        }
    }

    fn entries(&self) -> MutexGuard<'_, HashMap<(String, String), Entry>> {
        self.entries.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn with_entry<T>(&self, shopify_app: &str, shop: &str, read: impl FnOnce(&mut Entry) -> T) -> T {
        let mut entries = self.entries();
        read(entries
            .entry((shopify_app.to_string(), shop.to_string()))
            .or_default())
    }

    /// Whether `token` is one this process obtained and that expires within
    /// five minutes. A token stored by OAuth, by the token exchange or by
    /// another process is not known to expire.
    pub fn expiring(&self, shopify_app: &str, shop: &str, token: &str) -> bool {
        let entries = self.entries();
        let Some(Entry {
            granted: Some((granted, Some(expires_at))),
            ..
        }) = entries.get(&(shopify_app.to_string(), shop.to_string()))
        else {
            return false;
        };
        *granted == digest(token) && *expires_at - RENEWAL_MARGIN <= Utc::now()
    }

    /// The token to send to the Admin API of `shop` (a strict shop domain)
    /// for the Shopify app `shopify_app`: the stored one, unless it was just
    /// `rejected` with a `401` or is about to expire, or the shop has none and
    /// `grant` allows asking Shopify. When Shopify gives none, the stored
    /// token, whatever it is.
    pub async fn token(
        &self,
        app: &OpenShop,
        shopify_app: &str,
        shop: &str,
        grant: Option<Grant>,
        rejected: Option<&str>,
    ) -> Result<Option<String>> {
        let stored = stored(app, shopify_app, shop).await?;
        let (current, uninstalled) = match stored {
            Some(Stored { token, uninstalled }) => (token, uninstalled),
            None => (None, false),
        };
        let Some(grant) = grant.filter(|grant| self.may_grant(Some(*grant), shop)) else {
            return Ok(current);
        };
        let usable = current.as_deref().is_some_and(|token| {
            Some(token) != rejected
                && !self.expiring(shopify_app, shop, token)
                && !(uninstalled && grant == Grant::Vouched)
        });
        if usable {
            return Ok(current);
        }
        let granted = self.grant(app, shopify_app, shop, grant, rejected).await?;
        Ok(granted.or(current))
    }

    /// Asks Shopify for a token of `shop` with the client credentials of the
    /// Shopify app `shopify_app`, and stores it encrypted. `None` when `grant`
    /// does not allow it, when Shopify refused within the hour, or refuses now.
    ///
    /// One request at a time asks for an app and shop: the others wait, then
    /// use the token it stored, or fall back at once when it was refused.
    pub async fn grant(
        &self,
        app: &OpenShop,
        shopify_app: &str,
        shop: &str,
        grant: Grant,
        rejected: Option<&str>,
    ) -> Result<Option<String>> {
        let Some(shop) = strict_shop_domain(shop) else {
            return Ok(None);
        };
        if !self.may_grant(Some(grant), &shop) {
            return Ok(None);
        }

        let asking = self.with_entry(shopify_app, &shop, |entry| entry.asking.clone());
        let _asking = asking.lock().await;

        if self.with_entry(shopify_app, &shop, |entry| held_back(entry.refused_at)) {
            return Ok(None);
        }

        // Another request may have stored a token while this one waited.
        let stored = stored(app, shopify_app, &shop).await?;
        if let Some(stored) = &stored {
            if grant == Grant::Configured && stored.uninstalled {
                return Ok(None);
            }
            if let Some(token) = stored.token.as_deref()
                && Some(token) != rejected
                && !stored.uninstalled
                && !self.expiring(shopify_app, &shop, token)
            {
                return Ok(Some(token.to_string()));
            }
        }

        let resolved = app.apps().by_handle(Some(shopify_app))?;
        if resolved.api_key.is_empty() || resolved.api_secret.is_empty() {
            return Ok(None);
        }
        let token = match request(app, shopify_app, &shop, &resolved.api_key, &resolved.api_secret).await {
            Ok(token) => token,
            Err(error) => {
                logger::warn(format!("{error:#}"));
                self.with_entry(shopify_app, &shop, |entry| entry.refused_at = Some(Instant::now()));
                return Ok(None);
            }
        };

        let encrypted = match app.crypto().encrypt_string(&token.value) {
            Ok(encrypted) => encrypted,
            // Production without `ENCRYPTION_KEY`: the token is never stored in plaintext.
            Err(error) => {
                logger::warn(format!("{error:#}"));
                self.with_entry(shopify_app, &shop, |entry| entry.refused_at = Some(Instant::now()));
                return Ok(None);
            }
        };
        let written: Option<(uuid::Uuid,)> = match grant {
            // Shopify vouched for the shop: it is installed again.
            Grant::Vouched => {
                sqlx::query_as(
                    "INSERT INTO installations (app_handle, shop, access_token, scopes) VALUES ($1, $2, $3, $4) \
                     ON CONFLICT (app_handle, shop) DO UPDATE SET \
                       access_token = EXCLUDED.access_token, \
                       scopes = COALESCE(EXCLUDED.scopes, installations.scopes), \
                       uninstalled_at = NULL \
                     RETURNING id",
                )
                .bind(shopify_app)
                .bind(&shop)
                .bind(&encrypted)
                .bind(&token.scopes)
                .fetch_optional(app.db())
                .await?
            }
            // Background work never brings back a shop marked uninstalled.
            Grant::Configured => {
                sqlx::query_as(
                    "INSERT INTO installations (app_handle, shop, access_token, scopes) VALUES ($1, $2, $3, $4) \
                     ON CONFLICT (app_handle, shop) DO UPDATE SET \
                       access_token = EXCLUDED.access_token, \
                       scopes = COALESCE(EXCLUDED.scopes, installations.scopes) \
                     WHERE installations.uninstalled_at IS NULL \
                     RETURNING id",
                )
                .bind(shopify_app)
                .bind(&shop)
                .bind(&encrypted)
                .bind(&token.scopes)
                .fetch_optional(app.db())
                .await?
            }
        };
        if written.is_none() {
            return Ok(None);
        }

        self.with_entry(shopify_app, &shop, |entry| {
            entry.refused_at = None;
            entry.granted = Some((digest(&token.value), token.expires_at));
        });
        logger::info(format!(
            "[openshop] Client credentials token obtained for {shop} (app \"{shopify_app}\")"
        ));
        Ok(Some(token.value))
    }
}

/// What Shopify answered: the token, when it expires and its scopes.
struct Granted {
    value: String,
    expires_at: Option<DateTime<Utc>>,
    scopes: Option<String>,
}

/// Asks Shopify for a token. An error names the HTTP status and the error
/// code Shopify gives, never the request or the rest of the answer.
async fn request(app: &OpenShop, shopify_app: &str, shop: &str, api_key: &str, api_secret: &str) -> Result<Granted> {
    let response = app
        .transport()
        .post_json(OutboundRequest {
            url: format!("https://{shop}/admin/oauth/access_token"),
            headers: vec![("Content-Type".to_string(), FORM_CONTENT_TYPE.to_string())],
            body: json!({
                "grant_type": "client_credentials",
                "client_id": api_key,
                "client_secret": api_secret,
            }),
        })
        .await;
    let response = match response {
        Ok(response) => response,
        // The error of the transport names the address at most, never the body.
        Err(error) => {
            bail!("[openshop] The client credentials request of app \"{shopify_app}\" for {shop} failed: {error}")
        }
    };

    let data: Option<Value> = response.json().ok();
    if !response.ok() {
        bail!(refusal(shopify_app, shop, response.status, data.as_ref()));
    }

    let Some(value) = data
        .as_ref()
        .and_then(|data| data.get("access_token"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    else {
        bail!("[openshop] Shopify answered the client credentials of app \"{shopify_app}\" for {shop} without a token");
    };
    let expires_at = data
        .as_ref()
        .and_then(|data| data.get("expires_in"))
        .and_then(Value::as_i64)
        .filter(|expires_in| *expires_in > 0)
        .and_then(TimeDelta::try_seconds)
        .map(|expires_in| Utc::now() + expires_in);
    let scopes = data
        .as_ref()
        .and_then(|data| data.get("scope"))
        .and_then(Value::as_str)
        .filter(|scopes| !scopes.is_empty())
        .map(str::to_string);
    Ok(Granted {
        value: value.to_string(),
        expires_at,
        scopes,
    })
}

/// The message of a refusal: the status, and Shopify's error code when it
/// gives a short one.
fn refusal(shopify_app: &str, shop: &str, status: u16, data: Option<&Value>) -> String {
    let code = data.and_then(error_code).map(|code| format!(": {code}")).unwrap_or_default();
    format!("[openshop] Shopify refused the client credentials of app \"{shopify_app}\" for {shop} ({status}{code})")
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
    fn reads_the_shop_of_background_work() {
        assert_eq!(ClientCredentials::from_env(None).unwrap().configured_shop(), None);
        let credentials = ClientCredentials::from_env(Some(" Dev-Store-1.MyShopify.com ")).unwrap();
        assert_eq!(credentials.configured_shop(), Some("dev-store-1.myshopify.com"));
        let bare = ClientCredentials::from_env(Some("dev-store-1")).unwrap();
        assert_eq!(bare.configured_shop(), Some("dev-store-1.myshopify.com"));

        for value in [
            "example.com",
            "dev-store.myshopify.com.evil.example",
            "not a shop",
            "https://dev-store-1.myshopify.com",
            "evil.example://dev-store-1.myshopify.com",
            "dev-store-1.myshopify.com:8443",
        ] {
            let error = ClientCredentials::from_env(Some(value)).err().unwrap().to_string();
            assert_eq!(
                error,
                "[openshop] SHOPIFY_CLIENT_CREDENTIALS_SHOP must be a *.myshopify.com domain"
            );
        }
    }

    #[test]
    fn background_work_may_only_ask_for_the_configured_shop() {
        let credentials = ClientCredentials::from_env(Some("dev-store-1.myshopify.com")).unwrap();
        assert!(credentials.may_grant(Some(Grant::Configured), "dev-store-1.myshopify.com"));
        assert!(!credentials.may_grant(Some(Grant::Configured), "dev-store-2.myshopify.com"));
        assert!(!credentials.may_grant(Some(Grant::Configured), "Dev-Store-1.myshopify.com"));
        assert!(!credentials.may_grant(None, "dev-store-1.myshopify.com"));
        assert!(credentials.may_grant(Some(Grant::Vouched), "dev-store-2.myshopify.com"));
        assert!(!credentials.may_grant(Some(Grant::Vouched), "evil.example://dev-store-2.myshopify.com"));

        let off = ClientCredentials::from_env(None).unwrap();
        assert!(!off.may_grant(Some(Grant::Configured), "dev-store-1.myshopify.com"));
        assert!(off.may_grant(Some(Grant::Vouched), "dev-store-1.myshopify.com"));
    }

    #[test]
    fn only_a_token_this_process_obtained_expires() {
        let credentials = ClientCredentials::from_env(None).unwrap();
        let shop = "dev-store-1.myshopify.com";
        let set = |expires_at: Option<DateTime<Utc>>| {
            credentials.with_entry("default", shop, |entry| entry.granted = Some((digest("granted"), expires_at)));
        };
        assert!(!credentials.expiring("default", shop, "granted"));

        set(Some(Utc::now() + TimeDelta::minutes(6)));
        assert!(!credentials.expiring("default", shop, "granted"));
        set(Some(Utc::now() + TimeDelta::minutes(4)));
        assert!(credentials.expiring("default", shop, "granted"));
        set(Some(Utc::now() - TimeDelta::minutes(1)));
        assert!(credentials.expiring("default", shop, "granted"));
        // Another token, such as one OAuth stored since, is not this one.
        assert!(!credentials.expiring("default", shop, "oauth-token"));
        assert!(!credentials.expiring("other", shop, "granted"));
        // Without `expires_in`, only a `401` renews it.
        set(None);
        assert!(!credentials.expiring("default", shop, "granted"));
    }

    #[test]
    fn a_refusal_is_remembered_for_an_hour() {
        assert!(!held_back(None));
        assert!(held_back(Some(Instant::now())));
        if let Some(earlier) = Instant::now().checked_sub(REFUSAL_MEMORY - Duration::from_secs(60)) {
            assert!(held_back(Some(earlier)));
        }
        if let Some(earlier) = Instant::now().checked_sub(REFUSAL_MEMORY + Duration::from_secs(1)) {
            assert!(!held_back(Some(earlier)));
        }
    }

    #[test]
    fn a_refusal_names_the_status_and_the_code_only() {
        let data = json!({ "error": "shop_not_permitted", "error_description": "dev-app-key cannot use dev-app-secret" });
        let message = refusal("default", "dev-store-1.myshopify.com", 400, Some(&data));
        assert_eq!(
            message,
            "[openshop] Shopify refused the client credentials of app \"default\" for dev-store-1.myshopify.com \
             (400: shop_not_permitted)"
        );
        let message = refusal(
            "default",
            "dev-store-1.myshopify.com",
            500,
            Some(&json!({ "errors": "Something went wrong" })),
        );
        assert!(message.ends_with("for dev-store-1.myshopify.com (500)"), "{message}");
        assert!(refusal("default", "dev-store-1.myshopify.com", 502, None).ends_with("(502)"));
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
