//! Built-in MCP tools and resources.

use std::future::Future;

use anyhow::{Result, anyhow, bail};
use chrono::{DateTime, Datelike, TimeZone, Utc};
use indexmap::IndexMap;
use serde_json::{Value, json};

use super::registry::core_mcp_permissions;
use crate::engine::dispatch::DispatchOptions;
use crate::runtime::time::to_iso;
use crate::server::api::run_logs::{RunLogOptions, get_filtered_run_logs};
use crate::server::shop_domain::normalize_shop_domain;
use crate::shopify::is_api_version;
use crate::types::{JsonMap, McpContext, McpResource, McpResourceContent, McpRiskLevel, McpTool};

#[derive(sqlx::FromRow)]
struct RecentRun {
    id: uuid::Uuid,
    flow_name: String,
    status: String,
    created_at: DateTime<Utc>,
    completed_at: Option<DateTime<Utc>>,
}

pub struct CoreCapabilities {
    pub tools: IndexMap<String, McpTool>,
    pub resources: IndexMap<String, McpResource>,
}

fn string_input<'a>(input: &'a JsonMap, key: &str) -> Option<&'a str> {
    input
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
}

fn boolean_input(input: &JsonMap, key: &str) -> Option<bool> {
    input.get(key).and_then(Value::as_bool)
}

fn object_input(input: &JsonMap, key: &str) -> Value {
    match input.get(key) {
        Some(Value::Object(object)) => Value::Object(object.clone()),
        _ => json!({}),
    }
}

fn number_input(input: &JsonMap, key: &str, fallback: usize, max: usize) -> usize {
    match input.get(key).and_then(Value::as_f64) {
        Some(value) if value.is_finite() => (value.floor().max(1.0) as usize).min(max),
        _ => fallback,
    }
}

async fn assert_run_visible(ctx: &McpContext, run_id: &str) -> Result<()> {
    let visible = match uuid::Uuid::parse_str(run_id) {
        Ok(run_id) => sqlx::query_as::<_, (uuid::Uuid,)>(
            "SELECT id FROM flow_runs WHERE id = $1 AND app_handle = $2 AND shop = $3 LIMIT 1",
        )
        .bind(run_id)
        .bind(&ctx.shopify_app)
        .bind(&ctx.shop)
        .fetch_optional(&ctx.db)
        .await?
        .is_some(),
        Err(_) => false,
    };
    if !visible {
        bail!("Run not found");
    }
    Ok(())
}

fn json_text(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}

/// Text content plus the same value as structured content.
fn structured_result(value: Value) -> Value {
    json!({
        "content": [{ "type": "text", "text": json_text(&value) }],
        "structuredContent": value,
    })
}

/// Shopify releases a stable Admin API version on the first of January, April,
/// July and October at 17:00 UTC.
pub fn latest_stable_shopify_admin_api_version(now: DateTime<Utc>) -> String {
    let year = now.year();
    for month in [10, 7, 4, 1] {
        let release = Utc.with_ymd_and_hms(year, month, 1, 17, 0, 0).single().unwrap_or(now);
        if now >= release {
            return format!("{year}-{month:02}");
        }
    }
    format!("{}-10", year - 1)
}

fn api_version_input(input: &JsonMap) -> Result<String> {
    let value = match string_input(input, "apiVersion") {
        None | Some("latest") => latest_stable_shopify_admin_api_version(Utc::now()),
        Some(requested) => requested.to_string(),
    };
    if !is_api_version(&value) {
        bail!("apiVersion must use YYYY-MM format or \"latest\"");
    }
    Ok(value)
}

async fn list_installed_shops(ctx: &McpContext) -> Result<Value> {
    let rows: Vec<(String, Option<String>, DateTime<Utc>)> = sqlx::query_as(
        "SELECT shop, scopes, installed_at FROM installations \
         WHERE app_handle = $1 AND access_token IS NOT NULL AND uninstalled_at IS NULL \
         ORDER BY shop ASC",
    )
    .bind(&ctx.shopify_app)
    .fetch_all(&ctx.db)
    .await?;

    let shops: Vec<Value> = rows
        .into_iter()
        .map(|(shop, scopes, installed_at)| json!({ "shop": shop, "scopes": scopes, "installedAt": to_iso(&installed_at) }))
        .collect();
    Ok(json!({ "shops": shops }))
}

