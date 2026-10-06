//! Queueing a flow run.

use anyhow::{Result, bail};
use chrono::{Duration, Utc};
use serde::Serialize;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use super::backoff::resolve_retry_policy;
use super::errors::FlowConcurrencyError;
use crate::app::OpenShopConfig;
use crate::server::shopify_apps::DEFAULT_SHOPIFY_APP_HANDLE;
use crate::types::{Concurrency, DispatchOptions, JsonMap};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DispatchFlowParams {
    pub flow_name: String,
    /// Defaults to `{}`.
    pub input: Option<Value>,
    pub shop: String,
    /// Defaults to the single-app handle `default`.
    pub shopify_app: Option<String>,
    pub parent_run_id: Option<String>,
    pub options: Option<DispatchOptions>,
}

impl DispatchFlowParams {
    pub fn new(flow_name: impl Into<String>, shop: impl Into<String>) -> Self {
        Self {
            flow_name: flow_name.into(),
            shop: shop.into(),
            ..Default::default()
        }
    }

    pub fn input(mut self, input: Value) -> Self {
        self.input = Some(input);
        self
    }

    pub fn shopify_app(mut self, handle: impl Into<String>) -> Self {
        self.shopify_app = Some(handle.into());
        self
    }

    pub fn parent_run_id(mut self, run_id: impl Into<String>) -> Self {
        self.parent_run_id = Some(run_id.into());
        self
    }

    pub fn options(mut self, options: DispatchOptions) -> Self {
        self.options = Some(options);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchFlowResult {
    pub run_id: Uuid,
    pub status: &'static str,
}

pub(crate) fn unknown_flow(config: &OpenShopConfig, flow_name: &str) -> anyhow::Error {
    let available: Vec<&str> = config.flows.keys().map(String::as_str).collect();
    anyhow::anyhow!("Flow \"{flow_name}\" not found. Available: {}", available.join(", "))
}

/// Inserts a `pending` run. Concurrency is decided under an advisory lock scoped
/// to `(app, shop, flow)` so that simultaneous dispatches cannot both pass.
pub async fn dispatch_flow(
    db: &PgPool,
    config: &OpenShopConfig,
    params: DispatchFlowParams,
) -> Result<DispatchFlowResult> {
    let DispatchFlowParams {
        flow_name,
        input,
        shop,
        shopify_app,
        parent_run_id,
        options,
    } = params;
    let shopify_app = shopify_app.unwrap_or_else(|| DEFAULT_SHOPIFY_APP_HANDLE.to_string());
    let input = input.unwrap_or_else(|| Value::Object(JsonMap::new()));
    let options = options.unwrap_or_default();

    let Some(flow) = config.flows.get(&flow_name) else {
        return Err(unknown_flow(config, &flow_name));
    };

    let mut tx = db.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
        .bind(format!("{shopify_app}:{shop}:{flow_name}"))
        .execute(&mut *tx)
        .await?;

    if flow.concurrency.unwrap_or_default() == Concurrency::Reject {
        let existing: Option<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM flow_runs \
             WHERE app_handle = $1 AND shop = $2 AND flow_name = $3 \
               AND status IN ('pending', 'running', 'sleeping') \
             LIMIT 1",
        )
        .bind(&shopify_app)
        .bind(&shop)
        .bind(&flow_name)
        .fetch_optional(&mut *tx)
        .await?;

        if let Some((existing_run_id,)) = existing {
            bail!(FlowConcurrencyError {
                flow_name,
                shop,
                existing_run_id: existing_run_id.to_string(),
            });
        }
    }

    let now = Utc::now();
    let deadline_at = flow
        .timeout
        .map(|timeout| now + Duration::milliseconds(timeout.as_millis() as i64));
    let retry_policy =
        resolve_retry_policy(&[Some(config.retry_policy), Some(flow.retry_policy), options.retry_policy]);
    let available_at = now + Duration::milliseconds(options.delay_ms.unwrap_or(0) as i64);

    let (run_id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO flow_runs \
           (app_handle, shop, flow_name, status, input, deadline_at, parent_run_id, retry_policy, available_at) \
         VALUES ($1, $2, $3, 'pending', $4::json, $5, $6, $7::json, $8) \
         RETURNING id",
    )
    .bind(&shopify_app)
    .bind(&shop)
    .bind(&flow_name)
    .bind(input.to_string())
    .bind(deadline_at)
    .bind(parent_run_id)
    .bind(serde_json::to_string(&retry_policy)?)
    .bind(available_at)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(DispatchFlowResult {
        run_id,
        status: "pending",
    })
}
