//! Admin API for MCP capabilities and tokens.

use std::collections::HashMap;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{get, post, put};
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};
use sqlx::{Postgres, QueryBuilder};
use uuid::Uuid;

use crate::app::OpenShop;
use crate::db::schema::{McpAuditLog, McpToken};
use crate::runtime::time::{parse_date, to_iso};
use crate::server::http::{ApiResult, json_error, json_object_body, json_response};
use crate::server::mcp::registry::McpRegistry;
use crate::server::mcp::tokens::{GeneratedMcpToken, create_mcp_token, create_mcp_token_for_id};
use crate::server::shop::ShopSession;
use crate::types::{JsonMap, McpRiskLevel};

const DEFAULT_EXPIRATION_DAYS: i64 = 90;
const DAY_MS: i64 = 86_400_000;

fn string_value<'a>(body: &'a JsonMap, key: &str) -> Option<&'a str> {
    body.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// `expiresAt` (date or null) wins over `expiresInDays` (positive integer or null).
/// Without either, tokens expire after 90 days.
fn expires_at_from_body(body: &JsonMap) -> Result<Option<DateTime<Utc>>, &'static str> {
    if let Some(expires_at) = body.get("expiresAt") {
        return match expires_at {
            Value::Null => Ok(None),
            Value::String(text) if !text.trim().is_empty() => parse_date(text)
                .map(Some)
                .ok_or("expiresAt must be a valid date string or null"),
            _ => Err("expiresAt must be a valid date string or null"),
        };
    }

    if let Some(expires_in_days) = body.get("expiresInDays") {
        let invalid = "expiresInDays must be a positive integer or null";
        return match expires_in_days {
            Value::Null => Ok(None),
            Value::Number(number) => match number.as_f64() {
                Some(days) if days.fract() == 0.0 && days > 0.0 && days < 1e7 => {
                    Ok(Some(Utc::now() + Duration::milliseconds(days as i64 * DAY_MS)))
                }
                _ => Err(invalid),
            },
            _ => Err(invalid),
        };
    }

    Ok(Some(Utc::now() + Duration::days(DEFAULT_EXPIRATION_DAYS)))
}

fn public_token(
    row: &McpToken,
    permissions: &[String],
    registry: &McpRegistry,
    recent_audits: &[McpAuditLog],
) -> Value {
    let stale: Vec<&String> = permissions
        .iter()
        .filter(|permission| !registry.permissions.contains_key(*permission))
        .collect();
    json!({
        "id": row.id,
        "tokenId": row.token_id,
        "name": row.name,
        "tokenFingerprint": row.token_fingerprint,
        "status": row.status,
        "expiresAt": row.expires_at.as_ref().map(to_iso),
        "revokedAt": row.revoked_at.as_ref().map(to_iso),
        "lastUsedAt": row.last_used_at.as_ref().map(to_iso),
        "createdAt": to_iso(&row.created_at),
        "updatedAt": to_iso(&row.updated_at),
        "permissions": permissions,
        "stalePermissions": stale,
        "recentAudits": recent_audits,
    })
}

async fn permissions_for_tokens(
    app: &OpenShop,
    session: &ShopSession,
    token_ids: &[String],
) -> Result<HashMap<String, Vec<String>>, sqlx::Error> {
    if token_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let grants: Vec<(String, String)> = sqlx::query_as(
        "SELECT token_id, permission_key FROM mcp_permission_grants \
         WHERE app_handle = $1 AND shop = $2 AND token_id = ANY($3) AND granted = true \
         ORDER BY created_at ASC, permission_key ASC",
    )
    .bind(&session.shopify_app)
    .bind(&session.shop)
    .bind(token_ids)
    .fetch_all(app.db())
    .await?;

    let mut by_token: HashMap<String, Vec<String>> = HashMap::new();
    for (token_id, permission_key) in grants {
        by_token.entry(token_id).or_default().push(permission_key);
    }
    Ok(by_token)
}

/// The token row when it belongs to the session's shop and app.
async fn find_token(app: &OpenShop, session: &ShopSession, id: &str) -> Result<Option<McpToken>, sqlx::Error> {
    let Ok(id) = Uuid::parse_str(id) else {
        return Ok(None);
    };
    sqlx::query_as("SELECT * FROM mcp_tokens WHERE id = $1 AND app_handle = $2 AND shop = $3 LIMIT 1")
        .bind(id)
        .bind(&session.shopify_app)
        .bind(&session.shop)
        .fetch_optional(app.db())
        .await
}

fn token_not_found() -> Response {
    json_error(StatusCode::NOT_FOUND, "Token not found")
}

fn is_revoked(token: &McpToken) -> bool {
    token.status == "revoked" || token.revoked_at.is_some()
}

