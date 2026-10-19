//! Filtering and exporting the logs of a run.

use std::collections::HashSet;

use anyhow::Result;
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use serde_json::Value;
use sqlx::PgPool;

use crate::db::schema::LogRow;
use crate::runtime::time::to_iso;
use crate::server::log_query::{apply_context_expansion, prepare_log_query};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LogResponseRow {
    #[serde(flatten)]
    pub log: LogRow,
    /// Set when context lines are included, to tell matches from their neighbours.
    #[serde(rename = "_matched", skip_serializing_if = "Option::is_none")]
    pub matched: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FilteredRunLogs {
    pub logs: Vec<LogResponseRow>,
    /// Number of log lines of the run before filtering.
    pub total: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct RunLogOptions<'a> {
    pub query: &'a str,
    /// Comma-separated levels, e.g. `info,warn,error`.
    pub levels: &'a str,
    pub include_context: bool,
}

pub async fn get_filtered_run_logs(
    db: &PgPool,
    flow_run_id: &str,
    options: RunLogOptions<'_>,
) -> Result<FilteredRunLogs> {
    // Before reading anything: a query the engine refuses is the caller's mistake.
    let (parsed, matcher) = prepare_log_query(options.query)?;

    let all_logs: Vec<LogRow> = sqlx::query_as(
        "SELECT id, flow_run_id, level, message, payload, created_at FROM logs \
         WHERE flow_run_id = $1 ORDER BY created_at DESC",
    )
    .bind(flow_run_id)
    .fetch_all(db)
    .await?;
    let total = all_logs.len();

    let active_levels: HashSet<&str> = options.levels.split(',').filter(|level| !level.is_empty()).collect();

    let filtered: Vec<LogRow> = all_logs
        .into_iter()
        .filter(|log| active_levels.contains(log.level.as_str()))
        .filter(|log| parsed.time.from.is_none_or(|from| log.created_at >= from))
        .filter(|log| parsed.time.to.is_none_or(|to| log.created_at <= to))
        .collect();

    let plain = |log: LogRow| LogResponseRow { log, matched: None };

    if parsed.filters.is_empty() {
        return Ok(FilteredRunLogs {
            logs: filtered.into_iter().map(plain).collect(),
            total,
        });
    }

    let ids: Vec<String> = filtered.iter().map(|log| log.id.to_string()).collect();
    let matched: HashSet<&str> = filtered
        .iter()
        .zip(&ids)
        .filter(|(log, _)| matcher.matches(log.message.as_deref(), log.payload.as_ref()))
        .map(|(_, id)| id.as_str())
        .collect();

    if !options.include_context {
        let logs = filtered
            .iter()
            .zip(&ids)
            .filter(|(_, id)| matched.contains(id.as_str()))
            .map(|(log, _)| plain(log.clone()))
            .collect();
        return Ok(FilteredRunLogs { logs, total });
    }

    let id_refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    let visible = apply_context_expansion(&id_refs, &matched, parsed.context);
    let logs = filtered
        .iter()
        .zip(&ids)
        .filter(|(_, id)| visible.contains(id.as_str()))
        .map(|(log, id)| LogResponseRow {
            log: log.clone(),
            matched: Some(matched.contains(id.as_str())),
        })
        .collect();
    Ok(FilteredRunLogs { logs, total })
}

fn csv_escape(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

pub fn logs_to_csv(logs: &[LogResponseRow]) -> String {
    let mut lines = vec!["id,level,message,payload,createdAt".to_string()];
    for row in logs {
        let log = &row.log;
        let payload = match &log.payload {
            Some(payload) if !payload.is_null() => payload.to_string(),
            _ => Value::from("").to_string(),
        };
        lines.push(
            [
                log.id.to_string(),
                log.level.clone(),
                csv_escape(log.message.as_deref().unwrap_or_default()),
                csv_escape(&payload),
                to_iso(&log.created_at),
            ]
            .join(","),
        );
    }
    lines.join("\n")
}

/// `format` is `csv` or `json`; anything else is exported as JSON.
pub fn create_log_export_response(run_id: &str, format: &str, logs: &[LogResponseRow]) -> Response {
    let extension = if !format.is_empty() && format.chars().all(|character| character.is_ascii_alphanumeric()) {
        format
    } else {
        "json"
    };
    let disposition = format!("attachment; filename=\"run-{run_id}-logs.{extension}\"");

    if format == "csv" {
        return (
            [
                (CONTENT_TYPE, "text/csv".to_string()),
                (CONTENT_DISPOSITION, disposition),
            ],
            logs_to_csv(logs),
        )
            .into_response();
    }

    (
        [
            (CONTENT_TYPE, "application/json".to_string()),
            (CONTENT_DISPOSITION, disposition),
        ],
        serde_json::to_string_pretty(logs).unwrap_or_else(|_| "[]".to_string()),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use serde_json::json;
    use uuid::Uuid;

    fn row(message: Option<&str>, payload: Option<Value>) -> LogResponseRow {
        LogResponseRow {
            log: LogRow {
                id: Uuid::nil(),
                flow_run_id: Some("run".into()),
                level: "info".into(),
                message: message.map(str::to_string),
                payload,
                created_at: Utc.with_ymd_and_hms(2026, 3, 1, 10, 0, 0).unwrap(),
            },
            matched: None,
        }
    }

    #[test]
    fn exports_csv_with_escaped_cells() {
        let csv = logs_to_csv(&[row(Some("He said \"hi\""), Some(json!({ "a": 1 }))), row(None, None)]);
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines[0], "id,level,message,payload,createdAt");
        assert_eq!(
            lines[1],
            "00000000-0000-0000-0000-000000000000,info,\"He said \"\"hi\"\"\",\"{\"\"a\"\":1}\",2026-03-01T10:00:00.000Z"
        );
        assert_eq!(
            lines[2],
            "00000000-0000-0000-0000-000000000000,info,\"\",\"\"\"\"\"\",2026-03-01T10:00:00.000Z"
        );
    }

    #[test]
    fn serializes_matched_flag_only_when_present() {
        let plain = serde_json::to_value(row(Some("a"), None)).unwrap();
        assert!(plain.get("_matched").is_none());
        assert_eq!(plain["flowRunId"], "run");
        assert_eq!(plain["createdAt"], "2026-03-01T10:00:00.000Z");

        let mut matched = row(Some("a"), None);
        matched.matched = Some(false);
        assert_eq!(serde_json::to_value(matched).unwrap()["_matched"], false);
    }

    #[test]
    fn export_response_sets_download_headers() {
        let response = create_log_export_response("abc", "csv", &[]);
        assert_eq!(response.headers()[CONTENT_TYPE], "text/csv");
        assert_eq!(
            response.headers()[CONTENT_DISPOSITION],
            "attachment; filename=\"run-abc-logs.csv\""
        );

        let response = create_log_export_response("abc", "x\"y", &[]);
        assert_eq!(response.headers()[CONTENT_TYPE], "application/json");
        assert_eq!(
            response.headers()[CONTENT_DISPOSITION],
            "attachment; filename=\"run-abc-logs.json\""
        );
    }
}
