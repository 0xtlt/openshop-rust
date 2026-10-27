use std::panic::AssertUnwindSafe;
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post, put};
use futures::FutureExt;
use serde_json::{Value, json};

use crate::app::OpenShop;
use crate::db::schema::ProviderConfigRow;
use crate::runtime::time::to_iso;
use crate::server::http::{ApiResult, client_error, public_error, json_error, json_object_body, json_response};
use crate::server::provider_config::{
    parse_provider_config, provider_config_is_complete, provider_fields_for_response, public_provider_config,
};
use crate::server::shop::ShopSession;

async fn stored_config(
    app: &OpenShop,
    session: &ShopSession,
    provider_name: &str,
) -> Result<Option<ProviderConfigRow>, sqlx::Error> {
    sqlx::query_as("SELECT * FROM provider_configs WHERE app_handle = $1 AND shop = $2 AND provider_name = $3 LIMIT 1")
        .bind(&session.shopify_app)
        .bind(&session.shop)
        .bind(provider_name)
        .fetch_optional(app.db())
        .await
}

async fn list_providers(State(app): State<OpenShop>, Extension(session): Extension<ShopSession>) -> ApiResult {
    let stored_rows: Vec<ProviderConfigRow> =
        sqlx::query_as("SELECT * FROM provider_configs WHERE app_handle = $1 AND shop = $2")
            .bind(&session.shopify_app)
            .bind(&session.shop)
            .fetch_all(app.db())
            .await?;

    let mut providers = Vec::new();
    for (name, provider) in &app.config().providers {
        let stored = stored_rows.iter().find(|row| row.provider_name == *name);
        let config = app
            .crypto()
            .decrypt_config(stored.and_then(|row| row.config.as_ref()))?;

        providers.push(json!({
            "name": name,
            "fields": provider_fields_for_response(provider, &config),
            "config": public_provider_config(provider, &config),
            "configured": provider_config_is_complete(provider, &config),
            "lastCheckedAt": stored.and_then(|row| row.last_checked_at.as_ref().map(to_iso)),
            "lastCheckOk": stored.and_then(|row| row.last_check_ok),
        }));
    }
    Ok(json_response(StatusCode::OK, Value::Array(providers)))
}

async fn save_provider(
    State(app): State<OpenShop>,
    Extension(session): Extension<ShopSession>,
    Path(name): Path<String>,
    body: Bytes,
) -> ApiResult {
    let Some(provider) = app.config().providers.get(&name) else {
        return Ok(json_error(StatusCode::NOT_FOUND, "Provider not found"));
    };
    let body = json_object_body(&body);

    let existing = stored_config(&app, &session, &name).await?;
    let existing_config = app
        .crypto()
        .decrypt_config(existing.as_ref().and_then(|row| row.config.as_ref()))?;
    let parsed = match parse_provider_config(
        &provider.fields,
        provider.transformer.as_ref(),
        body.get("config"),
        &existing_config,
    ) {
        Ok(parsed) => parsed,
        Err(error) => return Ok(json_error(StatusCode::BAD_REQUEST, error)),
    };

    let encrypted = app.crypto().encrypt_config(&parsed)?;
    sqlx::query(
        "INSERT INTO provider_configs (app_handle, shop, provider_name, config) VALUES ($1, $2, $3, $4::json) \
         ON CONFLICT (app_handle, shop, provider_name) DO UPDATE SET config = EXCLUDED.config, updated_at = now()",
    )
    .bind(&session.shopify_app)
    .bind(&session.shop)
    .bind(&name)
    .bind(encrypted.to_string())
    .execute(app.db())
    .await?;

    Ok(json_response(StatusCode::OK, json!({ "ok": true })))
}

async fn check_provider(
    State(app): State<OpenShop>,
    Extension(session): Extension<ShopSession>,
    Path(name): Path<String>,
) -> ApiResult {
    let Some(provider) = app.config().providers.get(&name) else {
        return Ok(json_error(StatusCode::NOT_FOUND, "Provider not found"));
    };
    let Some(checker) = &provider.checker else {
        return Ok(json_error(StatusCode::BAD_REQUEST, "No checker defined"));
    };
    let stored = stored_config(&app, &session, &name).await?;

    let check = async {
        let config = app
            .crypto()
            .decrypt_config(stored.as_ref().and_then(|row| row.config.as_ref()))?;
        let ok = AssertUnwindSafe(checker(config))
            .catch_unwind()
            .await
            .unwrap_or_else(|_| Err(public_error("checker panicked")))?;

        if let Some(stored) = &stored {
            sqlx::query("UPDATE provider_configs SET last_checked_at = now(), last_check_ok = $1 WHERE id = $2")
                .bind(ok)
                .bind(stored.id)
                .execute(app.db())
                .await?;
        }
        anyhow::Ok(ok)
    };

    Ok(match tokio::time::timeout(CHECK_TIMEOUT, check).await {
        Ok(Ok(ok)) => json_response(StatusCode::OK, json!({ "ok": ok })),
        Err(_) => json_response(
            StatusCode::GATEWAY_TIMEOUT,
            json!({
                "ok": false,
                "error": format!("Error: the check timed out after {} seconds", CHECK_TIMEOUT.as_secs()),
            }),
        ),
        Ok(Err(error)) => {
            let failure = client_error(&format!("Check of provider \"{name}\" failed"), &error);
            let mut body = json!({ "ok": false, "error": format!("Error: {}", failure.message) });
            if let Some(id) = failure.error_id {
                body["errorId"] = json!(id);
            }
            json_response(StatusCode::INTERNAL_SERVER_ERROR, body)
        }
    })
}

/// The longest a connection check may take: the admin waits for the answer.
const CHECK_TIMEOUT: Duration = Duration::from_secs(10);

pub fn routes() -> Router<OpenShop> {
    Router::new()
        .route("/api/providers", get(list_providers))
        .route("/api/providers/{name}", put(save_provider))
        .route("/api/providers/{name}/check", post(check_provider))
}
