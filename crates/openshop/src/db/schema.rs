//! Rows of the framework tables. JSON output uses camelCase keys and ISO timestamps.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use sqlx::FromRow;
use uuid::Uuid;

use crate::runtime::time::{iso, iso_option};

#[derive(Debug, Clone, PartialEq, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Installation {
    pub id: Uuid,
    pub app_handle: String,
    pub shop: String,
    pub access_token: Option<String>,
    pub scopes: Option<String>,
    pub nonce: Option<String>,
    #[serde(serialize_with = "iso::serialize")]
    pub installed_at: DateTime<Utc>,
    #[serde(serialize_with = "iso_option::serialize")]
    pub uninstalled_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct FlowRun {
    pub id: Uuid,
    pub app_handle: String,
    pub shop: String,
    pub flow_name: String,
    pub status: String,
    pub input: Option<Value>,
    pub error: Option<String>,
    #[serde(serialize_with = "iso_option::serialize")]
    pub deadline_at: Option<DateTime<Utc>>,
    pub parent_run_id: Option<String>,
    pub attempts: i32,
    #[serde(serialize_with = "iso_option::serialize")]
    pub available_at: Option<DateTime<Utc>>,
    pub worker_id: Option<String>,
    pub retry_policy: Option<Value>,
    #[serde(serialize_with = "iso_option::serialize")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(serialize_with = "iso_option::serialize")]
    pub completed_at: Option<DateTime<Utc>>,
    #[serde(serialize_with = "iso::serialize")]
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct StepResult {
    pub id: Uuid,
    pub step_name: String,
    pub attempt: i32,
    pub status: String,
    pub output: Option<Value>,
    pub error: Option<String>,
    pub duration_ms: Option<i32>,
    #[serde(serialize_with = "iso::serialize")]
    pub created_at: DateTime<Utc>,
    pub flow_run_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConfigRow {
    pub id: Uuid,
    pub app_handle: String,
    pub shop: String,
    pub provider_name: String,
    pub config: Option<Value>,
    #[serde(serialize_with = "iso_option::serialize")]
    pub last_checked_at: Option<DateTime<Utc>>,
    pub last_check_ok: Option<bool>,
    #[serde(serialize_with = "iso::serialize")]
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct CronOverride {
    pub id: Uuid,
    pub app_handle: String,
    pub shop: String,
    /// `flow:schedule`
    pub cron_key: String,
    pub enabled: bool,
    #[serde(serialize_with = "iso::serialize")]
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct LogRow {
    pub id: Uuid,
    pub flow_run_id: Option<String>,
    pub level: String,
    pub message: Option<String>,
    pub payload: Option<Value>,
    #[serde(serialize_with = "iso::serialize")]
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct McpToken {
    pub id: Uuid,
    pub app_handle: String,
    pub shop: String,
    pub name: String,
    pub token_id: String,
    pub token_hash: String,
    pub token_fingerprint: String,
    pub status: String,
    #[serde(serialize_with = "iso_option::serialize")]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(serialize_with = "iso_option::serialize")]
    pub revoked_at: Option<DateTime<Utc>>,
    #[serde(serialize_with = "iso_option::serialize")]
    pub last_used_at: Option<DateTime<Utc>>,
    #[serde(serialize_with = "iso::serialize")]
    pub created_at: DateTime<Utc>,
    #[serde(serialize_with = "iso::serialize")]
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct McpAuditLog {
    pub id: Uuid,
    pub app_handle: String,
    pub shop: String,
    pub token_id: Option<String>,
    pub capability_type: Option<String>,
    pub capability_name: Option<String>,
    pub permission_keys: Option<Value>,
    pub target_shop: Option<String>,
    pub status: String,
    pub error: Option<String>,
    pub duration_ms: Option<i32>,
    pub request_id: Option<String>,
    #[serde(serialize_with = "iso::serialize")]
    pub created_at: DateTime<Utc>,
}