async fn run_admin_graphql(ctx: McpContext, input: JsonMap) -> Result<Value> {
    let Some(shop) = string_input(&input, "shop").and_then(normalize_shop_domain) else {
        bail!("shop must be a myshopify.com domain");
    };
    let Some(query) = string_input(&input, "query") else {
        bail!("query is required");
    };
    let variables = object_input(&input, "variables");
    let api_version = api_version_input(&input)?;

    let not_installed = || anyhow!("Shop \"{shop}\" is not installed for app \"{}\"", ctx.shopify_app);
    let uninstalled: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM installations \
         WHERE app_handle = $1 AND shop = $2 AND uninstalled_at IS NOT NULL)",
    )
    .bind(&ctx.shopify_app)
    .bind(&shop)
    .fetch_one(&ctx.db)
    .await?;
    if uninstalled {
        return Err(not_installed());
    }

    // Nothing vouches for the shop of a tool call: it is background work. The
    // shop of the `client_credentials_shop` setting may get a token with the
    // client credentials grant when it has none, and a stored token is
    // renewed as a flow's would be.
    let client = ctx
        .app
        .shopify_client(&shop, &ctx.shopify_app)
        .with_api_version(api_version.clone());
    let body = json!({ "query": query, "variables": variables });
    let Some(response) = client.send(&body).await? else {
        return Err(not_installed());
    };

    let mut result = JsonMap::new();
    result.insert("shop".into(), Value::from(shop));
    result.insert("apiVersion".into(), Value::from(api_version));
    result.insert("status".into(), Value::from(response.status));
    result.insert("ok".into(), Value::from(response.ok()));
    match serde_json::from_str::<Value>(&response.body) {
        Ok(Value::Object(parsed)) => result.extend(parsed),
        _ => {
            result.insert("responseText".into(), Value::from(response.body));
        }
    }
    Ok(structured_result(Value::Object(result)))
}

async fn search_logs(ctx: McpContext, input: JsonMap) -> Result<Value> {
    let Some(run_id) = string_input(&input, "runId") else {
        bail!("runId is required");
    };
    assert_run_visible(&ctx, run_id).await?;

    let limit = number_input(&input, "limit", 50, 200);
    let mut result = get_filtered_run_logs(
        &ctx.db,
        run_id,
        RunLogOptions {
            query: string_input(&input, "q").unwrap_or_default(),
            levels: string_input(&input, "levels").unwrap_or("info,warn,error"),
            include_context: boolean_input(&input, "includeContext").unwrap_or(true),
        },
    )
    .await?;
    result.logs.truncate(limit);

    let returned = result.logs.len();
    let mut value = serde_json::to_value(result)?;
    value["returned"] = Value::from(returned);
    Ok(structured_result(value))
}

fn list_flows(ctx: &McpContext) -> Value {
    let config = ctx.app.config();
    let flows: Vec<Value> = config
        .flows
        .iter()
        .map(|(name, flow)| {
            let crons: Vec<Value> = config
                .crons
                .iter()
                .filter(|cron| cron.flow == *name)
                .map(|cron| json!({ "name": cron.name, "schedule": cron.schedule, "shops": cron.shops_json() }))
                .collect();
            json!({
                "name": name,
                "inputSchema": flow.input.as_ref().map(|schema| schema.json_schema.clone()),
                "crons": crons,
            })
        })
        .collect();
    json!({ "flows": flows })
}

async fn run_flow_tool(ctx: McpContext, input: JsonMap) -> Result<Value> {
    let Some(flow) = string_input(&input, "flow") else {
        bail!("flow is required");
    };
    if !ctx.app.config().flows.contains_key(flow) {
        bail!("Unknown flow \"{flow}\"");
    }
    let result = ctx
        .app
        .dispatch_flow(
            DispatchOptions::new(flow, ctx.shop.clone())
                .input(object_input(&input, "input"))
                .shopify_app(ctx.shopify_app.clone()),
        )
        .await?;
    Ok(structured_result(serde_json::to_value(result)?))
}

