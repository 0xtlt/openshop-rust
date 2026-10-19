use axum::Router;
use axum::body::Bytes;
use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use serde_json::{Value, json};

use crate::app::OpenShop;
use crate::server::http::{ApiResult, json_error, json_object_body, json_response};
use crate::server::mcp::core::{cron_items, is_cron_key, set_cron_enabled};
use crate::server::shop::ShopSession;

async fn list_crons(State(app): State<OpenShop>, Extension(session): Extension<ShopSession>) -> ApiResult {
    let crons = cron_items(&app, &session.shopify_app, &session.shop, false).await?;
    Ok(json_response(StatusCode::OK, Value::Array(crons)))
}

async fn toggle_cron(
    State(app): State<OpenShop>,
    Extension(session): Extension<ShopSession>,
    body: Bytes,
) -> ApiResult {
    let body = json_object_body(&body);
    let (Some(key), Some(enabled)) = (
        body.get("key").and_then(Value::as_str).filter(|key| !key.is_empty()),
        body.get("enabled").and_then(Value::as_bool),
    ) else {
        return Ok(json_error(StatusCode::BAD_REQUEST, "key and enabled are required"));
    };

    if !is_cron_key(&app, key) {
        return Ok(json_error(StatusCode::NOT_FOUND, "Cron not found"));
    }
    set_cron_enabled(&app, &session.shopify_app, &session.shop, key, enabled).await?;
    Ok(json_response(
        StatusCode::OK,
        json!({ "ok": true, "key": key, "enabled": enabled }),
    ))
}

pub fn routes() -> Router<OpenShop> {
    Router::new()
        .route("/api/crons", get(list_crons))
        .route("/api/crons/toggle", post(toggle_cron))
}
