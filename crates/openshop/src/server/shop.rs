//! Admin API authentication: every `/api` request carries a Shopify session token.

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::http::header::AUTHORIZATION;
use axum::middleware::Next;
use axum::response::Response;
use serde_json::json;
use shopify_api::TokenData;
use shopify_api::oauth::{RequestedToken, SessionToken};

use super::http::{header, json_error};
use super::jwt::{is_staff_subject, read_jwt_audience};
use super::shopify_apps::ResolvedShopifyApp;
use crate::admin::AdminActor;
use crate::app::OpenShop;
use crate::runtime::{logger, report};
use crate::shopify::client::ShopifyClient;
use crate::shopify::client_credentials::Grant;
use crate::shopify::token_exchange::TIMEOUT;

/// The authenticated shop of an Admin API request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShopSession {
    pub shop: String,
    /// Handle of the Shopify app the session token was issued for.
    pub shopify_app: String,
    pub actor: AdminActor,
}

impl ShopSession {
    /// The Admin API of the shop of the session. Shopify vouched for that shop
    /// and app with the session token: a shop without a token, or a token the
    /// Admin API refuses, gets one with the client credentials grant. For any
    /// other shop or app, the client may do what background work may.
    pub(crate) fn shopify_client(&self, app: &OpenShop) -> ShopifyClient {
        let grant = Grant::vouched(&self.shopify_app, &self.shop).unwrap_or(Grant::Background);
        app.shopify_client(&self.shop, &self.shopify_app).granting(Some(grant))
    }
}

pub(crate) async fn shop_middleware(State(app): State<OpenShop>, mut request: Request, next: Next) -> Response {
    let Some(session_token) = header(request.headers(), AUTHORIZATION.as_str())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::to_string)
    else {
        return json_error(StatusCode::UNAUTHORIZED, "Unauthorized: missing session token");
    };
    match session_of(&app, &session_token, request.uri().path()).await {
        Ok(session) => {
            request.extensions_mut().insert(session);
            next.run(request).await
        }
        Err(reason) => json_error(StatusCode::UNAUTHORIZED, reason),
    }
}

/// The session of a staff member a session token vouches for, the shop
/// installed first when it had no token (best effort). The reason it is
/// refused otherwise, for the `401` that says so; why a token fails (which
/// claim, which app) is logged, not given to whoever sent it.
pub(crate) async fn session_of(app: &OpenShop, session_token: &str, path: &str) -> Result<ShopSession, &'static str> {
    let Some(audience) = read_jwt_audience(session_token) else {
        return Err("Unauthorized: missing token audience");
    };
    let verified = app.apps().by_api_key(&audience).and_then(|shopify_app| {
        let verified = shopify_app.oauth.verify_session_token(session_token)?;
        Ok((shopify_app.clone(), verified))
    });
    let (shopify_app, verified) = match verified {
        Ok(verified) => verified,
        Err(error) => {
            logger::warn_with(
                "[openshop] Session token refused",
                json!({ "reason": error.to_string(), "path": path }),
            );
            return Err("Unauthorized: invalid session token");
        }
    };

    // Checkout and customer account extensions get tokens signed with the same secret,
    // for a customer or an anonymous buyer. The Admin API is for staff sessions only.
    let staff = verified.user_id().unwrap_or_default().to_string();
    if !is_staff_subject(&staff) {
        return Err("Unauthorized: session token was not issued to a staff user");
    }

    let shop = verified.shop().to_string();
    report::describe(|request| {
        request.shop(&shop, &shopify_app.handle);
    });
    // Installation is best-effort: a failed token exchange must not block the request.
    let _ = ensure_installation(app, &shopify_app, &shop, &verified).await;

    Ok(ShopSession {
        shop,
        shopify_app: shopify_app.handle,
        actor: AdminActor {
            id: staff,
            session_id: verified.session_id().unwrap_or_default().to_string(),
        },
    })
}

/// Gives the shop an offline access token when it has none yet, by the
/// token exchange. When Shopify refuses it, or it cannot run, Shopify still
/// vouched for the shop with the session token: a development store of the
/// organization that owns the app gets a token with the client credentials
/// grant. A token this process obtained with the grant, at a launch for
/// example, is replaced by the exchange at the first session after it, and
/// before it expires.
///
/// One request at a time runs the exchange of a shop, for five seconds at
/// most; the others wait for it and use its token. A failed exchange is not
/// tried again for thirty seconds.
pub(crate) async fn ensure_installation(
    app: &OpenShop,
    shopify_app: &ResolvedShopifyApp,
    shop: &str,
    session: &SessionToken,
) -> anyhow::Result<()> {
    let existing = stored_token(app, shopify_app, shop).await?;
    if let Some(stored) = existing.as_ref().filter(|token| !token.is_empty()) {
        let expiring = app
            .crypto()
            .decrypt_string(Some(stored))
            .ok()
            .flatten()
            .is_some_and(|token| {
                let credentials = app.client_credentials();
                credentials.expiring(&shopify_app.handle, shop, &token)
                    || credentials.exchange_due(&shopify_app.handle, shop, &token)
            });
        if !expiring {
            return Ok(());
        }
    }
    if shopify_app.api_key.is_empty() || shopify_app.api_secret.is_empty() {
        return Ok(());
    }

    match exchange_once(app, shopify_app, shop, session, &existing).await {
        Ok(Exchange::Done) => return Ok(()),
        // Shopify does not answer: the grant would wait for it as well.
        Ok(Exchange::TimedOut) => return Ok(()),
        Ok(Exchange::Failed) | Err(_) => {}
    }
    let Some(grant) = Grant::vouched(&shopify_app.handle, shop) else {
        return Ok(());
    };
    let credentials = app.client_credentials();
    match credentials.grant(app, &shopify_app.handle, shop, &grant).await {
        // The exchange was just refused: the next session does not try it again for this token.
        Ok(Some(token)) => {
            credentials.exchange_due(&shopify_app.handle, shop, &token);
        }
        Ok(None) => {}
        Err(error) => logger::warn(format!("{error:#}")),
    }
    Ok(())
}

