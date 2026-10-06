use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::get;
use serde_json::{Value, json};

use crate::app::OpenShop;
use crate::server::http::json_response;
use crate::types::{CronEntry, JsonMap};

/// A cron entry as configured; unset fields are omitted.
fn cron_json(cron: &CronEntry) -> Value {
    let mut entry = JsonMap::new();
    if let Some(name) = &cron.name {
        entry.insert("name".into(), Value::from(name.clone()));
    }
    entry.insert("schedule".into(), Value::from(cron.schedule.clone()));
    entry.insert("flow".into(), Value::from(cron.flow.clone()));
    if let Some(input) = &cron.input {
        entry.insert("input".into(), input.clone());
    }
    if let Some(shops) = &cron.shops {
        entry.insert("shops".into(), shops.to_json());
    }
    Value::Object(entry)
}

async fn list_flows(State(app): State<OpenShop>) -> Response {
    let config = app.config();
    let flows: Vec<Value> = config
        .flows
        .iter()
        .map(|(name, flow)| {
            let crons: Vec<Value> = config
                .crons
                .iter()
                .filter(|cron| cron.flow == *name)
                .map(cron_json)
                .collect();
            json!({
                "name": name,
                "crons": crons,
                "inputSchema": flow.input.as_ref().map(|schema| schema.json_schema.clone()),
            })
        })
        .collect();
    json_response(StatusCode::OK, Value::Array(flows))
}

pub fn routes() -> Router<OpenShop> {
    Router::new().route("/api/flows", get(list_flows))
}
