use axum::Router;
use axum::body::Bytes;
use axum::extract::{Extension, Path, RawQuery, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use chrono::{DateTime, Duration, Utc};
use indexmap::IndexMap;
use serde_json::{Value, json};
use sqlx::{Postgres, QueryBuilder};
use uuid::Uuid;

use super::run_logs::{RunLogOptions, create_log_export_response, get_filtered_run_logs};
use crate::app::OpenShop;
use crate::db::schema::{FlowRun, StepResult};
use crate::engine::abort::cancel_run;
use crate::engine::dispatch::DispatchOptions;
use crate::engine::errors::FlowConcurrencyError;
use crate::runtime::time::parse_date;
use crate::server::http::{ApiResult, json_error, json_object_body, json_response, parse_query};
use crate::server::log_query::InvalidLogQuery;
use crate::server::shop::ShopSession;
use crate::types::{ACTIVE_RUN_STATUSES, ALL_RUN_STATUSES};

/// `Number(value) || fallback`, floored.
fn number_param(query: &IndexMap<String, String>, name: &str, fallback: i64) -> i64 {
    query
        .get(name)
        .and_then(|value| value.trim().parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value != 0.0)
        .map(|value| value.floor() as i64)
        .unwrap_or(fallback)
}

async fn list_runs_for(
    app: &OpenShop,
    session: &ShopSession,
    flow_name: Option<&str>,
    query: &IndexMap<String, String>,
) -> ApiResult {
    let limit = number_param(query, "limit", 50).clamp(1, 200);
    let offset = number_param(query, "offset", 0).max(0);
    let param = |name: &str| query.get(name).filter(|value| !value.is_empty());

    let mut sql: QueryBuilder<'_, Postgres> = QueryBuilder::new("SELECT * FROM flow_runs WHERE app_handle = ");
    sql.push_bind(&session.shopify_app);
    sql.push(" AND shop = ").push_bind(&session.shop);
    if let Some(flow_name) = flow_name {
        sql.push(" AND flow_name = ").push_bind(flow_name);
    }
    if let Some(search) = param("search") {
        let pattern = format!("%{search}%");
        sql.push(" AND (flow_name ILIKE ").push_bind(pattern.clone());
        sql.push(" OR status ILIKE ").push_bind(pattern.clone());
        sql.push(" OR id::text ILIKE ").push_bind(pattern).push(")");
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

    let runs: Vec<FlowRun> = sql.build_query_as().fetch_all(app.db()).await?;
    Ok(json_response(StatusCode::OK, json!(runs)))
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
async fn find_run(app: &OpenShop, session: &ShopSession, id: &str) -> Result<Option<FlowRun>, sqlx::Error> {
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
    let logs = get_filtered_run_logs(
        app.db(),
        &run.id.to_string(),
        RunLogOptions {
            query: query.get("q").map(String::as_str).unwrap_or_default(),
            levels: query.get("levels").map(String::as_str).unwrap_or("info,warn,error"),
            include_context: true,
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
    let run_id = run.id.to_string();
    let result = get_filtered_run_logs(
        app.db(),
        &run_id,
        RunLogOptions {
            query: query.get("q").map(String::as_str).unwrap_or_default(),
            levels: query.get("levels").map(String::as_str).unwrap_or("info,warn,error"),
            include_context: false,
        },
    )
    .await;
    let result = match result {
        Ok(result) => result,
        Err(error) => return log_query_failure(error),
    };
    let format = query.get("format").map(String::as_str).unwrap_or("json");
    Ok(create_log_export_response(&run_id, format, &result.logs))
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

    // The caller's mistakes, before a run is queued: a run that could only fail
    // would otherwise be accepted.
    let Some(flow) = app.config().flows.get(&name) else {
        return Ok(json_error(StatusCode::NOT_FOUND, format!("Flow \"{name}\" not found")));
    };
    if let Some(schema) = &flow.input
        && let Err(summary) = schema.parse(input.clone())
    {
        return Ok(json_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("Invalid input for flow \"{name}\": {summary}"),
        ));
    }

    let dispatched = app
        .dispatch_flow(
            DispatchOptions::new(name, session.shop.clone())
                .input(input)
                .shopify_app(session.shopify_app.clone()),
        )
        .await;
    match dispatched {
        Ok(result) => Ok(json_response(StatusCode::ACCEPTED, json!(result))),
        Err(error) => match error.downcast_ref::<FlowConcurrencyError>() {
            Some(conflict) => Ok(json_response(
                StatusCode::CONFLICT,
                json!({ "error": conflict.to_string(), "existingRunId": conflict.existing_run_id }),
            )),
            None => Err(error.into()),
        },
    }
}

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
    if !["failed", "completed", "canceled"].contains(&run.status.as_str()) {
        return Ok(json_error(
            StatusCode::CONFLICT,
            format!("Cannot retry a run with status \"{}\"", run.status),
        ));
    }

    let deadline_at: Option<DateTime<Utc>> = app
        .config()
        .flows
        .get(&run.flow_name)
        .and_then(|flow| flow.timeout)
        .map(|timeout| Utc::now() + Duration::milliseconds(timeout.as_millis() as i64));
    let run_id = run.id.to_string();
    let reset = mode == "reset";

    let mut tx = app.db().begin().await?;
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
    sqlx::query(
        "UPDATE flow_runs SET \
           status = 'pending', error = NULL, available_at = now(), completed_at = NULL, worker_id = NULL, \
           attempts = 0, deadline_at = $2, started_at = NULL, retry_policy = NULL \
         WHERE id = $1",
    )
    .bind(run.id)
    .bind(deadline_at)
    .execute(&mut *tx)
    .await?;
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
    let Some(run) = find_run(&app, &session, &id).await? else {
        return Ok(run_not_found());
    };
    if !ACTIVE_RUN_STATUSES.contains(&run.status.as_str()) {
        return Ok(json_error(
            StatusCode::CONFLICT,
            format!("Cannot cancel a run with status \"{}\"", run.status),
        ));
    }

    cancel_run(&run.id.to_string());
    sqlx::query("UPDATE flow_runs SET status = 'canceled', completed_at = now() WHERE id = $1")
        .bind(run.id)
        .execute(app.db())
        .await?;
    Ok(json_response(StatusCode::OK, json!({ "ok": true })))
}

async fn delete_runs(app: &OpenShop, ids: &[Uuid]) -> Result<(), sqlx::Error> {
    let text_ids: Vec<String> = ids.iter().map(Uuid::to_string).collect();
    let mut tx = app.db().begin().await?;
    sqlx::query("DELETE FROM logs WHERE flow_run_id = ANY($1)")
        .bind(&text_ids)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM step_results WHERE flow_run_id = ANY($1)")
        .bind(&text_ids)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM flow_runs WHERE id = ANY($1)")
        .bind(ids)
        .execute(&mut *tx)
        .await?;
    tx.commit().await
}

async fn delete_run(
    State(app): State<OpenShop>,
    Extension(session): Extension<ShopSession>,
    Path(id): Path<String>,
) -> ApiResult {
    let Some(run) = find_run(&app, &session, &id).await? else {
        return Ok(run_not_found());
    };
    if ACTIVE_RUN_STATUSES.contains(&run.status.as_str()) {
        return Ok(json_error(
            StatusCode::CONFLICT,
            "Cannot delete an active run — cancel it first",
        ));
    }
    delete_runs(&app, &[run.id]).await?;
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
    let to_delete: Vec<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM flow_runs \
         WHERE app_handle = $1 AND shop = $2 AND id = ANY($3) \
           AND status NOT IN ('running', 'pending', 'sleeping')",
    )
    .bind(&session.shopify_app)
    .bind(&session.shop)
    .bind(&candidates)
    .fetch_all(app.db())
    .await?;

    let delete_ids: Vec<Uuid> = to_delete.into_iter().map(|(id,)| id).collect();
    if delete_ids.is_empty() {
        return Ok(json_response(
            StatusCode::OK,
            json!({ "deleted": 0, "skipped": ids.len() }),
        ));
    }
    delete_runs(&app, &delete_ids).await?;
    Ok(json_response(
        StatusCode::OK,
        json!({ "deleted": delete_ids.len(), "skipped": ids.len() - delete_ids.len() }),
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
