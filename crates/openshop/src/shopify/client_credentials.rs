//! Access tokens obtained with Shopify's client credentials grant.
//!
//! Shopify gives such a token with no browser and no merchant, but only when
//! the app and the store belong to the same Shopify organization, as the
//! development stores of the organization that owns the app do. It lasts about
//! 24 hours and comes without a refresh token.
//!
//! The requests are `shopify_api`'s: each app, shop and kind of grant has one
//! shared [`Shopify`] client with [`Auth::client_credentials`], which asks for
//! a token at its first request, renews it five minutes before it expires and
//! once when the Admin API refuses it with a `401`, one renewal at a time for
//! everything that uses it. This module decides who may use such a client,
//! and stores what it obtains.
//!
//! A token is only asked for a shop Shopify vouched for, for the one shop the
//! operator named, or to renew a token a shop already holds:
//!
//! - [`Grant::Vouched`]: a launch from Shopify admin whose signed query
//!   covers `shop` and is recent, or a verified session token (the shop of
//!   its `dest`) whose token exchange Shopify refused. It names the app and
//!   the shop it was given for, and serves them only: for any other app or
//!   shop, it is a background grant. Any shop without a usable token; a
//!   success reinstalls a shop marked uninstalled.
//! - [`Grant::Background`]: anything else that may ask (flows, crons, routes,
//!   proxies, webhooks, the MCP tool). The shop the application names with
//!   its `client_credentials_shop` setting, with or without a row; any other shop
//!   only to renew the token its row holds, which proves an earlier
//!   installation through a signed flow. It never creates a row for such a
//!   shop, and never writes on a row marked uninstalled.
//!
//! A definite refusal (a `4xx` with Shopify's error code) is remembered in
//! the process for an hour per app and shop; any other failure (a `5xx`, a
//! `429`, a timeout, a network error, a token the database would not store)
//! for thirty seconds. The failure of a background request holds back
//! background requests only, so that a shop installed since is served at its
//! next launch; the failure of a vouched request holds back both kinds. The
//! caller then does what it did without the grant: it sends the stored token
//! as it is, or has none. Nothing of it is stored in the database but the
//! token, encrypted, in `installations`.
//!
//! A token is stored only where the row still holds what was read before
//! Shopify was asked: a shop uninstalled, or given another token by OAuth or
//! the token exchange, in the meantime keeps what it has.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex as StdMutex, MutexGuard};
use std::time::Duration;

use anyhow::{Result, bail};
use chrono::{DateTime, TimeDelta, Utc};
use sha2::{Digest, Sha256};
use shopify_api::transport::BoxFuture;
use shopify_api::{Auth, ShopDomain, Shopify, TokenData, TokenStore};
use sqlx::PgPool;
use tokio::sync::Mutex;
use tokio::time::Instant;

use super::token_exchange::{HOLD_BACK, TIMEOUT, TokenExchanges};
use super::{AppShop, app_shop};
use crate::app::OpenShop;
use crate::locked;
use crate::runtime::logger;
use crate::server::crypto::Crypto;
use crate::server::shop_domain::strict_shop_domain;

/// A token this process obtained is taken as expiring this long before it does.
const RENEWAL_MARGIN: TimeDelta = TimeDelta::minutes(5);

/// How long a definite refusal (a `4xx` with Shopify's error code, such as
/// `shop_not_permitted`) holds back another request for the same app and shop.
pub(crate) const REFUSAL_MEMORY: Duration = Duration::from_secs(60 * 60);

/// What lets a caller ask Shopify for a token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Grant {
    /// Shopify vouched for this shop, for this app: a recent signed launch,
    /// or a verified session token. Serves that app and shop only.
    Vouched { shopify_app: String, shop: String },
    /// No request vouches for the shop: the shop of the
    /// `client_credentials_shop` setting, or the renewal of a token a shop's
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

    /// What this grant lets a caller do for `shop` of the Shopify app
    /// `shopify_app`: Shopify vouched for that app and shop, or for another
    /// one, which leaves what background work may do.
    pub(crate) fn for_shop(&self, shopify_app: &str, shop: &str) -> Grant {
        match self {
            Grant::Vouched {
                shopify_app: vouched_app,
                shop: vouched_shop,
            } if vouched_app == shopify_app && vouched_shop == shop => self.clone(),
            _ => Grant::Background,
        }
    }

    fn is_vouched(&self) -> bool {
        matches!(self, Grant::Vouched { .. })
    }
}