pub(crate) async fn cron_items(
    app: &crate::app::OpenShop,
    app_handle: &str,
    shop: &str,
    shops_first: bool,
) -> Result<Vec<Value>> {
    let overrides: Vec<(String, bool)> =
        sqlx::query_as("SELECT cron_key, enabled FROM cron_overrides WHERE app_handle = $1 AND shop = $2")
            .bind(app_handle)
            .bind(shop)
            .fetch_all(app.db())
            .await?;

    Ok(app
        .config()
        .crons
        .iter()
        .enumerate()
        .map(|(index, cron)| {
            let key = cron.key();
            let enabled = overrides
                .iter()
                .find(|(cron_key, _)| *cron_key == key)
                .is_none_or(|(_, enabled)| *enabled);
            let mut item = JsonMap::new();
            item.insert("index".into(), Value::from(index));
            item.insert("key".into(), Value::from(key));
            item.insert("name".into(), json!(cron.name));
            item.insert("flow".into(), Value::from(cron.flow.clone()));
            item.insert("schedule".into(), Value::from(cron.schedule.clone()));
            if shops_first {
                item.insert("shops".into(), cron.shops_json());
                item.insert("input".into(), json!(cron.input));
            } else {
                item.insert("input".into(), json!(cron.input));
                item.insert("shops".into(), cron.shops_json());
            }
            item.insert("enabled".into(), Value::from(enabled));
            Value::Object(item)
        })
        .collect())
}

/// Whether the application declares a cron with this key. An override of any
/// other key is never read, so none is stored.
pub(crate) fn is_cron_key(app: &crate::app::OpenShop, key: &str) -> bool {
    app.config().crons.iter().any(|cron| cron.key() == key)
}

pub(crate) async fn set_cron_enabled(
    app: &crate::app::OpenShop,
    app_handle: &str,
    shop: &str,
    key: &str,
    enabled: bool,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO cron_overrides (app_handle, shop, cron_key, enabled) VALUES ($1, $2, $3, $4) \
         ON CONFLICT (app_handle, shop, cron_key) DO UPDATE SET enabled = EXCLUDED.enabled, updated_at = now()",
    )
    .bind(app_handle)
    .bind(shop)
    .bind(key)
    .bind(enabled)
    .execute(app.db())
    .await?;
    Ok(())
}

async fn set_cron_enabled_tool(ctx: McpContext, input: JsonMap) -> Result<Value> {
    let Some(key) = string_input(&input, "key") else {
        bail!("key is required");
    };
    let Some(enabled) = boolean_input(&input, "enabled") else {
        bail!("enabled is required");
    };
    if !is_cron_key(&ctx.app, key) {
        bail!("Cron not found");
    }
    set_cron_enabled(&ctx.app, &ctx.shopify_app, &ctx.shop, key, enabled).await?;
    Ok(structured_result(json!({ "ok": true, "key": key, "enabled": enabled })))
}

fn empty_schema() -> Value {
    json!({ "type": "object", "additionalProperties": false, "properties": {} })
}

fn logs_search_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["runId"],
        "properties": {
            "runId": { "type": "string", "description": "Flow run UUID." },
            "q": { "type": "string", "description": "Log search query." },
            "levels": { "type": "string", "description": "Comma-separated log levels. Defaults to info,warn,error." },
            "includeContext": { "type": "boolean", "description": "Include contextual logs around matches. Defaults to true." },
            "limit": { "type": "number", "minimum": 1, "maximum": 200, "description": "Maximum logs returned. Defaults to 50." },
        },
    })
}

const LOGS_SEARCH_DESCRIPTION: &str = "Search logs for a visible OpenShop flow run.";

fn tool<F, Fut>(
    description: &str,
    permissions: &[&str],
    risk_level: McpRiskLevel,
    input_schema: Value,
    run: F,
) -> McpTool
where
    F: Fn(McpContext, JsonMap) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Value>> + Send + 'static,
{
    McpTool::new(description, run)
        .permissions(permissions.iter().copied())
        .risk(risk_level)
        .input_schema(input_schema)
}

fn json_resource(value: Value) -> McpResourceContent {
    McpResourceContent {
        text: json_text(&value),
        mime_type: Some("application/json".to_string()),
    }
}

