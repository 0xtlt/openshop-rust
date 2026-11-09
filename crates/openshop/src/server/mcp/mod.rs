//! MCP endpoint: JSON-RPC over HTTP, authenticated with OpenShop-issued bearer tokens.

pub mod auth;
pub mod core;
pub mod registry;
pub mod schema;
pub mod tokens;
mod verifier;

use std::panic::AssertUnwindSafe;
use std::time::{Duration, Instant};

use anyhow::Result;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use futures::FutureExt;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use self::auth::{McpAuthResult, ResolvedMcpToken, resolve_mcp_bearer_token};
use self::registry::{McpRegistry, has_all_permissions};
use self::schema::validate_mcp_tool_arguments;
use crate::app::OpenShop;
use crate::server::client::request_client_ip;
use crate::server::connectors::Connectors;
use crate::server::http::{client_error, public_error, report_failure};
use crate::server::shop_domain::normalize_shop_domain;
use crate::shopify::client::ShopifyClient;
use crate::types::{JsonMap, McpContext};

const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
const MAX_MCP_PAYLOAD_BYTES: usize = 1_000_000;
const MAX_MCP_EXECUTION: Duration = Duration::from_millis(30_000);

/// The capability a request calls, as `mcp_audit_logs` records it.
#[derive(Default, Clone)]
struct ActiveAudit {
    /// `tool` or `resource`.
    capability_type: Option<&'static str>,
    capability_name: Option<String>,
    permission_keys: Vec<String>,
    target_shop: Option<String>,
}

fn with_protocol_header(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert("MCP-Protocol-Version", HeaderValue::from_static(MCP_PROTOCOL_VERSION));
    response
}

fn plain_error(status: u16, error: &str) -> Response {
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    with_protocol_header((status, axum::Json(json!({ "error": error }))).into_response())
}

fn json_rpc_result(id: Option<&Value>, result: Value) -> Response {
    with_protocol_header(
        axum::Json(json!({ "jsonrpc": "2.0", "id": id.cloned().unwrap_or(Value::Null), "result": result }))
            .into_response(),
    )
}

fn json_rpc_error(id: Option<&Value>, code: i64, message: &str, status: StatusCode) -> Response {
    with_protocol_header(
        (
            status,
            axum::Json(json!({
                "jsonrpc": "2.0",
                "id": id.cloned().unwrap_or(Value::Null),
                "error": { "code": code, "message": message },
            })),
        )
            .into_response(),
    )
}

fn params_object(params: Option<&Value>) -> JsonMap {
    match params {
        Some(Value::Object(params)) => params.clone(),
        _ => JsonMap::new(),
    }
}

fn text_content(text: String) -> Value {
    json!([{ "type": "text", "text": text }])
}

/// Strings become text content; objects without `content` / `structuredContent`
/// are returned both as pretty-printed text and as structured content.
fn normalize_tool_result(result: Value) -> Value {
    match &result {
        Value::String(text) => json!({ "content": text_content(text.clone()) }),
        Value::Object(object)
            if object.get("content").is_some_and(Value::is_array) || object.contains_key("structuredContent") =>
        {
            result
        }
        Value::Object(_) | Value::Array(_) => json!({
            "content": text_content(serde_json::to_string_pretty(&result).unwrap_or_default()),
            "structuredContent": result,
        }),
        Value::Null => json!({ "content": text_content(String::new()) }),
        other => json!({ "content": text_content(other.to_string()) }),
    }
}

fn create_context(app: &OpenShop, token: &ResolvedMcpToken, signal: CancellationToken) -> McpContext {
    McpContext {
        shop: token.shop.clone(),
        shopify_app: token.app_handle.clone(),
        token_id: token.token_id.clone(),
        permissions: token.permissions.clone(),
        signal,
        db: app.db().clone(),
        shopify: ShopifyClient::new(app, &token.shop, &token.app_handle),
        connectors: Connectors::for_shop(app, &token.shop, &token.app_handle),
        app: app.clone(),
    }
}

/// Runs a capability with a hard time limit. The signal is cancelled on timeout
/// and when the request is dropped. A panicking capability fails like one that
/// returned an error, so that it is audited and answered.
async fn with_timeout<T, F, Fut>(label: &str, run: F) -> Result<T>
where
    F: FnOnce(CancellationToken) -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let signal = CancellationToken::new();
    let _cancel_on_drop = signal.clone().drop_guard();
    let guarded = AssertUnwindSafe(async { run(signal).await }).catch_unwind();
    match tokio::time::timeout(MAX_MCP_EXECUTION, guarded).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err(public_error(format!("{label} panicked"))),
        Err(_) => Err(public_error(format!(
            "{label} timed out after {}ms",
            MAX_MCP_EXECUTION.as_millis()
        ))),
    }
}

fn list_tools(registry: &McpRegistry, permissions: &[String]) -> Value {
    let tools: Vec<Value> = registry
        .tools
        .iter()
        .filter(|(_, tool)| has_all_permissions(permissions, &tool.definition.required_permissions))
        .map(|(name, tool)| {
            json!({
                "name": name,
                "description": tool.definition.description,
                "inputSchema": tool.definition.input_schema.clone()
                    .unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
            })
        })
        .collect();
    json!({ "tools": tools })
}