/// Distinct string permissions from the body; the error names the first unknown one.
fn selected_permissions(body: &JsonMap, registry: &McpRegistry) -> Result<Vec<String>, String> {
    let mut permissions: Vec<String> = Vec::new();
    for permission in body
        .get("permissions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if !permissions.iter().any(|existing| existing == permission) {
            permissions.push(permission.to_string());
        }
    }
    match permissions
        .iter()
        .find(|permission| !registry.permissions.contains_key(*permission))
    {
        Some(unknown) => Err(format!("Unknown permission \"{unknown}\"")),
        None => Ok(permissions),
    }
}

async fn insert_grants(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    session: &ShopSession,
    token_id: &str,
    permissions: &[String],
) -> Result<(), sqlx::Error> {
    if permissions.is_empty() {
        return Ok(());
    }
    // Distinct timestamps keep the order in which permissions were selected.
    let now = Utc::now();
    let mut insert: QueryBuilder<Postgres> = QueryBuilder::new(
        "INSERT INTO mcp_permission_grants (app_handle, shop, token_id, permission_key, granted, created_at) ",
    );
    insert.push_values(permissions.iter().enumerate(), |mut row, (index, permission)| {
        row.push_bind(&session.shopify_app)
            .push_bind(&session.shop)
            .push_bind(token_id)
            .push_bind(permission)
            .push_bind(true)
            .push_bind(now + Duration::microseconds(index as i64));
    });
    insert.build().execute(&mut **tx).await?;
    Ok(())
}

async fn generate_token(existing_token_id: Option<String>) -> anyhow::Result<GeneratedMcpToken> {
    // Hashing is deliberately slow: keep it off the async workers.
    Ok(tokio::task::spawn_blocking(move || match existing_token_id {
        Some(token_id) => create_mcp_token_for_id(&token_id),
        None => create_mcp_token(),
    })
    .await?)
}

async fn capabilities(State(app): State<OpenShop>) -> Response {
    let registry = app.mcp_registry();
    let risk = |level: Option<McpRiskLevel>| level.unwrap_or(McpRiskLevel::Low);

    let tools: JsonMap = registry
        .tools
        .iter()
        .map(|(name, tool)| {
            (
                name.clone(),
                json!({
                    "name": name,
                    "description": tool.definition.description,
                    "inputSchema": tool.definition.input_schema,
                    "requiredPermissions": tool.definition.required_permissions,
                    "riskLevel": risk(tool.definition.risk_level),
                    "confirmationHint": tool.definition.confirmation_hint,
                    "source": tool.source,
                }),
            )
        })
        .collect();
    let resources: JsonMap = registry
        .resources
        .iter()
        .map(|(uri, resource)| {
            (
                uri.clone(),
                json!({
                    "uri": uri,
                    "name": resource.definition.name,
                    "description": resource.definition.description,
                    "mimeType": resource.definition.mime_type.as_deref().unwrap_or("text/plain"),
                    "requiredPermissions": resource.definition.required_permissions,
                    "riskLevel": risk(resource.definition.risk_level),
                    "source": resource.source,
                }),
            )
        })
        .collect();

    json_response(
        StatusCode::OK,
        json!({
            "enabled": registry.enabled,
            "permissions": registry.permissions,
            "tools": tools,
            "resources": resources,
            "defaultExpirationDays": DEFAULT_EXPIRATION_DAYS,
            "expirationOptions": [30, 90, 365, null],
        }),
    )
}

async fn list_tokens(State(app): State<OpenShop>, Extension(session): Extension<ShopSession>) -> ApiResult {
    let rows: Vec<McpToken> =
        sqlx::query_as("SELECT * FROM mcp_tokens WHERE app_handle = $1 AND shop = $2 ORDER BY created_at DESC")
            .bind(&session.shopify_app)
            .bind(&session.shop)
            .fetch_all(app.db())
            .await?;
    let token_ids: Vec<String> = rows.iter().map(|row| row.token_id.clone()).collect();
    let permissions = permissions_for_tokens(&app, &session, &token_ids).await?;

    let tokens: Vec<Value> = rows
        .iter()
        .map(|row| {
            let granted = permissions.get(&row.token_id).map(Vec::as_slice).unwrap_or_default();
            public_token(row, granted, app.mcp_registry(), &[])
        })
        .collect();
    Ok(json_response(StatusCode::OK, Value::Array(tokens)))
}

