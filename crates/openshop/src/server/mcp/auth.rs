//! Resolves an MCP bearer token to its shop, app and granted permissions.

use anyhow::Result;
use chrono::Utc;

use super::tokens::{extract_bearer_token, parse_mcp_token};
use super::verifier::{TokenVerifier, Verification};
use crate::app::OpenShop;
use crate::db::schema::McpToken;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedMcpToken {
    pub app_handle: String,
    pub shop: String,
    pub token_id: String,
    pub permissions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpAuthResult {
    Ok(ResolvedMcpToken),
    Rejected {
        status: u16,
        error: &'static str,
        token_id: Option<String>,
    },
}

fn rejected(status: u16, error: &'static str, token_id: Option<String>) -> McpAuthResult {
    McpAuthResult::Rejected {
        status,
        error,
        token_id,
    }
}

pub async fn resolve_mcp_bearer_token(app: &OpenShop, authorization: Option<&str>) -> Result<McpAuthResult> {
    let Some(bearer) = extract_bearer_token(authorization) else {
        return Ok(rejected(401, "Unauthorized: missing MCP bearer token", None));
    };
    let Some(parsed) = parse_mcp_token(&bearer) else {
        return Ok(rejected(401, "Unauthorized: invalid MCP token format", None));
    };

    let db = app.db();
    let stored: Option<McpToken> = sqlx::query_as("SELECT * FROM mcp_tokens WHERE token_id = $1 LIMIT 1")
        .bind(&parsed.token_id)
        .fetch_optional(db)
        .await?;
    let Some(stored) = stored else {
        return Ok(rejected(401, "Unauthorized: unknown MCP token", Some(parsed.token_id)));
    };

    // scrypt is deliberately slow: its cost is bounded, see `verifier`.
    match TokenVerifier::shared()
        .verify(&parsed.token, &stored.token_hash)
        .await?
    {
        Verification::Valid => {}
        Verification::Invalid => return Ok(rejected(401, "Unauthorized: invalid MCP token", Some(parsed.token_id))),
        Verification::Busy => {
            return Ok(rejected(
                503,
                "Service Unavailable: too many MCP tokens are being checked, try again",
                Some(parsed.token_id),
            ));
        }
    }
    if stored.status != "active" || stored.revoked_at.is_some() {
        return Ok(rejected(
            403,
            "Forbidden: MCP token is not active",
            Some(stored.token_id),
        ));
    }
    if stored.expires_at.is_some_and(|expires_at| expires_at <= Utc::now()) {
        return Ok(rejected(403, "Forbidden: MCP token has expired", Some(stored.token_id)));
    }

    let grants: Vec<(String,)> = sqlx::query_as(
        "SELECT permission_key FROM mcp_permission_grants \
         WHERE app_handle = $1 AND shop = $2 AND token_id = $3 AND granted = true",
    )
    .bind(&stored.app_handle)
    .bind(&stored.shop)
    .bind(&stored.token_id)
    .fetch_all(db)
    .await?;

    sqlx::query("UPDATE mcp_tokens SET last_used_at = now(), updated_at = now() WHERE id = $1")
        .bind(stored.id)
        .execute(db)
        .await?;

    Ok(McpAuthResult::Ok(ResolvedMcpToken {
        app_handle: stored.app_handle,
        shop: stored.shop,
        token_id: stored.token_id,
        permissions: grants.into_iter().map(|(key,)| key).collect(),
    }))
}
