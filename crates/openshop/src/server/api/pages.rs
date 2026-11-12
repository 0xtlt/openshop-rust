use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::get;
use serde_json::json;

use crate::admin_paths::{AdminNavigationResources, resolve_admin_pages};
use crate::app::{OpenShop, OpenShopConfig};
use crate::server::http::json_response;
use crate::types::{AdminPageId, AdminPageMode, ResolvedAdminPages};

/// Page modes after hiding the built-in screens that have nothing to show.
pub fn resolved_admin_pages(config: &OpenShopConfig) -> ResolvedAdminPages {
    // With MCP turned off there is nothing to manage: its page is off too,
    // unless the application states a mode for it.
    let mcp_disabled = config.mcp.as_ref().is_some_and(|mcp| mcp.enabled == Some(false));
    let mut pages = config.builtin_pages.unwrap_or_default();
    if mcp_disabled && pages.get(AdminPageId::Mcp).is_none() {
        pages.set(AdminPageId::Mcp, AdminPageMode::Disabled);
    }
    resolve_admin_pages(
        Some(&pages),
        Some(&AdminNavigationResources {
            flows: config.flows.len(),
            providers: config.providers.len(),
            crons: config.crons.len(),
            functions: config.functions.len(),
        }),
    )
}

async fn pages(State(app): State<OpenShop>) -> Response {
    json_response(StatusCode::OK, json!(resolved_admin_pages(app.config())))
}

pub fn routes() -> Router<OpenShop> {
    Router::new().route("/api/pages", get(pages))
}
