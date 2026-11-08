//! Shopify OAuth: authorization redirect, callback and the uninstall webhook.

use std::collections::BTreeMap;
use std::sync::Mutex;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{RawQuery, State};
use axum::http::header::{COOKIE as COOKIE_HEADER, LOCATION, SET_COOKIE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use chrono::Utc;
use rand::Rng;
use serde_json::json;
use shopify_api::ShopDomain;
use shopify_api::oauth::AuthorizationRequest;

use super::hmac::{constant_time_eq, hmac_sha256};
use super::http::{ApiResult, json_error, json_response, parse_query};
use super::shop::store_access_token;
use super::shop_domain::normalize_shop_domain;
use crate::app::OpenShop;

/// A 302 redirect.
pub(crate) fn redirect(location: &str) -> Response {
    (StatusCode::FOUND, [(LOCATION, location.to_string())]).into_response()
}

/// How long a started installation may take to come back to the callback.
const STATE_TTL: i64 = 10 * 60;

/// The cookie that binds an installation to the browser that started it: a
/// random identifier of the browser, which the state is signed with.
const COOKIE: &str = "openshop_oauth";

/// The states spent by this process, by signature, until they would expire
/// anyway. Only a callback signed by Shopify with a valid state adds to it.
static SPENT: Mutex<BTreeMap<String, i64>> = Mutex::new(BTreeMap::new());

/// The signature of a state. The state of an authorization, `<expiry>.<nonce>.<signature>`,
/// carries the pending installation, so that `/auth`, which anybody can call,
/// writes nothing: it is signed with the secret of the app over the handle of
/// the app, the shop, the expiry, the nonce and the browser. Any number of
/// installations of a shop may be pending, and a state is good for the browser
/// that started it only.
fn sign_state(secret: &str, app_handle: &str, shop: &str, expiry: i64, nonce: &str, browser: &str) -> String {
    let message = format!("openshop-oauth-state\0{app_handle}\0{shop}\0{expiry}\0{nonce}\0{browser}");
    hex::encode(hmac_sha256(secret, message.as_bytes()))
}

fn random_hex() -> String {
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// The identifier of the browser in the `Cookie` headers, if there is a
/// well-formed one.
fn browser_id(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(COOKIE_HEADER)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == COOKIE)
        .map(|(_, value)| value.to_string())
        .filter(|value| value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

fn cookie(value: &str, max_age: i64, secure: bool) -> String {
    let secure = if secure { "; Secure" } else { "" };
    format!("{COOKIE}={value}; Path=/auth; Max-Age={max_age}; HttpOnly; SameSite=Lax{secure}")
}

/// Whether `state` is a live state of this app and shop for the browser of
/// `headers`; the key to spend it by.
fn verify_state(secret: &str, app_handle: &str, shop: &str, state: &str, headers: &HeaderMap) -> Option<(String, i64)> {
    let mut parts = state.split('.');
    let (Some(expiry), Some(nonce), Some(signature), None) = (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    let browser = browser_id(headers)?;
    let expiry: i64 = expiry.parse().ok()?;
    let expected = sign_state(secret, app_handle, shop, expiry, nonce, &browser);
    if !constant_time_eq(expected.as_bytes(), signature.as_bytes()) || expiry < Utc::now().timestamp() {
        return None;
    }
    Some((signature.to_string(), expiry))
}

/// Spends a state: false if it was already.
fn spend(key: String, expiry: i64) -> bool {
    let mut spent = SPENT.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    spent.retain(|_, expiry| *expiry >= Utc::now().timestamp());
    spent.insert(key, expiry).is_none()
}

async fn start(State(app): State<OpenShop>, headers: HeaderMap, RawQuery(raw): RawQuery) -> ApiResult {
    let query = parse_query(raw.as_deref());
    let Some(raw_shop) = query.get("shop").filter(|shop| !shop.is_empty()) else {
        return Ok(json_error(StatusCode::BAD_REQUEST, "Missing shop parameter"));
    };
    let Some(shop) = normalize_shop_domain(raw_shop).and_then(|shop| ShopDomain::new(&shop).ok()) else {
        return Ok(json_error(StatusCode::BAD_REQUEST, "Invalid shop parameter"));
    };
    let shopify_app = match app.apps().by_handle(query.get("app").map(String::as_str)) {
        Ok(shopify_app) => shopify_app,
        Err(error) => return Ok(json_error(StatusCode::BAD_REQUEST, error.to_string())),
    };
    // A state signed with no secret would be one anybody can make.
    if shopify_app.api_secret.trim().is_empty() {
        return Ok(json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "[openshop] The Shopify app has no API secret",
        ));
    }

    // The browser keeps its cookie while it has one: its other pending
    // installations stay valid.
    let browser = browser_id(&headers).unwrap_or_else(random_hex);
    let expiry = Utc::now().timestamp() + STATE_TTL;
    let nonce = random_hex();
    let signature = sign_state(&shopify_app.api_secret, &shopify_app.handle, shop.as_str(), expiry, &nonce, &browser);
    let state = format!("{expiry}.{nonce}.{signature}");

    let request = AuthorizationRequest::new(format!("{}/auth/callback", shopify_app.app_url), state)
        .scopes(shopify_app.scopes.split(',').map(str::trim).filter(|scope| !scope.is_empty()));
    let auth_url = shopify_app.oauth.authorization_url(&shop, &request);
    let mut response = redirect(&auth_url);
    let secure = shopify_app.app_url.starts_with("https://");
    if let Ok(value) = HeaderValue::from_str(&cookie(&browser, STATE_TTL, secure)) {
        response.headers_mut().append(SET_COOKIE, value);
    }
    Ok(response)
}

async fn callback(State(app): State<OpenShop>, headers: HeaderMap, RawQuery(raw): RawQuery) -> ApiResult {
    let query = parse_query(raw.as_deref());
    let param = |name: &str| query.get(name).filter(|value| !value.is_empty());
    let (Some(raw_shop), Some(_), Some(state)) = (param("shop"), param("code"), param("state")) else {
        return Ok(json_error(StatusCode::BAD_REQUEST, "Missing required parameters"));
    };
    let Some(shop) = normalize_shop_domain(raw_shop) else {
        return Ok(json_error(StatusCode::BAD_REQUEST, "Invalid shop parameter"));
    };
    let Ok((shopify_app, callback)) = app.apps().by_signed_query(raw.as_deref().unwrap_or_default()) else {
        return Ok(json_error(StatusCode::UNAUTHORIZED, "Invalid HMAC"));
    };

    let spent = verify_state(&shopify_app.api_secret, &shopify_app.handle, &shop, state, &headers)
        .is_some_and(|(key, expiry)| spend(key, expiry));
    if !spent {
        return Ok(json_error(StatusCode::UNAUTHORIZED, "Invalid state/nonce"));
    }

    let token = match shopify_app.oauth.exchange_code(&callback).await {
        Ok(token) => token,
        // What Shopify answered; a request that did not reach it is the server's error.
        Err(error @ shopify_api::Error::Auth { .. }) => {
            return Ok(json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Token exchange failed: {error}"),
            ));
        }
        Err(error) => return Err(error.into()),
    };

    store_access_token(&app, &shopify_app.handle, &shop, &token, true).await?;

    // The cookie stays: it only names the browser, and the other
    // installations it started may still be pending. Back to the app in the
    // admin of the shop: at the admin origin when there is one, which serves
    // every shop on one origin and takes the shop in the query.
    Ok(redirect(&match &app.endpoints().admin_origin {
        Some(origin) => format!("{origin}/admin/apps/{}?shop={shop}", shopify_app.api_key),
        None => format!("https://{shop}/admin/apps/{}", shopify_app.api_key),
    }))
}

async fn app_uninstalled(State(app): State<OpenShop>, headers: HeaderMap, body: Bytes) -> ApiResult {
    let Ok((shopify_app, webhook)) = app.apps().by_webhook(&headers, &body) else {
        return Ok(json_error(StatusCode::UNAUTHORIZED, "Invalid webhook HMAC"));
    };

    // The body is covered by the signature, the shop header is not: the shop
    // is the one both name.
    if webhook.verify_payload_shop().is_ok() {
        let shop_domain = webhook.shop().to_string();
        sqlx::query(
            "UPDATE installations SET uninstalled_at = now(), access_token = NULL WHERE app_handle = $1 AND shop = $2",
        )
        .bind(&shopify_app.handle)
        .bind(&shop_domain)
        .execute(app.db())
        .await?;
        app.client_credentials().forget(&shopify_app.handle, &shop_domain);
    }

    Ok(json_response(StatusCode::OK, json!({ "ok": true })))
}

pub fn create_auth_routes() -> Router<OpenShop> {
    Router::new()
        .route("/auth", get(start))
        .route("/auth/", get(start))
        .route("/auth/callback", get(callback))
        .route("/auth/webhooks/app-uninstalled", post(app_uninstalled))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spent_states_are_forgotten_once_they_would_have_expired() {
        let now = Utc::now().timestamp();
        assert!(spend("spent-test-old".into(), now - 1));
        assert!(spend("spent-test-live".into(), now + 60));
        assert!(!spend("spent-test-live".into(), now + 60));
        let spent = SPENT.lock().unwrap();
        assert!(!spent.contains_key("spent-test-old"));
        assert!(spent.contains_key("spent-test-live"));
    }
}
