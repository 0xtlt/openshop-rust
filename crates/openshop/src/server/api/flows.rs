use axum::Router;
use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::get;
use serde_json::Value;

use crate::app::OpenShop;
use crate::server::http::json_response;
use crate::server::mcp::core::flow_items;
use crate::server::shop::ShopSession;
use crate::types::{Cron, CronShops, JsonMap};

/// A cron entry as configured; unset fields are omitted. The shops it names
/// are only the one of the session.
fn cron_json(cron: &Cron, shop: &str) -> Value {
    let mut entry = JsonMap::new();
    if let Some(name) = &cron.name {
        entry.insert("name".into(), Value::from(name.clone()));
    }
    entry.insert("schedule".into(), Value::from(cron.schedule.clone()));
    entry.insert("flow".into(), Value::from(cron.flow.clone()));
    if let Some(input) = &cron.input {
        entry.insert("input".into(), input.clone());
    }
    if cron.shops != CronShops::Global {
        entry.insert("shops".into(), cron.shops.to_json_for(shop));
    }
    Value::Object(entry)
}

async fn list_flows(State(app): State<OpenShop>, Extension(session): Extension<ShopSession>) -> Response {
    let flows = flow_items(&app, &session.shop, |cron| cron_json(cron, &session.shop));
    json_response(StatusCode::OK, Value::Array(flows))
}

pub fn routes() -> Router<OpenShop> {
    Router::new().route("/api/flows", get(list_flows))
}
