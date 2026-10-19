//! Admin API payloads, limited to the fields the UI reads. Dates are ISO-8601 strings.

use std::collections::HashMap;

use indexmap::IndexMap;
use serde::Deserialize;
use serde_json::Value;

/// Row of `GET /api/runs` and `GET /api/flows/:name/runs`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowRun {
    pub id: String,
    pub flow_name: String,
    pub status: String,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub completed_at: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepResult {
    pub id: String,
    pub step_name: String,
    pub status: String,
    #[serde(default)]
    pub output: Option<Value>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub duration_ms: Option<i64>,
    #[serde(default)]
    pub created_at: Option<String>,
}

/// `GET /api/runs/:id` response.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunDetail {
    pub id: String,
    pub flow_name: String,
    pub status: String,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub completed_at: Option<String>,
    #[serde(default)]
    pub steps: Vec<StepResult>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    pub id: String,
    pub level: String,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub payload: Option<Value>,
    #[serde(default)]
    pub created_at: Option<String>,
    /// Set by log searches: `false` marks a context line around a match.
    #[serde(default, rename = "_matched")]
    pub matched: Option<bool>,
}

/// `GET /api/runs/:id/logs` response.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct LogsResponse {
    pub logs: Vec<LogEntry>,
    #[serde(default)]
    pub total: u64,
    /// Lines of each level in the whole run, whatever the filters.
    #[serde(default, rename = "levelCounts")]
    pub level_counts: HashMap<String, u64>,
    /// Where the next page starts; the response holds the newest lines only.
    #[serde(default)]
    pub next: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct FlowCron {
    pub schedule: String,
}

/// `GET /api/flows` response item.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowSummary {
    pub name: String,
    #[serde(default)]
    pub crons: Vec<FlowCron>,
    /// JSON Schema of the flow input, or `null`.
    #[serde(default)]
    pub input_schema: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum CronShops {
    One(String),
    Many(Vec<String>),
}

/// `GET /api/crons` response item.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CronItem {
    #[serde(default)]
    pub name: Option<String>,
    pub schedule: String,
    pub flow: String,
    #[serde(default)]
    pub input: Option<Value>,
    #[serde(default)]
    pub shops: Option<CronShops>,
    pub key: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ConfigFieldOption {
    pub label: String,
    pub value: String,
}

/// Editable field of a provider or function configuration.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigField {
    /// `text`, `password`, `number`, `select` or `checkbox`.
    #[serde(rename = "type")]
    pub kind: String,
    pub label: String,
    #[serde(default)]
    pub placeholder: Option<String>,
    #[serde(default)]
    pub options: Option<Vec<ConfigFieldOption>>,
    #[serde(default)]
    pub has_value: Option<bool>,
}

/// `GET /api/providers` response item.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSummary {
    pub name: String,
    #[serde(default)]
    pub fields: IndexMap<String, ConfigField>,
    #[serde(default)]
    pub config: IndexMap<String, Value>,
    #[serde(default)]
    pub configured: bool,
    #[serde(default)]
    pub last_checked_at: Option<String>,
    #[serde(default)]
    pub last_check_ok: Option<bool>,
}

/// Row of the MCP audit log.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpAuditLog {
    pub id: String,
    #[serde(default)]
    pub capability_name: Option<String>,
    #[serde(default)]
    pub target_shop: Option<String>,
    pub status: String,
    #[serde(default)]
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct McpPermissionSummary {
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
    pub group: String,
}

/// `GET /api/mcp/capabilities` response.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct McpCapabilitySummary {
    #[serde(default)]
    pub permissions: IndexMap<String, McpPermissionSummary>,
}

/// `GET /api/mcp/tokens` response item; `recentAudits` only comes with `GET /api/mcp/tokens/:id`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpTokenSummary {
    pub id: String,
    pub token_id: String,
    pub name: String,
    pub token_fingerprint: String,
    /// `active`, `disabled` or `revoked`.
    pub status: String,
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub last_used_at: Option<String>,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub stale_permissions: Vec<String>,
    #[serde(default)]
    pub recent_audits: Option<Vec<McpAuditLog>>,
}