fn list_resources(registry: &McpRegistry, permissions: &[String]) -> Value {
    let resources: Vec<Value> = registry
        .resources
        .iter()
        .filter(|(_, resource)| has_all_permissions(permissions, &resource.definition.required_permissions))
        .map(|(uri, resource)| {
            let mut item = JsonMap::new();
            item.insert("uri".into(), Value::from(uri.clone()));
            item.insert("name".into(), Value::from(resource.definition.name.clone()));
            if let Some(description) = &resource.definition.description {
                item.insert("description".into(), Value::from(description.clone()));
            }
            item.insert(
                "mimeType".into(),
                Value::from(
                    resource
                        .definition
                        .mime_type
                        .clone()
                        .unwrap_or_else(|| "text/plain".to_string()),
                ),
            );
            Value::Object(item)
        })
        .collect();
    json!({ "resources": resources })
}

/// One authenticated JSON-RPC request.
#[derive(Clone, Copy)]
struct McpCall<'a> {
    app: &'a OpenShop,
    registry: &'a McpRegistry,
    token: &'a ResolvedMcpToken,
    request: &'a JsonMap,
    started_at: Instant,
    request_id: Option<&'a str>,
}

impl McpCall<'_> {
    /// Writes the row of `mcp_audit_logs` of the call. `status` is `success`,
    /// `denied` or `error`.
    async fn audit(&self, active: &ActiveAudit, status: &str, error: Option<&str>) -> Result<()> {
        sqlx::query(
            "INSERT INTO mcp_audit_logs \
               (app_handle, shop, token_id, capability_type, capability_name, permission_keys, target_shop, status, error, duration_ms, request_id) \
             VALUES ($1, $2, $3, $4, $5, $6::json, $7, $8, $9, $10, $11)",
        )
        .bind(&self.token.app_handle)
        .bind(&self.token.shop)
        .bind(&self.token.token_id)
        .bind(active.capability_type)
        .bind(&active.capability_name)
        .bind(serde_json::to_string(&active.permission_keys)?)
        .bind(&active.target_shop)
        .bind(status)
        .bind(error)
        .bind(self.started_at.elapsed().as_millis().min(i32::MAX as u128) as i32)
        .bind(self.request_id)
        .execute(self.app.db())
        .await?;
        Ok(())
    }

    /// Records the capability called in `active`. When the token lacks a
    /// permission it requires, the denial is audited and its answer returned.
    async fn authorize(
        &self,
        active: &mut ActiveAudit,
        capability_type: &'static str,
        name: &str,
        required_permissions: &[String],
    ) -> Result<Option<Response>> {
        *active = ActiveAudit {
            capability_type: Some(capability_type),
            capability_name: Some(name.to_string()),
            permission_keys: required_permissions.to_vec(),
            target_shop: None,
        };
        if has_all_permissions(&self.token.permissions, required_permissions) {
            return Ok(None);
        }
        self.audit(active, "denied", Some("Permission denied")).await?;
        Ok(Some(json_rpc_error(
            self.request.get("id"),
            -32003,
            &format!("Permission denied for {capability_type} \"{name}\""),
            StatusCode::OK,
        )))
    }
}

/// Outcome of a dispatched method: a response, or an error to audit and report.
async fn dispatch(call: McpCall<'_>, method: &str, active: &mut ActiveAudit) -> Result<Response> {
    let McpCall {
        app,
        registry,
        token,
        request,
        ..
    } = call;
    let id = request.get("id");
    let ok = StatusCode::OK;

    match method {
        "initialize" => Ok(json_rpc_result(
            id,
            json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {
                    "tools": { "listChanged": false },
                    "resources": { "subscribe": false, "listChanged": false },
                },
                "serverInfo": { "name": "openshop", "version": env!("CARGO_PKG_VERSION") },
            }),
        )),

        "tools/list" => Ok(json_rpc_result(id, list_tools(registry, &token.permissions))),

        "tools/call" => {
            let params = params_object(request.get("params"));
            let name = params.get("name").and_then(Value::as_str).unwrap_or_default();
            let Some(tool) = registry.tools.get(name) else {
                return Ok(json_rpc_error(id, -32602, &format!("Unknown tool \"{name}\""), ok));
            };
            if let Some(denied) = call
                .authorize(active, "tool", name, &tool.definition.required_permissions)
                .await?
            {
                return Ok(denied);
            }

            let input =
                match validate_mcp_tool_arguments(tool.definition.input_schema.as_ref(), params.get("arguments")) {
                    Ok(input) => input,
                    Err(error) => return Ok(json_rpc_error(id, -32602, &error, ok)),
                };
            if name == "openshop.admin.graphql" {
                // The shop the call asked for, so a refusal shows where it aimed.
                active.target_shop = match input.get("shop").and_then(Value::as_str) {
                    Some(requested) => normalize_shop_domain(requested),
                    None => Some(token.shop.clone()),
                };
            }

            let run = tool.definition.run.clone();
            let result = with_timeout(&format!("Tool \"{name}\""), |signal| {
                run(create_context(app, token, signal), input)
            })
            .await?;
            call.audit(active, "success", None).await?;
            Ok(json_rpc_result(id, normalize_tool_result(result)))
        }

        "resources/list" => Ok(json_rpc_result(id, list_resources(registry, &token.permissions))),

        "resources/read" => {
            let params = params_object(request.get("params"));
            let uri = params.get("uri").and_then(Value::as_str).unwrap_or_default();
            let Some(resource) = registry.resources.get(uri) else {
                return Ok(json_rpc_error(id, -32602, &format!("Unknown resource \"{uri}\""), ok));
            };
            if let Some(denied) = call
                .authorize(active, "resource", uri, &resource.definition.required_permissions)
                .await?
            {
                return Ok(denied);
            }

            let read = resource.definition.read.clone();
            let content = with_timeout(&format!("Resource \"{uri}\""), |signal| {
                read(create_context(app, token, signal))
            })
            .await?;
            call.audit(active, "success", None).await?;

            let mime_type = content
                .mime_type
                .or_else(|| resource.definition.mime_type.clone())
                .unwrap_or_else(|| "text/plain".to_string());
            Ok(json_rpc_result(
                id,
                json!({ "contents": [{ "uri": uri, "mimeType": mime_type, "text": content.text }] }),
            ))
        }

        _ => Ok(json_rpc_error(id, -32601, &format!("Method not found: {method}"), ok)),
    }
}