async fn create_token(
    State(app): State<OpenShop>,
    Extension(session): Extension<ShopSession>,
    body: Bytes,
) -> ApiResult {
    let registry = app.mcp_registry();
    let body = json_object_body(&body);
    let Some(name) = string_value(&body, "name") else {
        return Ok(json_error(StatusCode::BAD_REQUEST, "name is required"));
    };
    let permissions = match selected_permissions(&body, registry) {
        Ok(permissions) => permissions,
        Err(error) => return Ok(json_error(StatusCode::BAD_REQUEST, error)),
    };
    let expires_at = match expires_at_from_body(&body) {
        Ok(expires_at) => expires_at,
        Err(error) => return Ok(json_error(StatusCode::BAD_REQUEST, error)),
    };
    let generated = generate_token(None).await?;

    let mut tx = app.db().begin().await?;
    let row: McpToken = sqlx::query_as(
        "INSERT INTO mcp_tokens (app_handle, shop, name, token_id, token_hash, token_fingerprint, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING *",
    )
    .bind(&session.shopify_app)
    .bind(&session.shop)
    .bind(name)
    .bind(&generated.token_id)
    .bind(&generated.token_hash)
    .bind(&generated.token_fingerprint)
    .bind(expires_at)
    .fetch_one(&mut *tx)
    .await?;
    insert_grants(&mut tx, &session, &generated.token_id, &permissions).await?;
    tx.commit().await?;

    Ok(json_response(
        StatusCode::CREATED,
        json!({ "token": generated.token, "item": public_token(&row, &permissions, registry, &[]) }),
    ))
}

async fn get_token(
    State(app): State<OpenShop>,
    Extension(session): Extension<ShopSession>,
    Path(id): Path<String>,
) -> ApiResult {
    let Some(row) = find_token(&app, &session, &id).await? else {
        return Ok(token_not_found());
    };
    let permissions = permissions_for_tokens(&app, &session, std::slice::from_ref(&row.token_id)).await?;
    let audits: Vec<McpAuditLog> = sqlx::query_as(
        "SELECT * FROM mcp_audit_logs WHERE app_handle = $1 AND shop = $2 AND token_id = $3 \
         ORDER BY created_at DESC LIMIT 10",
    )
    .bind(&session.shopify_app)
    .bind(&session.shop)
    .bind(&row.token_id)
    .fetch_all(app.db())
    .await?;

    let granted = permissions.get(&row.token_id).map(Vec::as_slice).unwrap_or_default();
    Ok(json_response(
        StatusCode::OK,
        public_token(&row, granted, app.mcp_registry(), &audits),
    ))
}

async fn update_token(
    State(app): State<OpenShop>,
    Extension(session): Extension<ShopSession>,
    Path(id): Path<String>,
    body: Bytes,
) -> ApiResult {
    let body = json_object_body(&body);
    let Some(existing) = find_token(&app, &session, &id).await? else {
        return Ok(token_not_found());
    };
    if is_revoked(&existing) {
        return Ok(json_error(StatusCode::CONFLICT, "Cannot update a revoked token"));
    }

    let name = string_value(&body, "name");
    let status = body
        .get("status")
        .and_then(Value::as_str)
        .filter(|status| ["active", "disabled"].contains(status));
    let updates_expiration = body.contains_key("expiresAt") || body.contains_key("expiresInDays");
    let expires_at = if updates_expiration {
        match expires_at_from_body(&body) {
            Ok(expires_at) => expires_at,
            Err(error) => return Ok(json_error(StatusCode::BAD_REQUEST, error)),
        }
    } else {
        existing.expires_at
    };

    sqlx::query(
        "UPDATE mcp_tokens SET \
           name = COALESCE($2, name), status = COALESCE($3, status), expires_at = $4, updated_at = now() \
         WHERE id = $1",
    )
    .bind(existing.id)
    .bind(name)
    .bind(status)
    .bind(expires_at)
    .execute(app.db())
    .await?;
    Ok(json_response(StatusCode::OK, json!({ "ok": true })))
}

