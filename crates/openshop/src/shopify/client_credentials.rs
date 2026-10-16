//! Access tokens obtained with Shopify's client credentials grant.
//!
//! Shopify gives such a token with no browser and no merchant, but only when
//! the app and the store belong to the same Shopify organization, as the
//! development stores of the organization that owns the app do. It lasts about
//! 24 hours and comes without a refresh token.
//!
//! A token is only asked for a shop Shopify vouched for, for the one shop the
//! operator named, or to renew a token a shop already holds:
//!
//! - [`Grant::Vouched`]: a launch from Shopify admin whose signed query
//!   covers `shop` and is recent, or a verified session token (the shop of
//!   its `dest`). It names the app and the shop it was given for, and serves
//!   them only. Any shop without a usable token; a success reinstalls a shop
//!   marked uninstalled.
//! - [`Grant::Background`]: anything else that may ask (flows, crons, routes,
//!   proxies, the MCP tool). The shop of `SHOPIFY_CLIENT_CREDENTIALS_SHOP`,
//!   with or without a row; any other shop only to renew the token its row
//!   holds (after a `401`, or before a token this process obtained expires),
//!   which proves an earlier installation through a signed flow. It never
//!   creates a row for such a shop, and never writes on a row marked
//!   uninstalled.
//!
//! A definite refusal (a `4xx` with Shopify's error code) is remembered in
//! the process for an hour per app and shop; any other failure (a `5xx`, a
//! `429`, a timeout, a network error) for thirty seconds. The caller falls
//! back to what it did without the grant. Nothing of it is stored in the
//! database but the token, encrypted, in `installations`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex, MutexGuard};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use chrono::{DateTime, TimeDelta, Utc};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use tokio::time::Instant;

use crate::app::OpenShop;
use crate::runtime::{FORM_CONTENT_TYPE, HttpTransport, OutboundRequest, logger};
use crate::server::shop_domain::strict_shop_domain;

/// The environment variable that names the shop of background work.
pub const CLIENT_CREDENTIALS_SHOP_VAR: &str = "SHOPIFY_CLIENT_CREDENTIALS_SHOP";

/// A token this process obtained is renewed this long before it expires.
const RENEWAL_MARGIN: TimeDelta = TimeDelta::minutes(5);

/// How long a token request may take, whatever the transport allows: a
/// request that waits for it may be an admin page loading.
pub(crate) const GRANT_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a failure that may pass (a `5xx`, a `429`, a timeout, a network
/// error) holds back another request for the same app and shop.
pub(crate) const TRANSIENT_BACKOFF: Duration = Duration::from_secs(30);

/// How long a definite refusal (a `4xx` with Shopify's error code, such as
/// `shop_not_permitted`) holds back another request for the same app and shop.
pub(crate) const REFUSAL_MEMORY: Duration = Duration::from_secs(60 * 60);

/// What lets a caller ask Shopify for a token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Grant {
    /// Shopify vouched for this shop, for this app: a recent signed launch,
    /// or a verified session token. Serves that app and shop only.
    Vouched { shopify_app: String, shop: String },
    /// No request vouches for the shop: the shop of
    /// `SHOPIFY_CLIENT_CREDENTIALS_SHOP`, or the renewal of a token a shop's
    /// row holds.
    Background,
}

impl Grant {
    /// What Shopify vouched for: `shop`, a strict shop domain, for the
    /// Shopify app `shopify_app`.
    pub fn vouched(shopify_app: &str, shop: &str) -> Option<Self> {
        Some(Self::Vouched {
            shopify_app: shopify_app.to_string(),
            shop: strict_shop_domain(shop)?,
        })
    }
}