pub(crate) async fn handle(State(app): State<OpenShop>, request: Request) -> Response {
    // Before the token: a disabled endpoint looks nothing up and checks no hash.
    let registry = app.mcp_registry();
    if !registry.enabled {
        return plain_error(403, "MCP is disabled");
    }

    let client = request_client_ip(&request);
    let authorization = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);

    let token = match resolve_mcp_bearer_token(&app, authorization.as_deref()).await {
        Ok(McpAuthResult::Ok(token)) => token,
        Ok(McpAuthResult::Rejected {
            status,
            error,
            token_id,
        }) => {
            // The token id names a token, never its secret.
            crate::runtime::logger::warn_with(
                "[openshop] MCP token refused",
                json!({
                    "tokenId": token_id,
                    "reason": error,
                    "status": status,
                    "client": client.map(|address| address.to_string()),
                }),
            );
            return plain_error(status, error);
        }
        Err(error) => {
            report_failure("[openshop] MCP authentication failed", &error);
            return plain_error(500, "Internal Server Error");
        }
    };

    let Ok(body) = axum::body::to_bytes(request.into_body(), MAX_MCP_PAYLOAD_BYTES).await else {
        return plain_error(413, "MCP payload too large");
    };
    let invalid = || json_rpc_error(None, -32600, "Invalid JSON-RPC request", StatusCode::BAD_REQUEST);
    let Ok(Value::Object(request)) = serde_json::from_slice::<Value>(&body) else {
        return invalid();
    };
    let Some(method) = request.get("method").and_then(Value::as_str).map(str::to_string) else {
        return invalid();
    };
    if request.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return invalid();
    }

    if !request.contains_key("id") && method.starts_with("notifications/") {
        return with_protocol_header((StatusCode::ACCEPTED, Body::empty()).into_response());
    }

    let started_at = Instant::now();
    let request_id = match request.get("id") {
        None | Some(Value::Null) => None,
        Some(Value::String(id)) => Some(id.clone()),
        Some(other) => Some(other.to_string()),
    };
    let mut active = ActiveAudit::default();

    let call = McpCall {
        app: &app,
        registry,
        token: &token,
        request: &request,
        started_at,
        request_id: request_id.as_deref(),
    };
    match dispatch(call, &method, &mut active).await {
        Ok(response) => response,
        Err(error) => {
            // What the caller did wrong is told as it is; a database or network
            // failure is logged under an id, and only the id goes out.
            let message = client_error(&format!("MCP {method} failed"), &error).message;
            let _ = call.audit(&active, "error", Some(&message)).await;
            json_rpc_error(request.get("id"), -32000, &message, StatusCode::OK)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_tool_results() {
        assert_eq!(
            normalize_tool_result(json!("done")),
            json!({ "content": [{ "type": "text", "text": "done" }] })
        );
        let passthrough = json!({ "content": [{ "type": "text", "text": "x" }], "structuredContent": { "a": 1 } });
        assert_eq!(normalize_tool_result(passthrough.clone()), passthrough);
        assert_eq!(
            normalize_tool_result(json!({ "structuredContent": null })),
            json!({ "structuredContent": null })
        );
        assert_eq!(
            normalize_tool_result(json!({ "ok": true })),
            json!({
                "content": [{ "type": "text", "text": "{\n  \"ok\": true\n}" }],
                "structuredContent": { "ok": true },
            })
        );
        assert_eq!(
            normalize_tool_result(Value::Null),
            json!({ "content": [{ "type": "text", "text": "" }] })
        );
        assert_eq!(
            normalize_tool_result(json!(42)),
            json!({ "content": [{ "type": "text", "text": "42" }] })
        );
    }
}
