//! Admin API authentication: every `/api` request carries a Shopify session token.

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::http::header::AUTHORIZATION;
use axum::middleware::Next;
use axum::response::Response;
use serde_json::{Value, json};

use super::http::{header, json_error};
use super::jwt::{customer_id_from_jwt_sub, read_jwt_audience, verify_session_token};
use super::shopify_apps::ResolvedShopifyApp;
use crate::admin::AdminActor;
use crate::app::OpenShop;
use crate::runtime::{OutboundRequest, logger, report};
use crate::shopify::client::ShopifyClient;
use crate::shopify::client_credentials::Grant;

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

    let Some(audience) = read_jwt_audience(&session_token) else {
        return json_error(StatusCode::UNAUTHORIZED, "Unauthorized: missing token audience");
    };
    let verified = app.apps().by_api_key(&audience).and_then(|shopify_app| {
        let verified = verify_session_token(&session_token, &shopify_app.api_secret, &shopify_app.api_key)?;
        Ok((shopify_app.clone(), verified))
    });
    let (shopify_app, verified) = match verified {
        Ok(verified) => verified,
        Err(error) => return json_error(StatusCode::UNAUTHORIZED, format!("Unauthorized: {error}")),
    };

    // Checkout and customer account extensions get tokens signed with the same secret,
    // for a customer or an anonymous buyer. The Admin API is for staff sessions only.
    let subject = &verified.payload.sub;
    if subject.is_empty() || customer_id_from_jwt_sub(subject).is_some() {
        return json_error(
            StatusCode::UNAUTHORIZED,
            "Unauthorized: session token was not issued to a staff user",
        );
    }

    report::describe(|request| {
        request.shop(&verified.shop, &shopify_app.handle);
    });
    // Installation is best-effort: a failed token exchange must not block the request.
    let _ = ensure_installation(&app, &shopify_app, &verified.shop, &session_token).await;

    request.extensions_mut().insert(ShopSession {
        shop: verified.shop,
        shopify_app: shopify_app.handle,
        actor: AdminActor {
            id: verified.payload.sub,
            session_id: verified.payload.sid,
        },
    });
    next.run(request).await
}

/// Gives the shop an offline access token when it has none yet, by the
/// token exchange. When Shopify refuses it, or it cannot run, Shopify still
/// vouched for the shop with the session token: a development store of the
/// organization that owns the app gets a token with the client credentials
/// grant. A token this process obtained with the grant, at a launch for
/// example, is replaced by the exchange at the first session after it, and
/// before it expires.
async fn ensure_installation(
    app: &OpenShop,
    shopify_app: &ResolvedShopifyApp,
    shop: &str,
    session_token: &str,
) -> anyhow::Result<()> {
    let existing: Option<(Option<String>,)> =
        sqlx::query_as("SELECT access_token FROM installations WHERE app_handle = $1 AND shop = $2 LIMIT 1")
            .bind(&shopify_app.handle)
            .bind(shop)
            .fetch_optional(app.db())
            .await?;
    if let Some(stored) = existing.and_then(|(token,)| token).filter(|token| !token.is_empty()) {
        let expiring = app
            .crypto()
            .decrypt_string(Some(&stored))
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

    if matches!(exchange_token(app, shopify_app, shop, session_token).await, Ok(true)) {
        return Ok(());
    }
    let Some(grant) = Grant::vouched(&shopify_app.handle, shop) else {
        return Ok(());
    };
    let credentials = app.client_credentials();
    match credentials.grant(app, &shopify_app.handle, shop, &grant, None).await {
        // The exchange was just refused: the next session does not try it again for this token.
        Ok(Some(token)) => {
            credentials.exchange_due(&shopify_app.handle, shop, &token);
        }
        Ok(None) => {}
        Err(error) => logger::warn(format!("{error:#}")),
    }
    Ok(())
}

/// Exchanges the session token for an offline access token and stores it:
/// whether Shopify gave one.
async fn exchange_token(
    app: &OpenShop,
    shopify_app: &ResolvedShopifyApp,
    shop: &str,
    session_token: &str,
) -> anyhow::Result<bool> {
    let response = app
        .transport()
        .post_json(OutboundRequest {
            url: format!("https://{shop}/admin/oauth/access_token"),
            headers: vec![("Content-Type".to_string(), "application/json".to_string())],
            body: json!({
                "client_id": shopify_app.api_key,
                "client_secret": shopify_app.api_secret,
                "grant_type": "urn:ietf:params:oauth:grant-type:token-exchange",
                "subject_token": session_token,
                "subject_token_type": "urn:ietf:params:oauth:token-type:id_token",
                "requested_token_type": "urn:shopify:params:oauth:token-type:offline-access-token",
            }),
        })
        .await?;
    if !response.ok() {
        return Ok(false);
    }

    let data = response.json()?;
    let Some(access_token) = data.get("access_token").and_then(Value::as_str) else {
        return Ok(false);
    };
    let access_token = app.crypto().encrypt_string(access_token)?;
    let scopes = data.get("scope").and_then(Value::as_str);

    sqlx::query(
        "INSERT INTO installations (app_handle, shop, access_token, scopes) VALUES ($1, $2, $3, $4) \
         ON CONFLICT (app_handle, shop) DO UPDATE SET \
           access_token = EXCLUDED.access_token, scopes = EXCLUDED.scopes, uninstalled_at = NULL",
    )
    .bind(&shopify_app.handle)
    .bind(shop)
    .bind(access_token)
    .bind(scopes)
    .execute(app.db())
    .await?;

    logger::info(format!("[openshop] Token exchange successful for {shop}"));
    Ok(true)
}