/// What the process knows of one app and shop it asked Shopify for.
#[derive(Default, Clone, Copy)]
struct Entry {
    /// Until when the last failure of a background request holds back
    /// background requests.
    background_held: Option<Instant>,
    /// Until when the last failure of a vouched request holds back every
    /// request.
    vouched_held: Option<Instant>,
    /// The token this process obtained, as a digest, and when it expires.
    granted: Option<([u8; 32], DateTime<Utc>)>,
    /// Whether the token exchange was tried in place of that token.
    exchange_tried: bool,
}

impl Entry {
    /// Whether a failure still holds back a request of `grant`'s kind.
    fn holds_back(&self, grant: &Grant) -> bool {
        match grant {
            Grant::Vouched { .. } => held_back(self.vouched_held),
            Grant::Background => held_back(self.background_held) || held_back(self.vouched_held),
        }
    }

    /// Remembers a failure of a request of `grant`'s kind until `held_until`.
    fn hold_back(&mut self, grant: &Grant, held_until: Instant) {
        match grant {
            Grant::Vouched { .. } => self.vouched_held = Some(held_until),
            Grant::Background => self.background_held = Some(held_until),
        }
    }

    /// Whether it still keeps something: a failure that holds back, or a
    /// token of this process that has not expired.
    fn live(&self) -> bool {
        held_back(self.background_held)
            || held_back(self.vouched_held)
            || self.granted.is_some_and(|(_, expires_at)| Utc::now() < expires_at)
    }
}

type Entries = Arc<StdMutex<HashMap<AppShop, Entry>>>;

/// The shared client of an app, a shop, and whether Shopify vouched for it.
type ClientKey = (String, String, bool);

/// The token a shared client holds, as far as this process knows: the one it
/// started from, then each one it stored.
type HeldToken = Arc<StdMutex<Option<String>>>;

/// A client that obtains tokens with the grant, shared by every caller of
/// its app, shop and kind of grant, so that they make one request at a time.
/// Its [`HeldToken`] tells it from the clients that replace it.
#[derive(Clone)]
pub(crate) struct GrantClient {
    pub client: Shopify,
    token: HeldToken,
}

type Clients = Arc<StdMutex<HashMap<ClientKey, GrantClient>>>;

/// Forgets the shared client of `key` if it is still the one that holds
/// `token`: one that replaced it stays.
fn evict(clients: &StdMutex<HashMap<ClientKey, GrantClient>>, key: &ClientKey, token: &HeldToken) {
    let mut clients = locked(clients);
    if clients.get(key).is_some_and(|shared| Arc::ptr_eq(&shared.token, token)) {
        clients.remove(key);
    }
}

/// The error of a store that found another token in the row, stored by OAuth,
/// the token exchange or another process while Shopify was asked, or the shop
/// uninstalled: the row keeps what it has, and the caller starts again from it.
#[derive(Debug)]
pub(crate) struct Superseded(String);