/// Badge tone for a flow run or step status.
pub fn status_tone(status: &str) -> &'static str {
    match status {
        "completed" => "success",
        "failed" => "critical",
        "running" => "warning",
        "pending" | "sleeping" => "info",
        "canceled" => "neutral",
        _ => "auto",
    }
}

/// Tone and text of an inline status banner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BannerMessage {
    pub tone: &'static str,
    pub text: String,
}

impl BannerMessage {
    pub fn success(text: impl Into<String>) -> Self {
        Self {
            tone: "success",
            text: text.into(),
        }
    }

    pub fn critical(text: impl Into<String>) -> Self {
        Self {
            tone: "critical",
            text: text.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_statuses_to_badge_tones() {
        assert_eq!(status_tone("completed"), "success");
        assert_eq!(status_tone("failed"), "critical");
        assert_eq!(status_tone("running"), "warning");
        assert_eq!(status_tone("pending"), "info");
        assert_eq!(status_tone("sleeping"), "info");
        assert_eq!(status_tone("canceled"), "neutral");
        assert_eq!(status_tone("skipped"), "auto");
    }

    #[test]
    fn deserializes_run_details_with_camel_case_keys() {
        let run: RunDetail = serde_json::from_str(
            r#"{
                "id": "550e8400-e29b-41d4-a716-446655440000",
                "appHandle": "default",
                "shop": "demo.myshopify.com",
                "flowName": "syncOrders",
                "status": "failed",
                "input": { "limit": 10 },
                "error": "boom",
                "startedAt": "2026-10-06T12:00:00.000Z",
                "completedAt": null,
                "createdAt": "2026-10-06T11:59:59.000Z",
                "steps": [{
                    "id": "7b6f6f0a-58c6-4b6b-9d5c-0d8f0d0b7d11",
                    "stepName": "fetch",
                    "attempt": 1,
                    "status": "completed",
                    "output": { "count": 2 },
                    "error": null,
                    "durationMs": 42,
                    "createdAt": "2026-10-06T12:00:00.100Z",
                    "flowRunId": "550e8400-e29b-41d4-a716-446655440000"
                }]
            }"#,
        )
        .unwrap();
        assert_eq!(run.flow_name, "syncOrders");
        assert_eq!(run.completed_at, None);
        assert_eq!(run.steps[0].duration_ms, Some(42));
        assert!(run.steps[0].output.is_some());
    }

    #[test]
    fn deserializes_logs_crons_and_providers() {
        let logs: LogsResponse = serde_json::from_str(
            r#"{"logs":[{"id":"a","flowRunId":"r","level":"warn","message":null,"payload":null,
                "createdAt":"2026-10-06T12:00:00.000Z","_matched":false}],"total":7,
                "levelCounts":{"warn":3,"info":4},"next":"1.abc"}"#,
        )
        .unwrap();
        assert_eq!(logs.total, 7);
        assert_eq!(logs.level_counts["warn"], 3);
        assert_eq!(logs.next.as_deref(), Some("1.abc"));
        assert_eq!(logs.logs[0].matched, Some(false));

        let crons: Vec<CronItem> = serde_json::from_str(
            r#"[{"schedule":"0 * * * *","flow":"sync","shops":["a.myshopify.com","b.myshopify.com"],
                "index":0,"key":"sync:0 * * * *","enabled":true},
               {"name":"Nightly","schedule":"0 0 * * *","flow":"sync","shops":"all","input":{"full":true},
                "index":1,"key":"sync:0 0 * * *","enabled":false}]"#,
        )
        .unwrap();
        assert_eq!(
            crons[0].shops,
            Some(CronShops::Many(vec![
                "a.myshopify.com".into(),
                "b.myshopify.com".into()
            ]))
        );
        assert_eq!(crons[1].shops, Some(CronShops::One("all".into())));

        let provider: ProviderSummary = serde_json::from_str(
            r#"{"name":"erp","fields":{"zone":{"type":"select","label":"Zone","options":[{"label":"EU","value":"eu"}]},
                "apiKey":{"type":"password","label":"API key","hasValue":true}},
                "config":{"zone":"eu"},"configured":true,"lastCheckedAt":null,"lastCheckOk":null}"#,
        )
        .unwrap();
        assert_eq!(provider.fields.keys().collect::<Vec<_>>(), ["zone", "apiKey"]);
        assert_eq!(provider.fields["apiKey"].has_value, Some(true));
    }
}
