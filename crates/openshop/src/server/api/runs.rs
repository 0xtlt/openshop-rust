use axum::Router;
use axum::body::Bytes;
use axum::extract::{Extension, Path, RawQuery, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{get, post};
use chrono::{DateTime, Duration, Utc};
use indexmap::IndexMap;
use serde_json::{Value, json};
use sqlx::{Postgres, QueryBuilder};
use uuid::Uuid;

use super::run_logs::{
    DEFAULT_LIMIT, LogCursor, MAX_LIMIT, RunLogOptions, create_log_export_response, get_filtered_run_logs,
};
use crate::app::OpenShop;
use crate::db::schema::{FlowRun, StepResult};
use crate::engine::abort::cancel_run;
use crate::engine::dispatch::{DispatchOptions, lock_flow, reject_when_active};
use crate::engine::errors::{FlowConcurrencyError, InvalidFlowInput, UnknownFlowError};
use crate::runtime::time::parse_date;
use crate::server::http::{ApiResult, containing, json_error, json_object_body, json_response, parse_query};
use crate::server::log_query::InvalidLogQuery;
use crate::server::shop::ShopSession;
use crate::types::{ACTIVE_RUN_STATUSES, ALL_RUN_STATUSES, Concurrency};

/// The deepest page a list can be asked for. A deeper `OFFSET` makes the
/// database read and drop every row before it; narrow the filters instead.
const MAX_OFFSET: i64 = 10_000;

/// `Number(value) || fallback`, floored.
fn number_param(query: &IndexMap<String, String>, name: &str, fallback: i64) -> i64 {
    query
        .get(name)
        .and_then(|value| value.trim().parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value != 0.0)
        .map(|value| value.floor() as i64)
        .unwrap_or(fallback)
}

/// The runs of the session's shop and app, newest first, as `query` asks:
/// `limit` (50, at most 200), `offset`, `search` (in the flow, the status
/// and the id), `status`, `from` and `to`; those of `flow_name` only when it
/// is given. The inner `Err` is a query that cannot be answered, said for the
/// merchant (`offset must be at most 10000`).
pub(crate) async fn query_runs(
    app: &OpenShop,
    session: &ShopSession,
    flow_name: Option<&str>,
    query: &IndexMap<String, String>,
) -> Result<Result<Vec<FlowRun>, String>, sqlx::Error> {
    let limit = number_param(query, "limit", 50).clamp(1, 200);
    let offset = number_param(query, "offset", 0).max(0);
    if offset > MAX_OFFSET {
        return Ok(Err(format!("offset must be at most {MAX_OFFSET}")));
    }
    let param = |name: &str| query.get(name).filter(|value| !value.is_empty());

    let mut sql: QueryBuilder<Postgres> = QueryBuilder::new("SELECT * FROM flow_runs WHERE app_handle = ");
    sql.push_bind(&session.shopify_app);
    sql.push(" AND shop = ").push_bind(&session.shop);
    if let Some(flow_name) = flow_name {
        sql.push(" AND flow_name = ").push_bind(flow_name);
    }
    if let Some(search) = param("search") {
        let pattern = containing(search);
        sql.push(" AND (flow_name ILIKE ").push_bind(pattern.clone());
        sql.push(" ESCAPE '\\' OR status ILIKE ").push_bind(pattern.clone());
        sql.push(" ESCAPE '\\' OR id::text ILIKE ").push_bind(pattern);
        sql.push(" ESCAPE '\\')");
    }
    if let Some(status) = param("status").filter(|status| ALL_RUN_STATUSES.contains(&status.as_str())) {
        sql.push(" AND status = ").push_bind(status);
    }
    if let Some(from) = param("from").and_then(|value| parse_date(value)) {
        sql.push(" AND created_at >= ").push_bind(from);
    }
    if let Some(to) = param("to").and_then(|value| parse_date(value)) {
        sql.push(" AND created_at <= ").push_bind(to);
    }
    sql.push(" ORDER BY created_at DESC LIMIT ").push_bind(limit);
    sql.push(" OFFSET ").push_bind(offset);

    Ok(Ok(sql.build_query_as().fetch_all(app.db()).await?))
}

async fn list_runs_for(
    app: &OpenShop,
    session: &ShopSession,
    flow_name: Option<&str>,
    query: &IndexMap<String, String>,
) -> ApiResult {
    Ok(match query_runs(app, session, flow_name, query).await? {
        Ok(runs) => json_response(StatusCode::OK, json!(runs)),
        Err(refused) => json_error(StatusCode::BAD_REQUEST, refused),
    })
}

async fn list_runs(
    State(app): State<OpenShop>,
    Extension(session): Extension<ShopSession>,
    RawQuery(raw_query): RawQuery,
) -> ApiResult {
    list_runs_for(&app, &session, None, &parse_query(raw_query.as_deref())).await
}

async fn list_flow_runs(
    State(app): State<OpenShop>,
    Extension(session): Extension<ShopSession>,
    Path(name): Path<String>,
    RawQuery(raw_query): RawQuery,
) -> ApiResult {
    list_runs_for(&app, &session, Some(&name), &parse_query(raw_query.as_deref())).await
}

/// The run when it exists and belongs to the session's shop and app.
pub(crate) async fn find_run(app: &OpenShop, session: &ShopSession, id: &str) -> Result<Option<FlowRun>, sqlx::Error> {
    let Ok(id) = Uuid::parse_str(id) else {
        return Ok(None);
    };
    sqlx::query_as("SELECT * FROM flow_runs WHERE id = $1 AND app_handle = $2 AND shop = $3 LIMIT 1")
        .bind(id)
        .bind(&session.shopify_app)
        .bind(&session.shop)
        .fetch_optional(app.db())
        .await
}

fn run_not_found() -> axum::response::Response {
    json_error(StatusCode::NOT_FOUND, "Run not found")
}

async fn get_run(
    State(app): State<OpenShop>,
    Extension(session): Extension<ShopSession>,
    Path(id): Path<String>,
) -> ApiResult {
    let Some(run) = find_run(&app, &session, &id).await? else {
        return Ok(run_not_found());
    };
    let steps: Vec<StepResult> =
        sqlx::query_as("SELECT * FROM step_results WHERE flow_run_id = $1 ORDER BY created_at ASC")
            .bind(run.id.to_string())
            .fetch_all(app.db())
            .await?;

    let mut body = serde_json::to_value(&run)?;
    body["steps"] = json!(steps);
    Ok(json_response(StatusCode::OK, body))
}

async fn run_logs(
    State(app): State<OpenShop>,
    Extension(session): Extension<ShopSession>,
    Path(id): Path<String>,
    RawQuery(raw_query): RawQuery,
) -> ApiResult {
    let query = parse_query(raw_query.as_deref());
    let Some(run) = find_run(&app, &session, &id).await? else {
        return Ok(run_not_found());
    };
    let cursor = match query.get("cursor").filter(|cursor| !cursor.is_empty()) {
        Some(cursor) => match LogCursor::parse(cursor) {
            Some(cursor) => Some(cursor),
            None => return Ok(json_error(StatusCode::BAD_REQUEST, "Invalid cursor")),
        },
        None => None,
    };
    let limit = number_param(&query, "limit", DEFAULT_LIMIT as i64).clamp(1, MAX_LIMIT as i64);
    let logs = get_filtered_run_logs(
        app.db(),
        &run.id.to_string(),
        RunLogOptions {
            query: query.get("q").map(String::as_str).unwrap_or_default(),
            levels: query.get("levels").map(String::as_str).unwrap_or("info,warn,error"),
            include_context: true,
            limit: limit as usize,
            cursor,
        },
    )
    .await;
    match logs {
        Ok(logs) => Ok(json_response(StatusCode::OK, json!(logs))),
        Err(error) => log_query_failure(error),
    }
}

/// A query the engine refuses is a `400` with the reason; any other failure is
/// unexpected.
pub(super) fn log_query_failure(error: anyhow::Error) -> ApiResult {
    match error.downcast_ref::<InvalidLogQuery>() {
        Some(invalid) => Ok(json_error(StatusCode::BAD_REQUEST, invalid.to_string())),
        None => Err(error.into()),
    }
}

async fn export_run_logs(
    State(app): State<OpenShop>,
    Extension(session): Extension<ShopSession>,
    Path(id): Path<String>,
    RawQuery(raw_query): RawQuery,
) -> ApiResult {
    let query = parse_query(raw_query.as_deref());
    let Some(run) = find_run(&app, &session, &id).await? else {
        return Ok(run_not_found());
    };
    let format = query.get("format").map(String::as_str).unwrap_or("json");
    let response = create_log_export_response(
        app.db().clone(),
        &run.id.to_string(),
        format,
        query.get("q").map(String::as_str).unwrap_or_default(),
        query.get("levels").map(String::as_str).unwrap_or("info,warn,error"),
    );
    match response {
        Ok(response) => Ok(response),
        Err(invalid) => Ok(json_error(StatusCode::BAD_REQUEST, invalid.to_string())),
    }
}

async fn run_flow(
    State(app): State<OpenShop>,
    Extension(session): Extension<ShopSession>,
    Path(name): Path<String>,
    body: Bytes,
) -> ApiResult {
    let body = json_object_body(&body);
    let input = match body.get("input") {
        Some(Value::Null) | None => json!({}),
        Some(input) => input.clone(),
    };

    let dispatched = app
        .dispatch_flow(
            DispatchOptions::new(name, session.shop.clone())
                .input(input)
                .shopify_app(session.shopify_app.clone()),
        )
        .await;
    match dispatched {
        Ok(result) => Ok(json_response(StatusCode::ACCEPTED, json!(result))),
        Err(error) => {
            if let Some(conflict) = error.downcast_ref::<FlowConcurrencyError>() {
                Ok(concurrency_conflict(conflict))
            } else if let Some(unknown) = error.downcast_ref::<UnknownFlowError>() {
                Ok(json_error(
                    StatusCode::NOT_FOUND,
                    format!("Flow \"{}\" not found", unknown.flow_name),
                ))
            } else if let Some(invalid) = error.downcast_ref::<InvalidFlowInput>() {
                Ok(json_error(StatusCode::UNPROCESSABLE_ENTITY, invalid.to_string()))
            } else {
                Err(error.into())
            }
        }
    }
}

/// `409` for a flow that rejects concurrent runs, with the run that is active.
fn concurrency_conflict(conflict: &FlowConcurrencyError) -> Response {
    json_response(
        StatusCode::CONFLICT,
        json!({ "error": conflict.to_string(), "existingRunId": conflict.existing_run_id }),
    )
}

/// The answer to a transition that matched no row: the run is gone (`404`), or
/// it was not in a status the transition starts from (`409`, saying which). The
/// run is read again because it can have changed since the request read it.
async fn refused_transition(
    app: &OpenShop,
    session: &ShopSession,
    id: &str,
    allowed: &[&str],
    conflict: impl FnOnce(&FlowRun) -> String,
) -> ApiResult {
    let Some(run) = find_run(app, session, id).await? else {
        return Ok(run_not_found());
    };
    if allowed.contains(&run.status.as_str()) {
        return Ok(json_error(
            StatusCode::CONFLICT,
            "The run changed while the request was processed — try again",
        ));
    }
    Ok(json_error(StatusCode::CONFLICT, conflict(&run)))
}

/// Runs that are over, and so that can be retried.
const FINISHED_RUN_STATUSES: [&str; 3] = ["failed", "completed", "canceled"];

async fn retry_run(
    State(app): State<OpenShop>,
    Extension(session): Extension<ShopSession>,
    Path(id): Path<String>,
    RawQuery(raw_query): RawQuery,
) -> ApiResult {
    let query = parse_query(raw_query.as_deref());
    let mode = query.get("mode").map(String::as_str).unwrap_or("resume");
    let Some(run) = find_run(&app, &session, &id).await? else {
        return Ok(run_not_found());
    };

    let flow = app.config().flows.get(&run.flow_name);
    let deadline_at: Option<DateTime<Utc>> = flow
        .and_then(|flow| flow.timeout)
        .map(|timeout| Utc::now() + Duration::milliseconds(timeout.as_millis() as i64));
    let rejects_concurrency = flow.is_some_and(|flow| flow.concurrency.unwrap_or_default() == Concurrency::Reject);
    let run_id = run.id.to_string();
    let reset = mode == "reset";

    // Like a dispatch, under the lock of the flow for the shop: the status is
    // checked by the update itself, and a flow that rejects concurrent runs is
    // not given a second active one.
    let mut tx = app.db().begin().await?;
    lock_flow(&mut tx, &run.app_handle, &run.shop, &run.flow_name).await?;
    let retried = sqlx::query(
        "UPDATE flow_runs SET \
           status = 'pending', error = NULL, available_at = now(), completed_at = NULL, worker_id = NULL, \
           lease_id = NULL, \
           attempts = 0, deadline_at = $2, started_at = NULL, retry_policy = NULL \
         WHERE id = $1 AND app_handle = $4 AND shop = $5 AND status = ANY($3)",
    )
    .bind(run.id)
    .bind(deadline_at)
    .bind(FINISHED_RUN_STATUSES.as_slice())
    .bind(&run.app_handle)
    .bind(&run.shop)
    .execute(&mut *tx)
    .await?;
    if retried.rows_affected() == 0 {
        tx.rollback().await?;
        return refused_transition(&app, &session, &id, &FINISHED_RUN_STATUSES, |run| {
            format!("Cannot retry a run with status \"{}\"", run.status)
        })
        .await;
    }
    if rejects_concurrency
        && let Err(error) = reject_when_active(&mut tx, &run.app_handle, &run.shop, &run.flow_name, Some(run.id)).await
    {
        return match error.downcast_ref::<FlowConcurrencyError>() {
            Some(conflict) => Ok(concurrency_conflict(conflict)),
            None => Err(error.into()),
        };
    }

    if reset {
        sqlx::query("DELETE FROM step_results WHERE flow_run_id = $1")
            .bind(&run_id)
            .execute(&mut *tx)
            .await?;
    } else {
        sqlx::query("DELETE FROM step_results WHERE flow_run_id = $1 AND status != 'completed'")
            .bind(&run_id)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("INSERT INTO logs (flow_run_id, level, message, payload) VALUES ($1, 'info', $2, $3::json)")
        .bind(&run_id)
        .bind(format!(
            "Run retried ({})",
            if reset {
                "restart — all steps discarded"
            } else {
                "resume — completed steps kept"
            }
        ))
        .bind(json!({ "mode": mode, "flowName": run.flow_name }).to_string())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    Ok(json_response(
        StatusCode::OK,
        json!({ "ok": true, "runId": run_id, "mode": mode }),
    ))
}

async fn cancel(
    State(app): State<OpenShop>,
    Extension(session): Extension<ShopSession>,
    Path(id): Path<String>,
) -> ApiResult {
    let Ok(run_id) = Uuid::parse_str(&id) else {
        return Ok(run_not_found());
    };
    // The status is checked by the update itself: a run that finished since it
    // was read is not overwritten.
    let mut tx = app.db().begin().await?;
    let canceled = sqlx::query(
        "UPDATE flow_runs SET status = 'canceled', completed_at = now(), \
           worker_id = NULL, lease_id = NULL, available_at = NULL \
         WHERE id = $1 AND app_handle = $2 AND shop = $3 AND status = ANY($4)",
    )
    .bind(run_id)
    .bind(&session.shopify_app)
    .bind(&session.shop)
    .bind(ACTIVE_RUN_STATUSES.as_slice())
    .execute(&mut *tx)
    .await?;
    if canceled.rows_affected() == 0 {
        tx.rollback().await?;
        return refused_transition(&app, &session, &id, &ACTIVE_RUN_STATUSES, |run| {
            format!("Cannot cancel a run with status \"{}\"", run.status)
        })
        .await;
    }

    // The worker that executes the run, when it is this process, is told while
    // the row is still locked: a retry waits for the commit, so no new execution
    // of the run can register itself before this signal and receive it. The
    // writes of the worker are refused once the run is canceled.
    cancel_run(&run_id.to_string());
    tx.commit().await?;
    Ok(json_response(StatusCode::OK, json!({ "ok": true })))
}

/// Deletes the runs of the session that are over, with their steps and logs, and
/// returns the ones that were deleted. A run is deleted only while it is not
/// active, whatever its status was when the caller read it.
pub(crate) async fn delete_runs(app: &OpenShop, session: &ShopSession, ids: &[Uuid]) -> Result<Vec<Uuid>, sqlx::Error> {
    let mut tx = app.db().begin().await?;
    let deleted: Vec<(Uuid,)> = sqlx::query_as(
        "DELETE FROM flow_runs \
         WHERE id = ANY($1) AND app_handle = $2 AND shop = $3 AND status != ALL($4) \
         RETURNING id",
    )
    .bind(ids)
    .bind(&session.shopify_app)
    .bind(&session.shop)
    .bind(ACTIVE_RUN_STATUSES.as_slice())
    .fetch_all(&mut *tx)
    .await?;
    let deleted: Vec<Uuid> = deleted.into_iter().map(|(id,)| id).collect();

    let text_ids: Vec<String> = deleted.iter().map(Uuid::to_string).collect();
    sqlx::query("DELETE FROM logs WHERE flow_run_id = ANY($1)")
        .bind(&text_ids)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM step_results WHERE flow_run_id = ANY($1)")
        .bind(&text_ids)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(deleted)
}

async fn delete_run(
    State(app): State<OpenShop>,
    Extension(session): Extension<ShopSession>,
    Path(id): Path<String>,
) -> ApiResult {
    let Ok(run_id) = Uuid::parse_str(&id) else {
        return Ok(run_not_found());
    };
    if delete_runs(&app, &session, &[run_id]).await?.is_empty() {
        return refused_transition(&app, &session, &id, &FINISHED_RUN_STATUSES, |_| {
            "Cannot delete an active run — cancel it first".to_string()
        })
        .await;
    }
    Ok(json_response(StatusCode::OK, json!({ "ok": true })))
}

async fn bulk_delete(
    State(app): State<OpenShop>,
    Extension(session): Extension<ShopSession>,
    body: Bytes,
) -> ApiResult {
    let body = json_object_body(&body);
    let Some(ids) = body.get("ids").and_then(Value::as_array).filter(|ids| !ids.is_empty()) else {
        return Ok(json_error(StatusCode::BAD_REQUEST, "No IDs provided"));
    };
    if ids.len() > 100 {
        return Ok(json_error(StatusCode::BAD_REQUEST, "Max 100 runs per batch"));
    }

    let candidates: Vec<Uuid> = ids.iter().filter_map(|id| Uuid::parse_str(id.as_str()?).ok()).collect();
    let deleted = delete_runs(&app, &session, &candidates).await?;
    Ok(json_response(
        StatusCode::OK,
        json!({ "deleted": deleted.len(), "skipped": ids.len() - deleted.len() }),
    ))
}

pub fn routes() -> Router<OpenShop> {
    Router::new()
        .route("/api/runs", get(list_runs))
        .route("/api/runs/bulk-delete", post(bulk_delete))
        .route("/api/runs/{id}", get(get_run).delete(delete_run))
        .route("/api/runs/{id}/logs", get(run_logs))
        .route("/api/runs/{id}/logs/export", get(export_run_logs))
        .route("/api/runs/{id}/retry", post(retry_run))
        .route("/api/runs/{id}/cancel", post(cancel))
        .route("/api/flows/{name}/runs", get(list_flow_runs))
        .route("/api/flows/{name}/run", post(run_flow))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_numeric_query_params_like_javascript() {
        let query = |value: &str| IndexMap::from([("limit".to_string(), value.to_string())]);
        assert_eq!(number_param(&query("25"), "limit", 50), 25);
        assert_eq!(number_param(&query("10.9"), "limit", 50), 10);
        assert_eq!(number_param(&query("abc"), "limit", 50), 50);
        assert_eq!(number_param(&query("0"), "limit", 50), 50);
        assert_eq!(number_param(&query("-5"), "limit", 50), -5);
        assert_eq!(number_param(&IndexMap::new(), "limit", 50), 50);
    }
}