pub fn create_core_mcp_capabilities() -> CoreCapabilities {
    let mut tools = IndexMap::new();

    tools.insert(
        "openshop.shops.list".to_string(),
        tool(
            "List installed shops for this OpenShop app.",
            &[],
            McpRiskLevel::Low,
            empty_schema(),
            |ctx, _input| async move { Ok(structured_result(list_installed_shops(&ctx).await?)) },
        ),
    );

    tools.insert(
        "openshop.admin.graphql".to_string(),
        tool(
            "Run arbitrary Shopify Admin GraphQL queries and mutations for an installed shop.",
            &["shopify_admin_graphql"],
            McpRiskLevel::High,
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["shop", "query"],
                "properties": {
                    "shop": { "type": "string", "description": "Installed shop domain from openshop.shops.list." },
                    "query": { "type": "string", "description": "Shopify Admin GraphQL query or mutation." },
                    "variables": { "type": "object", "description": "GraphQL variables object." },
                    "apiVersion": { "type": "string", "description": "Shopify Admin API version, for example 2026-04. Defaults to the latest stable version when omitted or set to \"latest\"." },
                },
            }),
            run_admin_graphql,
        )
        .confirmation_hint(
            "This can read or mutate Shopify Admin data depending on the app OAuth scopes and GraphQL document.",
        ),
    );

    tools.insert(
        "openshop.logs.search".to_string(),
        tool(
            LOGS_SEARCH_DESCRIPTION,
            &["read_logs"],
            McpRiskLevel::Low,
            logs_search_schema(),
            search_logs,
        ),
    );

    tools.insert(
        "openshop.flows.list".to_string(),
        tool(
            "List configured OpenShop flows.",
            &["read_flows"],
            McpRiskLevel::Low,
            empty_schema(),
            |ctx, _input| async move { Ok(structured_result(list_flows(&ctx))) },
        ),
    );

    tools.insert(
        "openshop.flows.run".to_string(),
        tool(
            "Trigger an OpenShop flow for the authenticated shop.",
            &["run_flows"],
            McpRiskLevel::High,
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["flow"],
                "properties": {
                    "flow": { "type": "string", "description": "Flow name." },
                    "input": { "type": "object", "description": "Flow input object." },
                },
            }),
            run_flow_tool,
        )
        .confirmation_hint("Runs can call Shopify and external providers depending on the flow implementation."),
    );

    tools.insert(
        "openshop.crons.list".to_string(),
        tool(
            "List configured OpenShop crons with per-shop enabled state.",
            &["read_crons"],
            McpRiskLevel::Low,
            empty_schema(),
            |ctx, _input| async move {
                let crons = cron_items(&ctx.app, &ctx.shopify_app, &ctx.shop, true).await?;
                Ok(structured_result(json!({ "crons": crons })))
            },
        ),
    );

    tools.insert(
        "openshop.crons.set_enabled".to_string(),
        tool(
            "Enable or disable a configured OpenShop cron for the authenticated shop.",
            &["manage_crons"],
            McpRiskLevel::Medium,
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["key", "enabled"],
                "properties": {
                    "key": { "type": "string", "description": "Cron key from openshop.crons.list." },
                    "enabled": { "type": "boolean" },
                },
            }),
            set_cron_enabled_tool,
        )
        .confirmation_hint("Disabling a cron can stop scheduled automation for this shop."),
    );

    let mut resources = IndexMap::new();

    resources.insert(
        "openshop://docs/log-search".to_string(),
        McpResource::new("Log search guide", |_ctx| async move {
            Ok(McpResourceContent::from(
                [
                    "# OpenShop log search",
                    "",
                    "Use the `openshop.logs.search` tool with `read_logs` permission.",
                    "",
                    "Input fields:",
                    "- `runId` is required and must be a flow run visible to the token shop/app.",
                    "- `q` accepts the same query syntax as the OpenShop admin log search.",
                    "- `levels` defaults to `info,warn,error`.",
                    "- `includeContext` defaults to `true`.",
                    "- `limit` defaults to `50` and is capped at `200`.",
                ]
                .join("\n"),
            ))
        })
        .description("How to search OpenShop logs through MCP.")
        .mime_type("text/markdown")
        .permissions(["read_logs"]),
    );

    resources.insert(
        "openshop://permissions".to_string(),
        McpResource::new("MCP permission catalog", |ctx| async move {
            let core: JsonMap = core_mcp_permissions()
                .into_iter()
                .map(|(key, permission)| {
                    (
                        key,
                        json!({
                            "label": permission.label,
                            "description": permission.description,
                            "group": permission.group,
                            "riskLevel": permission.risk_level,
                        }),
                    )
                })
                .collect();
            let custom = ctx
                .app
                .config()
                .mcp
                .as_ref()
                .map(|mcp| serde_json::to_value(mcp.permissions()))
                .transpose()?
                .unwrap_or_else(|| json!({}));
            Ok(json_resource(json!({ "core": core, "custom": custom })))
        })
        .description("Core and custom MCP permission definitions visible to this OpenShop app.")
        .mime_type("application/json"),
    );

    resources.insert(
        "openshop://tools/openshop.logs.search".to_string(),
        McpResource::new("openshop.logs.search reference", |_ctx| async move {
            Ok(json_resource(json!({
                "name": "openshop.logs.search",
                "description": LOGS_SEARCH_DESCRIPTION,
                "requiredPermissions": ["read_logs"],
                "inputSchema": logs_search_schema(),
            })))
        })
        .description("Tool reference for OpenShop log search.")
        .mime_type("application/json")
        .permissions(["read_logs"]),
    );

    resources.insert(
        "openshop://runs/recent".to_string(),
        McpResource::new("Recent flow runs", |ctx| async move {
            let rows: Vec<RecentRun> = sqlx::query_as(
                "SELECT id, flow_name, status, created_at, completed_at FROM flow_runs \
                 WHERE app_handle = $1 AND shop = $2 ORDER BY created_at DESC LIMIT 20",
            )
            .bind(&ctx.shopify_app)
            .bind(&ctx.shop)
            .fetch_all(&ctx.db)
            .await
            .map_err(|error| anyhow!(error))?;

            let runs: Vec<Value> = rows
                .into_iter()
                .map(|run| {
                    json!({
                        "id": run.id,
                        "flowName": run.flow_name,
                        "status": run.status,
                        "createdAt": to_iso(&run.created_at),
                        "completedAt": run.completed_at.as_ref().map(to_iso),
                    })
                })
                .collect();
            Ok(json_resource(json!({ "runs": runs })))
        })
        .description("Recent flow runs for the authenticated shop.")
        .mime_type("application/json")
        .permissions(["read_flows"]),
    );

    CoreCapabilities { tools, resources }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_latest_released_api_version() {
        let at = |year, month, day, hour| Utc.with_ymd_and_hms(year, month, day, hour, 0, 0).unwrap();
        assert_eq!(latest_stable_shopify_admin_api_version(at(2026, 4, 1, 16)), "2026-01");
        assert_eq!(latest_stable_shopify_admin_api_version(at(2026, 4, 1, 17)), "2026-04");
        assert_eq!(latest_stable_shopify_admin_api_version(at(2026, 12, 31, 0)), "2026-10");
        assert_eq!(latest_stable_shopify_admin_api_version(at(2026, 1, 1, 0)), "2025-10");
        assert_eq!(latest_stable_shopify_admin_api_version(at(2026, 7, 15, 0)), "2026-07");
    }

    #[test]
    fn validates_api_version_input() {
        let input = |value: Value| value.as_object().cloned().unwrap();
        assert_eq!(
            api_version_input(&input(json!({ "apiVersion": "2025-10" }))).unwrap(),
            "2025-10"
        );
        assert!(is_api_version(&api_version_input(&input(json!({}))).unwrap()));
        assert!(is_api_version(
            &api_version_input(&input(json!({ "apiVersion": "latest" }))).unwrap()
        ));
        assert!(api_version_input(&input(json!({ "apiVersion": "unstable" }))).is_err());
    }

    #[test]
    fn clamps_numeric_inputs() {
        let input = |value: Value| value.as_object().cloned().unwrap();
        assert_eq!(number_input(&input(json!({})), "limit", 50, 200), 50);
        assert_eq!(number_input(&input(json!({ "limit": 0 })), "limit", 50, 200), 1);
        assert_eq!(number_input(&input(json!({ "limit": 12.9 })), "limit", 50, 200), 12);
        assert_eq!(number_input(&input(json!({ "limit": 999 })), "limit", 50, 200), 200);
        assert_eq!(number_input(&input(json!({ "limit": "5" })), "limit", 50, 200), 50);
    }

    #[test]
    fn exposes_the_core_capabilities() {
        let core = create_core_mcp_capabilities();
        assert_eq!(
            core.tools.keys().collect::<Vec<_>>(),
            vec![
                "openshop.shops.list",
                "openshop.admin.graphql",
                "openshop.logs.search",
                "openshop.flows.list",
                "openshop.flows.run",
                "openshop.crons.list",
                "openshop.crons.set_enabled",
            ]
        );
        assert_eq!(core.resources.len(), 4);
    }
}