/// What the process knows of one app and shop.
#[derive(Default)]
struct Entry {
    /// Held while Shopify is asked, so that one request at a time does.
    asking: Arc<Mutex<()>>,
    /// Until when the last failure holds back another request.
    held_until: Option<Instant>,
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

/// Whether a failure that holds back until `held_until` still does.
fn held_back(held_until: Option<Instant>) -> bool {
    held_until.is_some_and(|held_until| Instant::now() < held_until)
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

    /// Whether `grant` lets a caller ask a token of `shop`, a strict shop
    /// domain, for the Shopify app `shopify_app`, given its row (`stored`)
    /// and whether the token of that row is being `renewed`.
    fn may_ask(&self, grant: &Grant, shopify_app: &str, shop: &str, stored: Option<&Stored>, renewed: bool) -> bool {
        match grant {
            Grant::Vouched {
                shopify_app: vouched_app,
                shop: vouched_shop,
            } => vouched_app == shopify_app && vouched_shop == shop,
            Grant::Background => {
                self.configured.as_deref() == Some(shop)
                    || (renewed && stored.is_some_and(|stored| stored.token.is_some() && !stored.uninstalled))
            }
        }
    }

    /// Whether `grant` lets a caller ask a token of `shop` (a strict shop
    /// domain) when it has none.
    pub fn may_ask_without_token(&self, grant: Option<&Grant>, shopify_app: &str, shop: &str) -> bool {
        grant.is_some_and(|grant| self.may_ask(grant, shopify_app, shop, None, false))
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

    /// The token to send to the Admin API of `shop` for the Shopify app
    /// `shopify_app`: the stored one, unless it was just `rejected` with a
    /// `401`, is about to expire, or the shop has none (or is marked
    /// uninstalled, for a vouched grant), and `grant` allows asking Shopify.
    /// When Shopify gives none, the stored token, whatever it is.
    pub async fn token(
        &self,
        app: &OpenShop,
        shopify_app: &str,
        shop: &str,
        grant: Option<&Grant>,
        rejected: Option<&str>,
    ) -> Result<Option<String>> {
        let stored = stored(app, shopify_app, shop).await?;
        let current = stored.as_ref().and_then(|stored| stored.token.clone());
        let (Some(grant), Some(shop)) = (grant, strict_shop_domain(shop)) else {
            return Ok(current);
        };
        let uninstalled = stored.as_ref().is_some_and(|stored| stored.uninstalled);
        let renewed = current
            .as_deref()
            .is_some_and(|token| Some(token) == rejected || self.expiring(shopify_app, &shop, token));
        let reinstalled = uninstalled && matches!(grant, Grant::Vouched { .. });
        if current.is_some() && !renewed && !reinstalled {
            return Ok(current);
        }
        if !self.may_ask(grant, shopify_app, &shop, stored.as_ref(), renewed) {
            return Ok(current);
        }
        let granted = self.grant(app, shopify_app, &shop, grant, rejected).await?;
        Ok(granted.or(current))
    }

    /// Asks Shopify for a token of `shop` with the client credentials of the
    /// Shopify app `shopify_app`, and stores it encrypted. `None` when `grant`
    /// does not allow it, when a failure still holds requests back, or when
    /// Shopify refuses now.
    ///
    /// One request at a time asks for an app and shop: the others wait, then
    /// use the token it stored, or fall back at once when it failed.
    ///
    /// Fails, without asking Shopify, when the token could not be stored
    /// encrypted (production without `ENCRYPTION_KEY`), and with a message that
    /// names neither the token nor its ciphertext when it could not be written.
    pub async fn grant(
        &self,
        app: &OpenShop,
        shopify_app: &str,
        shop: &str,
        grant: &Grant,
        rejected: Option<&str>,
    ) -> Result<Option<String>> {
        let Some(shop) = strict_shop_domain(shop) else {
            return Ok(None);
        };
        if let Grant::Vouched { .. } = grant
            && !self.may_ask(grant, shopify_app, &shop, None, false)
        {
            return Ok(None);
        }

        let asking = self.with_entry(shopify_app, &shop, |entry| entry.asking.clone());
        let _asking = asking.lock().await;

        if self.with_entry(shopify_app, &shop, |entry| held_back(entry.held_until)) {
            return Ok(None);
        }

        // Another request may have stored a token while this one waited.
        let stored = stored(app, shopify_app, &shop).await?;
        if let Some(stored) = &stored {
            if *grant == Grant::Background && stored.uninstalled {
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
        // Past this point, a token the row holds is one to replace.
        let renewed = stored.as_ref().is_some_and(|stored| stored.token.is_some());
        if !self.may_ask(grant, shopify_app, &shop, stored.as_ref(), renewed) {
            return Ok(None);
        }

        let resolved = app.apps().by_handle(Some(shopify_app))?;
        if resolved.api_key.is_empty() || resolved.api_secret.is_empty() {
            return Ok(None);
        }
        // A configuration error, not a refusal: nothing is asked, nothing is
        // held back, and the token is never stored in plaintext.
        if let Err(error) = app.crypto().can_encrypt() {
            bail!("[openshop] Cannot store a client credentials token for {shop}: {error}");
        }

        let token = match request(
            app.transport().as_ref(),
            shopify_app,
            &shop,
            &resolved.api_key,
            &resolved.api_secret,
        )
        .await
        {
            Ok(token) => token,
            Err(failure) => {
                logger::warn(&failure.message);
                let held_until = Instant::now() + failure.holds_back();
                self.with_entry(shopify_app, &shop, |entry| entry.held_until = Some(held_until));
                return Ok(None);
            }
        };

        let unwritten = || anyhow!("[openshop] Could not store the client credentials token of app \"{shopify_app}\" for {shop}");
        let encrypted = app.crypto().encrypt_string(&token.value).map_err(|_| unwritten())?;
        let written: Option<(uuid::Uuid,)> = match grant {
            // Shopify vouched for the shop: it is installed, again if it was not.
            Grant::Vouched { .. } => sqlx::query_as(
                "INSERT INTO installations (app_handle, shop, access_token, scopes) VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (app_handle, shop) DO UPDATE SET \
                   access_token = EXCLUDED.access_token, \
                   scopes = COALESCE(EXCLUDED.scopes, installations.scopes), \
                   installed_at = CASE \
                     WHEN installations.uninstalled_at IS NOT NULL OR installations.access_token IS NULL THEN now() \
                     ELSE installations.installed_at END, \
                   uninstalled_at = NULL \
                 RETURNING id",
            ),
            // The configured shop: a row is created when there is none, but a
            // row marked uninstalled is never brought back.
            Grant::Background if self.configured.as_deref() == Some(shop.as_str()) => sqlx::query_as(
                "INSERT INTO installations (app_handle, shop, access_token, scopes) VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (app_handle, shop) DO UPDATE SET \
                   access_token = EXCLUDED.access_token, \
                   scopes = COALESCE(EXCLUDED.scopes, installations.scopes) \
                 WHERE installations.uninstalled_at IS NULL \
                 RETURNING id",
            ),
            // Any other shop: the token of its row is replaced, nothing else.
            Grant::Background => sqlx::query_as(
                "UPDATE installations SET access_token = $3, scopes = COALESCE($4, scopes) \
                 WHERE app_handle = $1 AND shop = $2 AND access_token IS NOT NULL AND uninstalled_at IS NULL \
                 RETURNING id",
            ),
        }
        .bind(shopify_app)
        .bind(&shop)
        .bind(&encrypted)
        .bind(&token.scopes)
        .fetch_optional(app.db())
        .await
        // A database error may quote the row it refused: it is left out.
        .map_err(|_| unwritten())?;
        if written.is_none() {
            return Ok(None);
        }

        self.with_entry(shopify_app, &shop, |entry| {
            entry.held_until = None;
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

/// Why no token came: a message that names the HTTP status and Shopify's
/// error code at most, and whether Shopify definitely refused.
#[derive(Debug)]
struct Failure {
    message: String,
    definite: bool,
}

impl Failure {
    fn transient(message: String) -> Self {
        Self {
            message,
            definite: false,
        }
    }

    /// How long it holds back another request for the same app and shop.
    fn holds_back(&self) -> Duration {
        if self.definite { REFUSAL_MEMORY } else { TRANSIENT_BACKOFF }
    }
}

/// Asks Shopify for a token, for [`GRANT_TIMEOUT`] at most. A failure names
/// the HTTP status and the error code Shopify gives, never the request or the
/// rest of the answer.
async fn request(
    transport: &dyn HttpTransport,
    shopify_app: &str,
    shop: &str,
    api_key: &str,
    api_secret: &str,
) -> std::result::Result<Granted, Failure> {
    let sent = transport.post_json(OutboundRequest {
        url: format!("https://{shop}/admin/oauth/access_token"),
        headers: vec![("Content-Type".to_string(), FORM_CONTENT_TYPE.to_string())],
        body: json!({
            "grant_type": "client_credentials",
            "client_id": api_key,
            "client_secret": api_secret,
        }),
    });
    let response = match tokio::time::timeout(GRANT_TIMEOUT, sent).await {
        Ok(Ok(response)) => response,
        // The error of the transport names the address at most, never the body.
        Ok(Err(error)) => {
            return Err(Failure::transient(format!(
                "[openshop] The client credentials request of app \"{shopify_app}\" for {shop} failed: {error}"
            )));
        }
        Err(_) => {
            return Err(Failure::transient(format!(
                "[openshop] The client credentials request of app \"{shopify_app}\" for {shop} timed out after {} s",
                GRANT_TIMEOUT.as_secs()
            )));
        }
    };

    let data: Option<Value> = response.json().ok();
    if !response.ok() {
        return Err(Failure {
            message: refusal(shopify_app, shop, response.status, data.as_ref()),
            definite: definite_refusal(response.status, data.as_ref()),
        });
    }

    let Some(value) = data
        .as_ref()
        .and_then(|data| data.get("access_token"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    else {
        return Err(Failure::transient(format!(
            "[openshop] Shopify answered the client credentials of app \"{shopify_app}\" for {shop} without a token"
        )));
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

/// Whether Shopify definitely refused: a `4xx` other than `408` and `429`
/// that carries an error code, such as `shop_not_permitted`,
/// `invalid_client` or `invalid_request`.
fn definite_refusal(status: u16, data: Option<&Value>) -> bool {
    (400..500).contains(&status) && !matches!(status, 408 | 429) && data.and_then(error_code).is_some()
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

    fn stored(token: Option<&str>, uninstalled: bool) -> Stored {
        Stored {
            token: token.map(str::to_string),
            uninstalled,
        }
    }

    #[test]
    fn background_work_asks_for_the_configured_shop_or_to_renew_a_stored_token() {
        let credentials = ClientCredentials::from_env(Some("dev-store-1.myshopify.com")).unwrap();
        let background = Grant::Background;
        let ask = |shop: &str, row: Option<&Stored>, renewed: bool| {
            credentials.may_ask(&background, "default", shop, row, renewed)
        };
        assert!(ask("dev-store-1.myshopify.com", None, false));
        assert!(!ask("dev-store-2.myshopify.com", None, false));
        assert!(!ask("Dev-Store-1.myshopify.com", None, false));
        // Another shop: only the token its row holds is renewed.
        assert!(ask("dev-store-2.myshopify.com", Some(&stored(Some("t"), false)), true));
        assert!(!ask("dev-store-2.myshopify.com", Some(&stored(Some("t"), false)), false));
        assert!(!ask("dev-store-2.myshopify.com", Some(&stored(Some("t"), true)), true));
        assert!(!ask("dev-store-2.myshopify.com", Some(&stored(None, false)), true));
        assert!(!ask("dev-store-2.myshopify.com", None, true));
        assert!(credentials.may_ask_without_token(Some(&background), "default", "dev-store-1.myshopify.com"));
        assert!(!credentials.may_ask_without_token(Some(&background), "default", "dev-store-2.myshopify.com"));
        assert!(!credentials.may_ask_without_token(None, "default", "dev-store-1.myshopify.com"));

        let off = ClientCredentials::from_env(None).unwrap();
        assert!(!off.may_ask(&background, "default", "dev-store-1.myshopify.com", None, false));
        assert!(off.may_ask(
            &background,
            "default",
            "dev-store-1.myshopify.com",
            Some(&stored(Some("t"), false)),
            true
        ));
    }

    #[test]
    fn a_vouched_grant_serves_the_app_and_shop_it_was_given_for_only() {
        let credentials = ClientCredentials::from_env(Some("dev-store-1.myshopify.com")).unwrap();
        let vouched = Grant::vouched("first", " Dev-Store-2.MyShopify.com ").unwrap();
        assert_eq!(
            vouched,
            Grant::Vouched {
                shopify_app: "first".to_string(),
                shop: "dev-store-2.myshopify.com".to_string(),
            }
        );
        assert!(credentials.may_ask(&vouched, "first", "dev-store-2.myshopify.com", None, false));
        assert!(!credentials.may_ask(&vouched, "second", "dev-store-2.myshopify.com", None, false));
        // Not even the configured shop: a vouched grant is no background grant.
        assert!(!credentials.may_ask(&vouched, "first", "dev-store-1.myshopify.com", None, false));
        assert!(!credentials.may_ask(&vouched, "first", "dev-store-3.myshopify.com", None, false));
        assert_eq!(Grant::vouched("first", "evil.example://dev-store-2.myshopify.com"), None);
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
    fn a_failure_holds_back_until_its_time_is_up() {
        assert!(!held_back(None));
        assert!(held_back(Some(Instant::now() + Duration::from_secs(1))));
        assert!(!held_back(Some(Instant::now())));
        assert_eq!(TRANSIENT_BACKOFF, Duration::from_secs(30));
        assert_eq!(REFUSAL_MEMORY, Duration::from_secs(60 * 60));
        assert_eq!(GRANT_TIMEOUT, Duration::from_secs(5));
    }

    #[test]
    fn only_a_refusal_with_a_code_is_definite() {
        let code = |code: &str| json!({ "error": code });
        for (status, data) in [
            (400, code("shop_not_permitted")),
            (401, code("invalid_client")),
            (400, code("invalid_request")),
            (403, json!({ "errors": "unauthorized_client" })),
        ] {
            assert!(definite_refusal(status, Some(&data)), "{status} {data}");
        }
        for (status, data) in [
            (429, Some(code("too_many_requests"))),
            (408, Some(code("timeout"))),
            (500, Some(code("internal_error"))),
            (503, None),
            (400, None),
            (400, Some(json!({ "error": "a sentence with spaces" }))),
        ] {
            assert!(!definite_refusal(status, data.as_ref()), "{status} {data:?}");
        }
    }

    struct Answer(Option<(u16, Value)>);

    impl HttpTransport for Answer {
        fn post_json(
            &self,
            _request: OutboundRequest,
        ) -> futures::future::BoxFuture<'_, Result<crate::runtime::OutboundResponse>> {
            Box::pin(async move {
                match &self.0 {
                    Some((status, body)) => Ok(crate::runtime::OutboundResponse {
                        status: *status,
                        body: body.to_string(),
                    }),
                    // Never answers, as a host that accepts the connection and hangs.
                    None => std::future::pending().await,
                }
            })
        }
    }

    async fn failure(transport: Answer) -> Failure {
        match request(&transport, "default", "dev-store-1.myshopify.com", "key", "secret").await {
            Ok(_) => panic!("expected a failure"),
            Err(failure) => failure,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_request_that_hangs_gives_up_after_five_seconds() {
        let started = Instant::now();
        let failure = failure(Answer(None)).await;
        assert_eq!(started.elapsed(), GRANT_TIMEOUT);
        assert!(!failure.definite);
        assert_eq!(failure.holds_back(), TRANSIENT_BACKOFF);
        assert_eq!(
            failure.message,
            "[openshop] The client credentials request of app \"default\" for dev-store-1.myshopify.com timed out after 5 s"
        );
    }

    #[tokio::test]
    async fn a_refusal_holds_back_for_an_hour_and_an_outage_for_thirty_seconds() {
        let refused = failure(Answer(Some((400, json!({ "error": "shop_not_permitted" }))))).await;
        assert_eq!(refused.holds_back(), REFUSAL_MEMORY);
        for (status, body) in [(503, json!({})), (429, json!({ "errors": "Throttled" })), (200, json!({}))] {
            let failure = failure(Answer(Some((status, body)))).await;
            assert_eq!(failure.holds_back(), TRANSIENT_BACKOFF, "{status}");
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