async fn set_permissions(
    State(app): State<OpenShop>,
    Extension(session): Extension<ShopSession>,
    Path(id): Path<String>,
    body: Bytes,
) -> ApiResult {
    let body = json_object_body(&body);
    let permissions = match selected_permissions(&body, app.mcp_registry()) {
        Ok(permissions) => permissions,
        Err(error) => return Ok(json_error(StatusCode::BAD_REQUEST, error)),
    };
    let Some(row) = find_token(&app, &session, &id).await? else {
        return Ok(token_not_found());
    };
    if is_revoked(&row) {
        return Ok(json_error(StatusCode::CONFLICT, "Cannot update a revoked token"));
    }

    let mut tx = app.db().begin().await?;
    sqlx::query("DELETE FROM mcp_permission_grants WHERE app_handle = $1 AND shop = $2 AND token_id = $3")
        .bind(&session.shopify_app)
        .bind(&session.shop)
        .bind(&row.token_id)
        .execute(&mut *tx)
        .await?;
    insert_grants(&mut tx, &session, &row.token_id, &permissions).await?;
    sqlx::query("UPDATE mcp_tokens SET updated_at = now() WHERE id = $1")
        .bind(row.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    Ok(json_response(StatusCode::OK, json!({ "ok": true })))
}

async fn rotate_token(
    State(app): State<OpenShop>,
    Extension(session): Extension<ShopSession>,
    Path(id): Path<String>,
) -> ApiResult {
    let Some(existing) = find_token(&app, &session, &id).await? else {
        return Ok(token_not_found());
    };
    if is_revoked(&existing) {
        return Ok(json_error(StatusCode::CONFLICT, "Cannot rotate a revoked token"));
    }
    let generated = generate_token(Some(existing.token_id)).await?;

    let updated = sqlx::query(
        "UPDATE mcp_tokens SET \
           token_hash = $1, token_fingerprint = $2, status = 'active', revoked_at = NULL, updated_at = now() \
         WHERE id = $3 AND app_handle = $4 AND shop = $5",
    )
    .bind(&generated.token_hash)
    .bind(&generated.token_fingerprint)
    .bind(existing.id)
    .bind(&session.shopify_app)
    .bind(&session.shop)
    .execute(app.db())
    .await?;
    if updated.rows_affected() == 0 {
        return Ok(token_not_found());
    }
    Ok(json_response(
        StatusCode::OK,
        json!({ "ok": true, "token": generated.token }),
    ))
}

async fn revoke_token(
    State(app): State<OpenShop>,
    Extension(session): Extension<ShopSession>,
    Path(id): Path<String>,
) -> ApiResult {
    let Some(existing) = find_token(&app, &session, &id).await? else {
        return Ok(token_not_found());
    };
    sqlx::query("UPDATE mcp_tokens SET status = 'revoked', revoked_at = now(), updated_at = now() WHERE id = $1")
        .bind(existing.id)
        .execute(app.db())
        .await?;
    Ok(json_response(StatusCode::OK, json!({ "ok": true })))
}

pub fn routes() -> Router<OpenShop> {
    Router::new()
        .route("/api/mcp/capabilities", get(capabilities))
        .route("/api/mcp/tokens", get(list_tokens).post(create_token))
        .route("/api/mcp/tokens/{id}", get(get_token).patch(update_token))
        .route("/api/mcp/tokens/{id}/permissions", put(set_permissions))
        .route("/api/mcp/tokens/{id}/rotate", post(rotate_token))
        .route("/api/mcp/tokens/{id}/revoke", post(revoke_token))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(value: Value) -> JsonMap {
        value.as_object().cloned().unwrap()
    }

    #[test]
    fn defaults_to_ninety_days() {
        let expires_at = expires_at_from_body(&JsonMap::new()).unwrap().unwrap();
        let days = (expires_at - Utc::now()).num_hours();
        assert!((90 * 24 - 1..=90 * 24).contains(&days));
    }

    #[test]
    fn reads_explicit_expiration() {
        assert_eq!(expires_at_from_body(&body(json!({ "expiresAt": null }))), Ok(None));
        let explicit = expires_at_from_body(&body(json!({ "expiresAt": "2030-01-01T00:00:00.000Z" })))
            .unwrap()
            .unwrap();
        assert_eq!(to_iso(&explicit), "2030-01-01T00:00:00.000Z");
        assert_eq!(
            expires_at_from_body(&body(json!({ "expiresAt": "soon" }))),
            Err("expiresAt must be a valid date string or null")
        );
        assert_eq!(
            expires_at_from_body(&body(json!({ "expiresAt": 5 }))),
            Err("expiresAt must be a valid date string or null")
        );
    }

    #[test]
    fn reads_relative_expiration() {
        assert_eq!(expires_at_from_body(&body(json!({ "expiresInDays": null }))), Ok(None));
        let relative = expires_at_from_body(&body(json!({ "expiresInDays": 30 })))
            .unwrap()
            .unwrap();
        assert!((29 * 24..=30 * 24).contains(&(relative - Utc::now()).num_hours()));
        for invalid in [json!(0), json!(-1), json!(1.5), json!("30")] {
            assert_eq!(
                expires_at_from_body(&body(json!({ "expiresInDays": invalid }))),
                Err("expiresInDays must be a positive integer or null")
            );
        }
    }

    #[test]
    fn trims_string_values() {
        let body = body(json!({ "name": "  Agent  ", "blank": "   ", "number": 1 }));
        assert_eq!(string_value(&body, "name"), Some("Agent"));
        assert_eq!(string_value(&body, "blank"), None);
        assert_eq!(string_value(&body, "number"), None);
    }
}