impl std::fmt::Display for Superseded {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for Superseded {}

/// Whether `error` is a store that found the row changed.
fn superseded(error: &shopify_api::Error) -> bool {
    matches!(error, shopify_api::Error::TokenStore(inner) if inner.is::<Superseded>())
}

/// The client credentials grant of the application, and what this process
/// remembers of it.
pub(crate) struct ClientCredentials {
    configured: Option<String>,
    entries: Entries,
    /// The clients that obtain tokens with the grant, shared by every caller.
    clients: Clients,
    /// The Shopify apps whose configuration error background work reported.
    reported: StdMutex<HashSet<String>>,
    /// The token exchange of the session token, which falls back to this grant.
    pub(crate) exchanges: TokenExchanges,
}

/// The installation row of an app and shop.
pub(crate) struct Stored {
    pub token: Option<String>,
    /// The token as the row holds it, to write over it only if it is still there.
    ciphertext: Option<String>,
    pub uninstalled: bool,
}

impl Stored {
    /// Its token, unless the shop is marked uninstalled, or the token is one
    /// this process obtained that is about to expire.
    fn usable(&self, credentials: &ClientCredentials, shopify_app: &str, shop: &str) -> Option<&str> {
        self.token
            .as_deref()
            .filter(|token| !self.uninstalled && !credentials.expiring(shopify_app, shop, token))
    }
}

/// The installation row of an app and shop, its token decrypted.
pub(crate) async fn stored(app: &OpenShop, shopify_app: &str, shop: &str) -> Result<Option<Stored>> {
    let row: Option<(Option<String>, bool)> = sqlx::query_as(
        "SELECT access_token, uninstalled_at IS NOT NULL FROM installations \
         WHERE app_handle = $1 AND shop = $2 LIMIT 1",
    )
    .bind(shopify_app)
    .bind(shop)
    .fetch_optional(app.db())
    .await?;
    let Some((ciphertext, uninstalled)) = row else {
        return Ok(None);
    };
    Ok(Some(Stored {
        token: app.crypto().decrypt_string(ciphertext.as_deref())?,
        ciphertext,
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

/// Updates what the process knows of an app and shop it asked Shopify for.
/// An entry that keeps nothing is dropped, and a new one first drops every
/// entry that no longer keeps anything.
fn remember(entries: &Entries, shopify_app: &str, shop: &str, update: impl FnOnce(&mut Entry)) {
    let key = app_shop(shopify_app, shop);
    let mut entries = locked(entries);
    if !entries.contains_key(&key) {
        entries.retain(|_, entry| entry.live());
    }
    let entry = entries.entry(key.clone()).or_default();
    update(entry);
    if !entry.live() {
        entries.remove(&key);
    }
}

impl ClientCredentials {
    /// The grant of the application. Its `client_credentials_shop` setting,
    /// when given, names the shop background work may ask a token for.
    pub fn new(configured_shop: Option<&str>) -> Result<Self> {
        let configured = match configured_shop {
            None => None,
            Some(value) => match strict_shop_domain(value) {
                Some(shop) => Some(shop),
                None => bail!("[openshop] client_credentials_shop must be a *.myshopify.com domain"),
            },
        };
        Ok(Self {
            configured,
            entries: Arc::default(),
            clients: Arc::default(),
            reported: StdMutex::default(),
            exchanges: TokenExchanges::default(),
        })
    }

    /// The shop of the `client_credentials_shop` setting, if it is given.
    pub fn configured_shop(&self) -> Option<&str> {
        self.configured.as_deref()
    }

    /// Whether `grant`, already narrowed to this app and shop by
    /// [`Grant::for_shop`], lets a caller ask a token of `shop`, a strict shop
    /// domain, given its row (`stored`).
    fn may_ask(&self, grant: &Grant, shop: &str, stored: Option<&Stored>) -> bool {
        match grant {
            Grant::Vouched { .. } => true,
            Grant::Background => {
                if stored.is_some_and(|stored| stored.uninstalled) {
                    return false;
                }
                self.configured.as_deref() == Some(shop) || stored.is_some_and(|stored| stored.token.is_some())
            }
        }
    }

    fn entries(&self) -> MutexGuard<'_, HashMap<AppShop, Entry>> {
        locked(&self.entries)
    }

    fn remember(&self, shopify_app: &str, shop: &str, update: impl FnOnce(&mut Entry)) {
        remember(&self.entries, shopify_app, shop, update);
    }

    /// Whether a failure still holds back a request of `grant`'s kind for
    /// this app and shop.
    fn held_back(&self, shopify_app: &str, shop: &str, grant: &Grant) -> bool {
        self.entries()
            .get(&app_shop(shopify_app, shop))
            .is_some_and(|entry| entry.holds_back(grant))
    }

    /// Whether background work should report, for the Shopify app
    /// `shopify_app`, a configuration error it meets at every tick: once per
    /// process.
    pub fn first_report(&self, shopify_app: &str) -> bool {
        locked(&self.reported).insert(shopify_app.to_string())
    }

    /// Whether `token` is one this process obtained and that expires within
    /// five minutes. A token stored by OAuth, by the token exchange or by
    /// another process is not known to expire.
    pub fn expiring(&self, shopify_app: &str, shop: &str, token: &str) -> bool {
        let entries = self.entries();
        let Some(Entry {
            granted: Some((granted, expires_at)),
            ..
        }) = entries.get(&app_shop(shopify_app, shop))
        else {
            return false;
        };
        *granted == digest(token) && *expires_at - RENEWAL_MARGIN <= Utc::now()
    }

    /// Whether `token` is one this process obtained, for which the token
    /// exchange was not tried yet: the first call says yes, the next ones no.
    /// The offline token of the exchange does not expire, and is kept over a
    /// token of about 24 hours when Shopify gives it.
    pub fn exchange_due(&self, shopify_app: &str, shop: &str, token: &str) -> bool {
        let mut entries = self.entries();
        let Some(entry) = entries.get_mut(&app_shop(shopify_app, shop)) else {
            return false;
        };
        let due = entry.granted.is_some_and(|(granted, _)| granted == digest(token)) && !entry.exchange_tried;
        entry.exchange_tried |= due;
        due
    }

    /// The client of `shop` that obtains its tokens with the client
    /// credentials of the Shopify app `shopify_app`, starting from the token
    /// of its row (`stored`): `None` when `grant` does not allow it, when a
    /// failure still holds requests back, or when the app has no credentials.
    ///
    /// Fails, without asking Shopify, when a token could not be stored
    /// encrypted (no usable `ENCRYPTION_KEY`).
    pub(crate) async fn client(
        &self,
        app: &OpenShop,
        shopify_app: &str,
        shop: &str,
        grant: &Grant,
        stored: Option<&Stored>,
    ) -> Result<Option<GrantClient>> {
        let Some(shop) = strict_shop_domain(shop) else {
            return Ok(None);
        };
        let grant = grant.for_shop(shopify_app, &shop);
        if self.held_back(shopify_app, &shop, &grant) || !self.may_ask(&grant, &shop, stored) {
            return Ok(None);
        }
        if let Err(error) = app.crypto().can_encrypt() {
            bail!("[openshop] Cannot store a client credentials token for {shop}: {error}");
        }
        let resolved = app.apps().by_handle(Some(shopify_app))?;
        if resolved.api_key.is_empty() || resolved.api_secret.is_empty() {
            return Ok(None);
        }

        let current = stored
            .filter(|stored| !stored.uninstalled)
            .and_then(|stored| stored.token.clone());
        let key = (shopify_app.to_string(), shop.clone(), grant.is_vouched());
        let mut clients = locked(&self.clients);
        // A row given another token since (OAuth, the token exchange, another
        // process) makes a new client, which starts from it.
        if let Some(shared) = clients.get(&key)
            && *locked(&shared.token) == current
        {
            return Ok(Some(shared.clone()));
        }

        let mut auth = Auth::client_credentials(&resolved.api_key, &resolved.api_secret);
        if let Some(token) = &current {
            let mut start = TokenData::new(token);
            // When this process obtained it, the client renews it before it expires.
            start.expires_at = self
                .entries()
                .get(&app_shop(shopify_app, &shop))
                .and_then(|entry| entry.granted)
                .filter(|(granted, _)| *granted == digest(token))
                .map(|(_, expires_at)| expires_at);
            auth = auth.with_token(start);
        }
        let token: HeldToken = Arc::new(StdMutex::new(current));
        let store = InstallationStore {
            db: app.db().clone(),
            crypto: app.crypto().clone(),
            entries: self.entries.clone(),
            clients: Arc::downgrade(&self.clients),
            key: key.clone(),
            held: token.clone(),
            shopify_app: shopify_app.to_string(),
            grant: grant.clone(),
            configured: self.configured.as_deref() == Some(shop.as_str()),
            seen: Mutex::new(Seen {
                read: stored.is_some(),
                ciphertext: stored.and_then(|stored| stored.ciphertext.clone()),
                uninstalled: stored.is_some_and(|stored| stored.uninstalled),
            }),
        };
        let client = app
            .apps()
            .oauth(resolved)
            .builder(&ShopDomain::new(&shop)?)
            .auth(auth)
            .token_store(store)
            .build()?;
        let shared = GrantClient { client, token };
        clients.insert(key, shared.clone());
        Ok(Some(shared))
    }

    /// Forgets the shared clients of an app and shop: its row was given
    /// another token, or the shop was uninstalled.
    pub(crate) fn forget(&self, shopify_app: &str, shop: &str) {
        locked(&self.clients).retain(|(app, known, _), _| !(app == shopify_app && known == shop));
    }

    /// Remembers that the grant of `shop` failed for a caller of `grant`'s
    /// kind with `error`, when `error` is a failure to obtain or store a
    /// token: whether it was. The shared client is forgotten, and the caller
    /// does what it did without it. A row found changed holds nothing back.
    pub(crate) fn failed(
        &self,
        shopify_app: &str,
        shop: &str,
        grant: &Grant,
        client: &GrantClient,
        error: &shopify_api::Error,
    ) -> bool {
        let Some(definite) = grant_failure(error) else {
            return false;
        };
        let Some(shop) = strict_shop_domain(shop) else {
            return false;
        };
        let grant = grant.for_shop(shopify_app, &shop);
        let key = (shopify_app.to_string(), shop.clone(), grant.is_vouched());
        if superseded(error) {
            evict(&self.clients, &key, &client.token);
            return true;
        }
        let message = if definite {
            format!("[openshop] Shopify refused the client credentials of app \"{shopify_app}\" for {shop}: {error}")
        } else {
            format!("[openshop] The client credentials of app \"{shopify_app}\" for {shop} failed: {error}")
        };
        if definite && grant.is_vouched() {
            logger::info(&message);
        } else {
            logger::warn(&message);
        }
        let held_until = Instant::now() + if definite { REFUSAL_MEMORY } else { HOLD_BACK };
        self.remember(shopify_app, &shop, |entry| entry.hold_back(&grant, held_until));
        evict(&self.clients, &key, &client.token);
        true
    }

    /// A token of `shop` for the Shopify app `shopify_app`: the stored one
    /// when it is usable, else one obtained now with the client credentials
    /// grant, when `grant` allows it. `None` when Shopify gives none, or
    /// does not answer within [`TIMEOUT`]: the request itself goes on, and
    /// its token is stored.
    ///
    /// Fails when the token could not be stored encrypted (no usable
    /// `ENCRYPTION_KEY`), or written, with a message that names neither the
    /// token nor its ciphertext; that holds back as a failure that may pass.
    pub async fn grant(&self, app: &OpenShop, shopify_app: &str, shop: &str, grant: &Grant) -> Result<Option<String>> {
        let Some(shop) = strict_shop_domain(shop) else {
            return Ok(None);
        };
        let stored = stored(app, shopify_app, &shop).await?;
        if let Some(token) = stored.as_ref().and_then(|stored| stored.usable(self, shopify_app, &shop)) {
            return Ok(Some(token.to_string()));
        }
        let Some(client) = self.client(app, shopify_app, &shop, grant, stored.as_ref()).await? else {
            return Ok(None);
        };
        match tokio::time::timeout(TIMEOUT, client.client.access_token()).await {
            Ok(Ok(token)) => Ok(Some(token)),
            Ok(Err(error)) => {
                self.failed(shopify_app, &shop, grant, &client, &error);
                if superseded(&error) {
                    // What the row holds now, if it is usable.
                    let now = self::stored(app, shopify_app, &shop).await?;
                    return Ok(now
                        .as_ref()
                        .and_then(|stored| stored.usable(self, shopify_app, &shop))
                        .map(str::to_string));
                }
                if matches!(error, shopify_api::Error::TokenStore(_)) {
                    bail!("{error}");
                }
                Ok(None)
            }
            Err(_) => {
                logger::warn(format!(
                    "[openshop] The client credentials request of app \"{shopify_app}\" for {shop} took more than {} s",
                    TIMEOUT.as_secs()
                ));
                Ok(None)
            }
        }
    }
}

/// Whether `error` is a failure to obtain or store a token: `Some(true)` for
/// a definite refusal of the token endpoint, a `4xx` other than `408` and
/// `429` with an error code, `Some(false)` for one that may pass.
fn grant_failure(error: &shopify_api::Error) -> Option<bool> {
    use shopify_api::Error;
    match error {
        Error::Auth { .. } => Some(
            error
                .token_endpoint_status()
                .is_some_and(|status| (400..500).contains(&status) && !matches!(status, 408 | 429))
                && error.oauth_code().is_some(),
        ),
        Error::Transport { url, .. } => url.ends_with("/admin/oauth/access_token").then_some(false),
        Error::TokenStore(_) => Some(false),
        _ => None,
    }
}

/// What a client's store read of the row, to write over it only if it
/// still holds that.
struct Seen {
    /// Whether there was a row.
    read: bool,
    ciphertext: Option<String>,
    uninstalled: bool,
}

/// Where a shared client stores the tokens it obtains: the installation row,
/// encrypted, where it still holds what was seen.
struct InstallationStore {
    db: PgPool,
    crypto: Crypto,
    entries: Entries,
    /// The registry of the client, which a store that fails leaves.
    clients: std::sync::Weak<StdMutex<HashMap<ClientKey, GrantClient>>>,
    key: ClientKey,
    /// The token the client holds, as far as the registry knows.
    held: HeldToken,
    shopify_app: String,
    grant: Grant,
    /// Whether the shop is the one of the `client_credentials_shop` setting.
    configured: bool,
    seen: Mutex<Seen>,
}

impl InstallationStore {
    /// Writes the token where the grant allows it: whether it was written.
    async fn write(&self, shop: &str, token: &TokenData, seen: &Seen) -> Result<Option<String>> {
        let unwritten = || {
            anyhow::anyhow!(
                "[openshop] Could not store the client credentials token of app \"{}\" for {shop}",
                self.shopify_app
            )
        };
        let encrypted = self.crypto.encrypt_string(&token.access_token).map_err(|_| unwritten())?;
        let written: Option<(uuid::Uuid,)> = match &self.grant {
            Grant::Vouched { .. } => sqlx::query_as(
                "INSERT INTO installations (app_handle, shop, access_token, scopes) VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (app_handle, shop) DO UPDATE SET \
                   access_token = EXCLUDED.access_token, \
                   scopes = COALESCE(EXCLUDED.scopes, installations.scopes), \
                   installed_at = CASE \
                     WHEN installations.uninstalled_at IS NOT NULL OR installations.access_token IS NULL THEN now() \
                     ELSE installations.installed_at END, \
                   uninstalled_at = NULL \
                 WHERE $5 AND installations.access_token IS NOT DISTINCT FROM $6 \
                   AND (installations.uninstalled_at IS NOT NULL) = $7 \
                 RETURNING id",
            ),
            Grant::Background if self.configured => sqlx::query_as(
                "INSERT INTO installations (app_handle, shop, access_token, scopes) VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (app_handle, shop) DO UPDATE SET \
                   access_token = EXCLUDED.access_token, \
                   scopes = COALESCE(EXCLUDED.scopes, installations.scopes) \
                 WHERE installations.uninstalled_at IS NULL \
                   AND $5 AND installations.access_token IS NOT DISTINCT FROM $6 AND NOT $7 \
                 RETURNING id",
            ),
            Grant::Background => sqlx::query_as(
                "UPDATE installations SET access_token = $3, scopes = COALESCE($4, scopes) \
                 WHERE app_handle = $1 AND shop = $2 AND access_token IS NOT NULL AND uninstalled_at IS NULL \
                   AND $5 AND access_token IS NOT DISTINCT FROM $6 AND NOT $7 \
                 RETURNING id",
            ),
        }
        .bind(&self.shopify_app)
        .bind(shop)
        .bind(&encrypted)
        .bind(token.scope.as_deref().filter(|scopes| !scopes.is_empty()))
        .bind(seen.read)
        .bind(seen.ciphertext.as_deref())
        .bind(seen.uninstalled)
        .fetch_optional(&self.db)
        .await
        .map_err(|_| unwritten())?;
        Ok(written.map(|_| encrypted))
    }
}

impl TokenStore for InstallationStore {
    fn save<'a>(
        &'a self,
        shop: &'a ShopDomain,
        token: &'a TokenData,
    ) -> BoxFuture<'a, std::result::Result<(), Box<dyn std::error::Error + Send + Sync>>> {
        Box::pin(async move {
            let shop = shop.as_str();
            let mut seen = self.seen.lock().await;
            let error: Box<dyn std::error::Error + Send + Sync> = match self.write(shop, token, &seen).await {
                Ok(Some(ciphertext)) => {
                    *seen = Seen {
                        read: true,
                        ciphertext: Some(ciphertext),
                        uninstalled: false,
                    };
                    *locked(&self.held) = Some(token.access_token.clone());
                    let granted = token.expires_at.map(|expires_at| (digest(&token.access_token), expires_at));
                    remember(&self.entries, &self.shopify_app, shop, |entry| {
                        *entry = Entry {
                            granted,
                            ..Entry::default()
                        };
                    });
                    logger::info(format!(
                        "[openshop] Client credentials token obtained for {shop} (app \"{}\")",
                        self.shopify_app
                    ));
                    return Ok(());
                }
                // Another token was stored in the meantime, or the shop
                // uninstalled: the row keeps it, and the caller starts again from it.
                Ok(None) => Box::new(Superseded(format!(
                    "[openshop] The installation of app \"{}\" for {shop} changed while Shopify was asked for a token",
                    self.shopify_app
                ))),
                Err(error) => error.to_string().into(),
            };
            // Once a save failed, `shopify_api` tries it again at each request
            // of the client, which no caller should get any more.
            if let Some(clients) = self.clients.upgrade() {
                evict(&clients, &self.key, &self.held);
            }
            Err(error)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_shop_of_background_work() {
        assert_eq!(ClientCredentials::new(None).unwrap().configured_shop(), None);
        let credentials = ClientCredentials::new(Some(" Dev-Store-1.MyShopify.com ")).unwrap();
        assert_eq!(credentials.configured_shop(), Some("dev-store-1.myshopify.com"));
        let bare = ClientCredentials::new(Some("dev-store-1")).unwrap();
        assert_eq!(bare.configured_shop(), Some("dev-store-1.myshopify.com"));

        for value in [
            "example.com",
            "dev-store.myshopify.com.evil.example",
            "not a shop",
            "https://dev-store-1.myshopify.com",
            "evil.example://dev-store-1.myshopify.com",
            "dev-store-1.myshopify.com:8443",
        ] {
            let error = ClientCredentials::new(Some(value)).err().unwrap().to_string();
            assert_eq!(
                error,
                "[openshop] client_credentials_shop must be a *.myshopify.com domain"
            );
        }
    }

    fn stored(token: Option<&str>, uninstalled: bool) -> Stored {
        Stored {
            token: token.map(str::to_string),
            ciphertext: token.map(str::to_string),
            uninstalled,
        }
    }

    #[test]
    fn background_work_asks_for_the_configured_shop_or_to_renew_a_stored_token() {
        let credentials = ClientCredentials::new(Some("dev-store-1.myshopify.com")).unwrap();
        let background = Grant::Background;
        let ask = |shop: &str, row: Option<&Stored>| credentials.may_ask(&background, shop, row);
        assert!(ask("dev-store-1.myshopify.com", None));
        assert!(!ask("dev-store-1.myshopify.com", Some(&stored(None, true))));
        assert!(!ask("dev-store-2.myshopify.com", None));
        assert!(!ask("Dev-Store-1.myshopify.com", None));
        assert!(ask("dev-store-2.myshopify.com", Some(&stored(Some("t"), false))));
        assert!(!ask("dev-store-2.myshopify.com", Some(&stored(Some("t"), true))));
        assert!(!ask("dev-store-2.myshopify.com", Some(&stored(None, false))));

        let off = ClientCredentials::new(None).unwrap();
        assert!(!off.may_ask(&background, "dev-store-1.myshopify.com", None));
        assert!(off.may_ask(&background, "dev-store-1.myshopify.com", Some(&stored(Some("t"), false))));
    }

    #[test]
    fn a_vouched_grant_serves_its_app_and_shop_and_is_a_background_grant_elsewhere() {
        let vouched = Grant::vouched("first", " Dev-Store-2.MyShopify.com ").unwrap();
        assert_eq!(
            vouched,
            Grant::Vouched {
                shopify_app: "first".to_string(),
                shop: "dev-store-2.myshopify.com".to_string(),
            }
        );
        assert_eq!(vouched.for_shop("first", "dev-store-2.myshopify.com"), vouched);
        assert_eq!(vouched.for_shop("second", "dev-store-2.myshopify.com"), Grant::Background);
        assert_eq!(vouched.for_shop("first", "dev-store-3.myshopify.com"), Grant::Background);
        assert_eq!(Grant::vouched("first", "evil.example://dev-store-2.myshopify.com"), None);
    }

    #[test]
    fn only_a_token_this_process_obtained_expires() {
        let credentials = ClientCredentials::new(None).unwrap();
        let shop = "dev-store-1.myshopify.com";
        let set = |expires_at: DateTime<Utc>| {
            credentials.remember("default", shop, |entry| entry.granted = Some((digest("granted"), expires_at)));
        };
        assert!(!credentials.expiring("default", shop, "granted"));

        set(Utc::now() + TimeDelta::minutes(6));
        assert!(!credentials.expiring("default", shop, "granted"));
        set(Utc::now() + TimeDelta::minutes(4));
        assert!(credentials.expiring("default", shop, "granted"));
        assert!(!credentials.expiring("default", shop, "oauth-token"));
        assert!(!credentials.expiring("other", shop, "granted"));
        set(Utc::now() - TimeDelta::minutes(1));
        assert!(!credentials.expiring("default", shop, "granted"));
        assert!(credentials.entries().is_empty());
    }

    #[test]
    fn a_configuration_error_is_reported_once_per_app() {
        let credentials = ClientCredentials::new(None).unwrap();
        assert!(credentials.first_report("default"));
        assert!(!credentials.first_report("default"));
        assert!(credentials.first_report("second"));
    }

    /// Answers every request with `status` and `body`.
    struct Answer(u16, serde_json::Value);

    impl shopify_api::transport::Transport for Answer {
        fn send(
            &self,
            _request: shopify_api::transport::HttpRequest,
        ) -> BoxFuture<'_, std::result::Result<shopify_api::transport::HttpResponse, shopify_api::transport::TransportError>>
        {
            let response = shopify_api::transport::HttpResponse::json(self.0, &self.1);
            Box::pin(async move { Ok(response) })
        }
    }

    /// The error of a client credentials request that Shopify answers so.
    async fn grant_error(status: u16, body: serde_json::Value) -> shopify_api::Error {
        let client = Shopify::builder("dev-store-1")
            .transport(Answer(status, body))
            .client_credentials("key", "secret")
            .build()
            .unwrap();
        client.access_token().await.unwrap_err()
    }

    #[tokio::test]
    async fn only_a_refusal_with_a_code_is_definite() {
        let code = |code: &str| serde_json::json!({ "error": code });
        for (status, body) in [
            (400, code("shop_not_permitted")),
            (401, code("invalid_client")),
            (400, code("invalid_request")),
            (403, code("made_up_code")),
        ] {
            let error = grant_error(status, body.clone()).await;
            assert_eq!(grant_failure(&error), Some(true), "{status} {body}: {error}");
        }
        // A redirection is not followed: it is an answer without a token.
        for (status, body) in [
            (429, code("too_many_requests")),
            (408, code("timeout")),
            (500, code("internal_error")),
            (503, serde_json::json!({})),
            (400, serde_json::json!({})),
            (307, serde_json::json!({})),
            (200, serde_json::json!({})),
        ] {
            let error = grant_error(status, body.clone()).await;
            assert_eq!(grant_failure(&error), Some(false), "{status} {body}: {error}");
        }
    }
}
