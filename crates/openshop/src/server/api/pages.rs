use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::get;
use serde_json::json;

use crate::app::{OpenShop, OpenShopConfig};
use crate::server::http::json_response;
use crate::types::ResolvedAdminPages;
use openshop_shared::{AdminNavigationResources, resolve_admin_pages};

/// Page modes after hiding the built-in screens that have nothing to show.
pub fn resolved_admin_pages(config: &OpenShopConfig) -> ResolvedAdminPages {
    resolve_admin_pages(
        config.pages.as_ref(),
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
