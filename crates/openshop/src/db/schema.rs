//! Rows of the framework tables. JSON output uses camelCase keys and ISO timestamps.
//!
//! [`Installation`], [`FlowRun`], [`StepResult`] and [`LogRow`] are
//! [records](Record): an application reads them across shops, with typed
//! columns, and never writes them.
//!
//! ```no_run
//! use openshop::{FlowRun, PgPool, Result};
//!
//! /// The runs that failed, the latest first, whatever their shop.
//! async fn failed(db: &PgPool) -> Result<Vec<FlowRun>> {
//!     FlowRun::all_shops(db)
//!         .filter(FlowRun::STATUS.eq("failed"))
//!         .order_by(FlowRun::CREATED_AT.desc())
//!         .limit(50)
//!         .all()
//!         .await
//! }
//! ```

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::record::{Column, Condition, Query, Record};
use crate::runtime::time::{iso, iso_option};
use crate::types::ShopScope;

/// Makes a row of a framework table a [`Record`], with one typed column per
/// field a query filters or sorts on: `FlowRun::STATUS`. The columns after
/// `read` hold JSON: they are read with the row, and not compared.
macro_rules! record {
    (
        $record:ty, $table:literal,
        { $($constant:ident: $value:ty = $column:literal),+ $(,)? }
        $(, read: [$($json:literal),+ $(,)?])?
    ) => {
        impl Record for $record {
            const TABLE: &'static str = $table;
            const COLUMNS: &'static [&'static str] = &[$($column),+ $($(, $json)+)?];
        }

        impl $record {
            $(
                #[doc = concat!("The column `", $column, "`.")]
                pub const $constant: Column<Self, $value> = Column::new($column);
            )+
        }
    };
}

/// A Shopify app installed on a shop, without the access token: it stays in
/// the database, and [`ShopifyClient`](crate::ShopifyClient) reads it there.
#[derive(Debug, Clone, PartialEq, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Installation {
    pub id: Uuid,
    pub app_handle: String,
    pub shop: String,
    pub scopes: Option<String>,
    #[serde(serialize_with = "iso::serialize")]
    pub installed_at: DateTime<Utc>,
    /// Set when the shop uninstalled the app.
    #[serde(serialize_with = "iso_option::serialize")]
    pub uninstalled_at: Option<DateTime<Utc>>,
}

record!(Installation, "installations", {
    ID: Uuid = "id",
    APP_HANDLE: String = "app_handle",
    SHOP: String = "shop",
    SCOPES: String = "scopes",
    INSTALLED_AT: DateTime<Utc> = "installed_at",
    UNINSTALLED_AT: DateTime<Utc> = "uninstalled_at",
});

impl Installation {
    /// The installations of every shop and every Shopify app, with those a
    /// shop started without finishing and those it removed.
    pub fn all_shops(db: &PgPool) -> Query<'_, Self> {
        Query::new(db, None)
    }

    /// The installations in place: the shop has the app, and the application
    /// holds its access token.
    pub fn installed(db: &PgPool) -> Query<'_, Self> {
        Self::all_shops(db).filter(Condition::sql(
            "\"access_token\" IS NOT NULL AND \"uninstalled_at\" IS NULL",
        ))
    }
}

/// A run of a flow for a shop. `status` is `pending`, `sleeping`, `running`,
/// `completed`, `failed` or `canceled`.
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

record!(FlowRun, "flow_runs", {
    ID: Uuid = "id",
    APP_HANDLE: String = "app_handle",
    SHOP: String = "shop",
    FLOW_NAME: String = "flow_name",
    STATUS: String = "status",
    ERROR: String = "error",
    DEADLINE_AT: DateTime<Utc> = "deadline_at",
    PARENT_RUN_ID: String = "parent_run_id",
    ATTEMPTS: i32 = "attempts",
    AVAILABLE_AT: DateTime<Utc> = "available_at",
    WORKER_ID: String = "worker_id",
    STARTED_AT: DateTime<Utc> = "started_at",
    COMPLETED_AT: DateTime<Utc> = "completed_at",
    CREATED_AT: DateTime<Utc> = "created_at",
}, read: ["input", "retry_policy"]);

impl FlowRun {
    /// The runs of every shop and every Shopify app.
    pub fn all_shops(db: &PgPool) -> Query<'_, Self> {
        Query::new(db, None)
    }

    /// The runs of the shop and the Shopify app of a context: a flow, a
    /// webhook, an admin function…
    pub fn of(scope: &impl ShopScope) -> Query<'_, Self> {
        Query::new(scope.app().db(), Some(scope.shop().to_string()))
            .filter(Self::APP_HANDLE.eq(scope.shopify_app()))
    }
}

/// An attempt of a step of a run: its output when it completed, its error
/// when it failed.
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

record!(StepResult, "step_results", {
    ID: Uuid = "id",
    STEP_NAME: String = "step_name",
    ATTEMPT: i32 = "attempt",
    STATUS: String = "status",
    ERROR: String = "error",
    DURATION_MS: i32 = "duration_ms",
    CREATED_AT: DateTime<Utc> = "created_at",
    FLOW_RUN_ID: String = "flow_run_id",
}, read: ["output"]);

impl StepResult {
    /// The step attempts of a run. A run id that comes from a request is not
    /// proof of its shop: find the run first, with `FlowRun::of(&ctx)`.
    pub fn of_run(db: &PgPool, run: Uuid) -> Query<'_, Self> {
        Query::new(db, None).filter(Self::FLOW_RUN_ID.eq(run.to_string()))
    }
}

/// `Debug` does not show the configuration, which holds the credentials of the provider.
#[derive(Clone, PartialEq, Serialize, FromRow)]
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

impl std::fmt::Debug for ProviderConfigRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderConfigRow")
            .field("id", &self.id)
            .field("app_handle", &self.app_handle)
            .field("shop", &self.shop)
            .field("provider_name", &self.provider_name)
            .field("config", &self.config.as_ref().map(|_| "[redacted]"))
            .field("last_checked_at", &self.last_checked_at)
            .field("last_check_ok", &self.last_check_ok)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

/// A line a flow wrote to its logger.
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

record!(LogRow, "logs", {
    ID: Uuid = "id",
    FLOW_RUN_ID: String = "flow_run_id",
    LEVEL: String = "level",
    MESSAGE: String = "message",
    CREATED_AT: DateTime<Utc> = "created_at",
}, read: ["payload"]);

impl LogRow {
    /// The log lines of a run. A run id that comes from a request is not
    /// proof of its shop: find the run first, with `FlowRun::of(&ctx)`.
    pub fn of_run(db: &PgPool, run: Uuid) -> Query<'_, Self> {
        Query::new(db, None).filter(Self::FLOW_RUN_ID.eq(run.to_string()))
    }
}

/// `Debug` does not show the hash of the token.
#[derive(Clone, PartialEq, Serialize, FromRow)]
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

impl std::fmt::Debug for McpToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpToken")
            .field("id", &self.id)
            .field("app_handle", &self.app_handle)
            .field("shop", &self.shop)
            .field("name", &self.name)
            .field("token_id", &self.token_id)
            .field("token_hash", &crate::runtime::redact::redacted(&self.token_hash))
            .field("token_fingerprint", &self.token_fingerprint)
            .field("status", &self.status)
            .field("expires_at", &self.expires_at)
            .field("revoked_at", &self.revoked_at)
            .field("last_used_at", &self.last_used_at)
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .finish()
    }
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
