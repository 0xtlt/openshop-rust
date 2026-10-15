//! Shopify OAuth: authorization redirect, callback and the uninstall webhook.

use axum::Router;
use axum::body::Bytes;
use axum::extract::{RawQuery, State};
use axum::http::header::LOCATION;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use rand::RngCore;
use serde_json::{Value, json};
use uuid::Uuid;

use super::http::{ApiResult, header, json_error, json_response, parse_query};
use super::shop_domain::normalize_shop_domain;
use crate::app::OpenShop;
use crate::runtime::OutboundRequest;
use openshop_shared::rpc::encode_uri_component;

/// A 302 redirect.
pub(crate) fn redirect(location: &str) -> Response {
    (StatusCode::FOUND, [(LOCATION, location.to_string())]).into_response()
}

async fn start(State(app): State<OpenShop>, RawQuery(raw): RawQuery) -> ApiResult {
    let query = parse_query(raw.as_deref());
    let Some(raw_shop) = query.get("shop").filter(|shop| !shop.is_empty()) else {
        return Ok(json_error(StatusCode::BAD_REQUEST, "Missing shop parameter"));
    };
    let Some(shop) = normalize_shop_domain(raw_shop) else {
        return Ok(json_error(StatusCode::BAD_REQUEST, "Invalid shop parameter"));
    };
    let shopify_app = match app.apps().by_handle(query.get("app").map(String::as_str)) {
        Ok(shopify_app) => shopify_app,
        Err(error) => return Ok(json_error(StatusCode::BAD_REQUEST, error.to_string())),
    };

    let mut nonce = [0u8; 16];
    rand::rng().fill_bytes(&mut nonce);
    let nonce = hex::encode(nonce);

    sqlx::query(
        "INSERT INTO installations (app_handle, shop, nonce) VALUES ($1, $2, $3) \
         ON CONFLICT (app_handle, shop) DO UPDATE SET nonce = EXCLUDED.nonce",
    )
    .bind(&shopify_app.handle)
    .bind(&shop)
    .bind(&nonce)
    .execute(app.db())
    .await?;

    let redirect_uri = format!("{}/auth/callback", shopify_app.app_url);
    let auth_url = format!(
        "https://{shop}/admin/oauth/authorize?client_id={}&scope={}&redirect_uri={}&state={nonce}",
        shopify_app.api_key,
        shopify_app.scopes,
        encode_uri_component(&redirect_uri),
    );
    Ok(redirect(&auth_url))
}

async fn callback(State(app): State<OpenShop>, RawQuery(raw): RawQuery) -> ApiResult {
    let query = parse_query(raw.as_deref());
    let param = |name: &str| query.get(name).filter(|value| !value.is_empty());
    let (Some(raw_shop), Some(code), Some(state)) = (param("shop"), param("code"), param("state")) else {
        return Ok(json_error(StatusCode::BAD_REQUEST, "Missing required parameters"));
    };
    let Some(shop) = normalize_shop_domain(raw_shop) else {
        return Ok(json_error(StatusCode::BAD_REQUEST, "Invalid shop parameter"));
    };
    let Ok(shopify_app) = app.apps().by_signed_query(&query) else {
        return Ok(json_error(StatusCode::UNAUTHORIZED, "Invalid HMAC"));
    };

    let installation: Option<(Uuid, Option<String>)> =
        sqlx::query_as("SELECT id, nonce FROM installations WHERE app_handle = $1 AND shop = $2 LIMIT 1")
            .bind(&shopify_app.handle)
            .bind(&shop)
            .fetch_optional(app.db())
            .await?;
    let Some((installation_id, _)) = installation.filter(|(_, nonce)| nonce.as_deref() == Some(state.as_str())) else {
        return Ok(json_error(StatusCode::UNAUTHORIZED, "Invalid state/nonce"));
    };

    let token_response = app
        .transport()
        .post_json(OutboundRequest {
            url: format!("https://{shop}/admin/oauth/access_token"),
            headers: vec![("Content-Type".to_string(), "application/json".to_string())],
            body: json!({
                "client_id": shopify_app.api_key,
                "client_secret": shopify_app.api_secret,
                "code": code,
            }),
        })
        .await?;
    if !token_response.ok() {
        return Ok(json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Token exchange failed: {}", token_response.body),
        ));
    }

    let token_data = token_response.json()?;
    let access_token = token_data
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("Token exchange response has no access_token"))?;

    sqlx::query(
        "UPDATE installations SET \
           access_token = $1, access_token_expires_at = NULL, scopes = $2, nonce = NULL, \
           installed_at = now(), uninstalled_at = NULL \
         WHERE id = $3",
    )
    .bind(app.crypto().encrypt_string(access_token)?)
    .bind(token_data.get("scope").and_then(Value::as_str))
    .bind(installation_id)
    .execute(app.db())
    .await?;

    Ok(redirect(&format!("https://{shop}/admin/apps/{}", shopify_app.api_key)))
}

async fn app_uninstalled(State(app): State<OpenShop>, headers: HeaderMap, body: Bytes) -> ApiResult {
    let body = String::from_utf8_lossy(&body);
    let hmac = header(&headers, "x-shopify-hmac-sha256").unwrap_or_default();
    let Ok(shopify_app) = app.apps().by_webhook_hmac(&body, hmac) else {
        return Ok(json_error(StatusCode::UNAUTHORIZED, "Invalid webhook HMAC"));
    };

    let data: Value = serde_json::from_str(&body)?;
    // The body is covered by the HMAC; the header is not, so it is only a fallback.
    let shop_domain = data
        .get("myshopify_domain")
        .and_then(Value::as_str)
        .or_else(|| header(&headers, "x-shopify-shop-domain"))
        .and_then(normalize_shop_domain);

    if let Some(shop_domain) = shop_domain {
        sqlx::query(
            "UPDATE installations SET uninstalled_at = now(), access_token = NULL, access_token_expires_at = NULL \
             WHERE app_handle = $1 AND shop = $2",
        )
        .bind(&shopify_app.handle)
        .bind(shop_domain)
        .execute(app.db())
        .await?;
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