/// Stores the offline access token Shopify gave a shop, and drops the token
/// of the client credentials grant held for it. An installation by OAuth
/// (`reinstall`) starts again: its nonce is spent and `installed_at` is now.
pub(crate) async fn store_access_token(
    app: &OpenShop,
    shopify_app: &str,
    shop: &str,
    token: &TokenData,
    reinstall: bool,
) -> anyhow::Result<()> {
    let restart = if reinstall {
        ", nonce = NULL, installed_at = now()"
    } else {
        ""
    };
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO installations (app_handle, shop, access_token, scopes) VALUES ($1, $2, $3, $4) \
         ON CONFLICT (app_handle, shop) DO UPDATE SET \
           access_token = EXCLUDED.access_token, scopes = EXCLUDED.scopes, uninstalled_at = NULL{restart}"
    )))
    .bind(shopify_app)
    .bind(shop)
    .bind(app.crypto().encrypt_string(&token.access_token)?)
    .bind(token.scope.as_deref())
    .execute(app.db())
    .await?;
    app.client_credentials().forget(shopify_app, shop);
    Ok(())
}

/// The access token of the row as it is stored, if the row has one.
async fn stored_token(app: &OpenShop, shopify_app: &ResolvedShopifyApp, shop: &str) -> anyhow::Result<Option<String>> {
    let row: Option<(Option<String>,)> =
        sqlx::query_as("SELECT access_token FROM installations WHERE app_handle = $1 AND shop = $2 LIMIT 1")
            .bind(&shopify_app.handle)
            .bind(shop)
            .fetch_optional(app.db())
            .await?;
    Ok(row.and_then(|(token,)| token))
}

/// How the exchange of a request ended.
enum Exchange {
    /// Shopify gave a token that is stored, or another request stored one
    /// while this one waited for its turn.
    Done,
    /// No token: refused, failed, or held back by an earlier failure.
    Failed,
    /// Shopify did not answer in time.
    TimedOut,
}

/// The exchange of the shop, once for the requests that arrive together.
/// `seen` is the token the row held when the request looked.
async fn exchange_once(
    app: &OpenShop,
    shopify_app: &ResolvedShopifyApp,
    shop: &str,
    session: &SessionToken,
    seen: &Option<String>,
) -> anyhow::Result<Exchange> {
    let exchanges = &app.client_credentials().exchanges;
    if exchanges.held_back(&shopify_app.handle, shop) {
        return Ok(Exchange::Failed);
    }
    let _turn = exchanges.turn(&shopify_app.handle, shop).await;
    // What the request that had the turn did is what this one needs.
    if exchanges.held_back(&shopify_app.handle, shop) {
        return Ok(Exchange::Failed);
    }
    if stored_token(app, shopify_app, shop).await? != *seen {
        return Ok(Exchange::Done);
    }

    let exchanged = tokio::time::timeout(TIMEOUT, exchange_token(app, shopify_app, shop, session)).await;
    let outcome = match exchanged {
        Ok(Ok(true)) => return Ok(Exchange::Done),
        Ok(Ok(false)) => Exchange::Failed,
        Ok(Err(error)) => {
            logger::warn(format!("{error:#}"));
            Exchange::Failed
        }
        Err(_) => {
            logger::warn(format!(
                "[openshop] The token exchange of app \"{}\" for {shop} timed out after {} s",
                shopify_app.handle,
                TIMEOUT.as_secs()
            ));
            Exchange::TimedOut
        }
    };
    exchanges.hold_back(&shopify_app.handle, shop);
    Ok(outcome)
}

/// Exchanges the session token for an offline access token and stores it:
/// whether Shopify gave one.
async fn exchange_token(
    app: &OpenShop,
    shopify_app: &ResolvedShopifyApp,
    shop: &str,
    session: &SessionToken,
) -> anyhow::Result<bool> {
    let token = match shopify_app
        .oauth
        .exchange_session_token(session, RequestedToken::Offline)
        .await
    {
        Ok(token) => token,
        Err(shopify_api::Error::Auth { .. }) => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    store_access_token(app, &shopify_app.handle, shop, &token, false).await?;

    logger::info(format!("[openshop] Token exchange successful for {shop}"));
    Ok(true)
}
