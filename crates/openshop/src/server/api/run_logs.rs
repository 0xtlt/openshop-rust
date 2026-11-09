//! Reading and exporting the logs of a run, a page or a chunk at a time.
//!
//! Levels and the time range are restricted by the database. The text filters
//! (`|=`, `!=`, `|~`, free text) run on the lines as they are read, so a
//! request holds one chunk of a run at a time and not the run. Lines are
//! ordered newest first, by `(created_at, id)`, which is what the cursors of
//! the pages follow.

use std::collections::{BTreeMap, VecDeque};

use anyhow::Result;
use axum::body::{Body, Bytes};
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::response::Response;
use chrono::{DateTime, Utc};
use futures::stream::{self, Stream};
use serde::Serialize;
use serde_json::Value;
use sqlx::{PgPool, Postgres, QueryBuilder};
use uuid::Uuid;

use crate::db::schema::LogRow;
use crate::runtime::time::to_iso;
use crate::server::log_query::{InvalidLogQuery, LogMatcher, ParsedLogQuery, prepare_log_query};

/// Lines read from the database at a time.
const CHUNK: i64 = 500;
/// Lines of a page when the request names no limit.
pub const DEFAULT_LIMIT: usize = 1_000;
/// The most lines a page holds, whatever the request asks for.
pub const MAX_LIMIT: usize = 5_000;
/// The most lines kept before a match to show as its context.
const MAX_CONTEXT_BEFORE: usize = 1_000;

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
    /// Number of lines of each level in the run, before filtering.
    #[serde(rename = "levelCounts")]
    pub level_counts: BTreeMap<String, usize>,
    /// Where the next page starts, `null` on the last one.
    pub next: Option<String>,
}

/// The position of a line in the order of a run: where a page stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogCursor {
    created_at: DateTime<Utc>,
    id: Uuid,
    /// Lines of context still to show after this one, when a page stopped
    /// inside the context of a match.
    after_left: usize,
}

impl LogCursor {
    pub fn parse(value: &str) -> Option<Self> {
        let mut parts = value.split('.');
        let micros = parts.next()?;
        let id = parts.next()?;
        let after_left = match parts.next() {
            Some(left) => left.parse().ok()?,
            None => 0,
        };
        if parts.next().is_some() {
            return None;
        }
        Some(Self {
            created_at: DateTime::from_timestamp_micros(micros.parse().ok()?)?,
            id: Uuid::parse_str(id).ok()?,
            after_left,
        })
    }

    fn of(log: &LogRow) -> Self {
        Self {
            created_at: log.created_at,
            id: log.id,
            after_left: 0,
        }
    }

    pub fn encode(&self) -> String {
        let (micros, id) = (self.created_at.timestamp_micros(), self.id);
        match self.after_left {
            0 => format!("{micros}.{id}"),
            left => format!("{micros}.{id}.{left}"),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RunLogOptions<'a> {
    pub query: &'a str,
    /// Comma-separated levels, e.g. `info,warn,error`.
    pub levels: &'a str,
    pub include_context: bool,
    /// The most lines of the page. A page that has no line yet takes a match
    /// with the context before it (1,000 lines at most) even if it is longer.
    pub limit: usize,
    /// The `next` of the previous page.
    pub cursor: Option<LogCursor>,
}

/// The lines of a run a scan keeps in the database: its levels, its time
/// range, and where it starts.
struct Scan {
    run_id: String,
    levels: Vec<String>,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    after: Option<LogCursor>,
    done: bool,
}

impl Scan {
    fn new(run_id: &str, levels: &str, parsed: &ParsedLogQuery, after: Option<LogCursor>) -> Self {
        let levels: Vec<String> = levels
            .split(',')
            .filter(|level| !level.is_empty())
            .map(str::to_string)
            .collect();
        Self {
            run_id: run_id.to_string(),
            done: levels.is_empty(),
            levels,
            from: parsed.time.from,
            to: parsed.time.to,
            after,
        }
    }

    /// The next lines of the scan, `None` once it has read them all.
    async fn next_chunk(&mut self, db: &PgPool, size: i64) -> Result<Option<Vec<LogRow>>> {
        if self.done {
            return Ok(None);
        }
        let mut query = QueryBuilder::<Postgres>::new(
            "SELECT id, flow_run_id, level, message, payload, created_at FROM logs WHERE flow_run_id = ",
        );
        query.push_bind(self.run_id.clone());
        query.push(" AND level = ANY(").push_bind(self.levels.clone()).push(")");
        if let Some(from) = self.from {
            query.push(" AND created_at >= ").push_bind(from);
        }
        if let Some(to) = self.to {
            query.push(" AND created_at <= ").push_bind(to);
        }
        if let Some(after) = self.after {
            // The first condition is the one an index on `created_at` can use.
            query.push(" AND created_at <= ").push_bind(after.created_at);
            query.push(" AND (created_at < ").push_bind(after.created_at);
            query.push(" OR id < ").push_bind(after.id).push(")");
        }
        query.push(" ORDER BY created_at DESC, id DESC LIMIT ").push_bind(size);
        let rows: Vec<LogRow> = query.build_query_as().fetch_all(db).await?;
        if (rows.len() as i64) < size {
            self.done = true;
        }
        self.after = rows.last().map(LogCursor::of).or(self.after);
        Ok((!rows.is_empty()).then_some(rows))
    }
}

async fn level_counts(db: &PgPool, run_id: &str) -> Result<BTreeMap<String, usize>> {
    let rows: Vec<(String, i64)> =
        sqlx::query_as("SELECT level, count(*) FROM logs WHERE flow_run_id = $1 GROUP BY level")
            .bind(run_id)
            .fetch_all(db)
            .await?;
    Ok(rows.into_iter().map(|(level, count)| (level, count as usize)).collect())
}

fn plain(log: LogRow) -> LogResponseRow {
    LogResponseRow { log, matched: None }
}

/// One page of the lines of a run.
///
/// Without a text filter the database returns the page. With one, the scan
/// reads the run chunk by chunk, keeping the matches, and the context lines
/// around them when asked for, until the page is full. The cursor of a page
/// carries what the next needs of the context, so the pages put end to end
/// hold what one unpaged request would, and none holds more than its limit.
pub async fn get_filtered_run_logs(
    db: &PgPool,
    flow_run_id: &str,
    options: RunLogOptions<'_>,
) -> Result<FilteredRunLogs> {
    // Before reading anything: a query the engine refuses is the caller's mistake.
    let (parsed, matcher) = prepare_log_query(options.query)?;
    let level_counts = level_counts(db, flow_run_id).await?;
    let total = level_counts.values().sum();
    let limit = options.limit.clamp(1, MAX_LIMIT);
    let mut scan = Scan::new(flow_run_id, options.levels, &parsed, options.cursor);

    if parsed.filters.is_empty() {
        let mut logs = scan.next_chunk(db, limit as i64 + 1).await?.unwrap_or_default();
        let next = if logs.len() > limit {
            logs.truncate(limit);
            logs.last().map(|log| LogCursor::of(log).encode())
        } else {
            None
        };
        return Ok(FilteredRunLogs {
            logs: logs.into_iter().map(plain).collect(),
            total,
            level_counts,
            next,
        });
    }

    let (before, after) = if options.include_context {
        (parsed.context.before.min(MAX_CONTEXT_BEFORE), parsed.context.after)
    } else {
        (0, 0)
    };
    let mut page = Page {
        matcher: &matcher,
        with_context: options.include_context,
        before,
        after,
        limit,
        pending: VecDeque::new(),
        after_left: options.cursor.map_or(0, |cursor| cursor.after_left),
        logs: Vec::new(),
        last: None,
    };
    let mut next = None;
    'scan: while let Some(chunk) = scan.next_chunk(db, CHUNK).await? {
        for log in chunk {
            if let Some(cursor) = page.push(log) {
                next = Some(cursor.encode());
                break 'scan;
            }
        }
    }
    Ok(FilteredRunLogs {
        logs: page.logs,
        total,
        level_counts,
        next,
    })
}

/// A page being filled by a scan, newest line first.
struct Page<'a> {
    matcher: &'a LogMatcher,
    with_context: bool,
    before: usize,
    after: usize,
    limit: usize,
    /// Lines that may become the context before the next match.
    pending: VecDeque<LogRow>,
    /// Lines still to take after the last match.
    after_left: usize,
    logs: Vec<LogResponseRow>,
    /// The last line of the page.
    last: Option<LogCursor>,
}

impl Page<'_> {
    fn emit(&mut self, log: LogRow, matched: bool) {
        self.last = Some(LogCursor::of(&log));
        self.logs.push(LogResponseRow {
            log,
            matched: self.with_context.then_some(matched),
        });
    }

    /// Takes the next line of the scan. Returns where the next page starts
    /// when the line would make this one longer than its limit.
    ///
    /// A match is shown with the lines before it, never without: when they do
    /// not fit in what is left of the page, the page stops before them and the
    /// next one starts with them. The one exception is a page with nothing yet,
    /// which takes them whatever the limit so that pages always advance; they
    /// are at most `MAX_CONTEXT_BEFORE`. The lines after a match come one at a
    /// time, and a page that stops among them hands the number still to come to
    /// the next in its cursor. A page ends at its last shown line, so a line is
    /// only read again if it was not shown.
    fn push(&mut self, log: LogRow) -> Option<LogCursor> {
        let matched = self.matcher.matches(log.message.as_deref(), log.payload.as_ref());
        let full = self.logs.len() >= self.limit;
        if matched {
            let needed = if self.after_left > 0 { 1 } else { self.pending.len() + 1 };
            if !self.logs.is_empty() && self.logs.len() + needed > self.limit {
                return self.stop();
            }
            while let Some(context) = self.pending.pop_front() {
                self.emit(context, false);
            }
            self.emit(log, true);
            self.after_left = self.after;
        } else if self.after_left > 0 {
            if full {
                return self.stop();
            }
            self.after_left -= 1;
            self.emit(log, false);
        } else if !full && self.before > 0 {
            if self.pending.len() == self.before {
                self.pending.pop_front();
            }
            self.pending.push_back(log);
        }
        None
    }

    /// Where the next page starts: after the last line of this one.
    fn stop(&self) -> Option<LogCursor> {
        self.last.map(|last| LogCursor {
            after_left: self.after_left,
            ..last
        })
    }
}

/// A cell, quoted. A spreadsheet reads a cell that starts with `=`, `+`, `-`,
/// `@`, a tab or a carriage return as a formula, and a log line holds what a
/// customer typed: such a cell starts with `'` (OWASP, CSV injection), which
/// the spreadsheet does not show.
fn csv_escape(value: &str) -> String {
    let neutralized = if value.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{value}")
    } else {
        value.to_string()
    };
    format!("\"{}\"", neutralized.replace('"', "\"\""))
}

const CSV_HEADER: &str = "id,level,message,payload,createdAt";

fn csv_row(log: &LogRow) -> String {
    let payload = match &log.payload {
        Some(payload) if !payload.is_null() => payload.to_string(),
        _ => Value::from("").to_string(),
    };
    [
        log.id.to_string(),
        log.level.clone(),
        csv_escape(log.message.as_deref().unwrap_or_default()),
        csv_escape(&payload),
        to_iso(&log.created_at),
    ]
    .join(",")
}

/// An element of the pretty-printed array of a JSON export.
fn json_item(log: &LogRow) -> String {
    let pretty = serde_json::to_string_pretty(&plain(log.clone())).unwrap_or_else(|_| "{}".to_string());
    pretty
        .lines()
        .map(|line| format!("  {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ExportFormat {
    Csv,
    Json,
}

/// A file being written as its lines are read.
struct Export {
    db: PgPool,
    scan: Scan,
    /// `None` when the query has no text filter.
    matcher: Option<LogMatcher>,
    format: ExportFormat,
    started: bool,
    finished: bool,
    /// Lines written so far.
    written: usize,
}

impl Export {
    /// The text of the next chunk of the file, `None` once it is complete.
    async fn next_bytes(&mut self) -> Result<Option<Bytes>> {
        while !self.finished {
            let chunk = self.scan.next_chunk(&self.db, CHUNK).await?;
            let mut text = String::new();
            if !self.started {
                self.started = true;
                text.push_str(match self.format {
                    ExportFormat::Csv => CSV_HEADER,
                    ExportFormat::Json => "[",
                });
            }
            for log in chunk.iter().flatten() {
                if let Some(matcher) = &self.matcher
                    && !matcher.matches(log.message.as_deref(), log.payload.as_ref())
                {
                    continue;
                }
                if self.format == ExportFormat::Json && self.written > 0 {
                    text.push(',');
                }
                text.push('\n');
                text.push_str(&match self.format {
                    ExportFormat::Csv => csv_row(log),
                    ExportFormat::Json => json_item(log),
                });
                self.written += 1;
            }
            if chunk.is_none() {
                self.finished = true;
                if self.format == ExportFormat::Json {
                    text.push_str(if self.written == 0 { "]" } else { "\n]" });
                }
            }
            if !text.is_empty() {
                return Ok(Some(Bytes::from(text)));
            }
        }
        Ok(None)
    }
}

/// `format` is `csv` or `json`; anything else is exported as JSON. `query`
/// and `levels` are those of [`RunLogOptions`]. The file is written as the
/// lines are read: it is never held whole.
pub fn create_log_export_response(
    db: PgPool,
    run_id: &str,
    format: &str,
    query: &str,
    levels: &str,
) -> Result<Response, InvalidLogQuery> {
    // Before the answer starts: a query the engine refuses is a `400`, not a broken file.
    let (parsed, matcher) = prepare_log_query(query)?;
    let format = if format == "csv" {
        ExportFormat::Csv
    } else {
        ExportFormat::Json
    };
    let (content_type, extension) = match format {
        ExportFormat::Csv => ("text/csv", "csv"),
        ExportFormat::Json => ("application/json", "json"),
    };
    let export = Export {
        db,
        scan: Scan::new(run_id, levels, &parsed, None),
        matcher: (!parsed.filters.is_empty()).then_some(matcher),
        format,
        started: false,
        finished: false,
        written: 0,
    };
    let mut response = Response::new(Body::from_stream(export_stream(export)));
    let headers = response.headers_mut();
    headers.insert(CONTENT_TYPE, content_type.parse().expect("static header"));
    let disposition = format!("attachment; filename=\"run-{run_id}-logs.{extension}\"");
    if let Ok(value) = disposition.parse() {
        headers.insert(CONTENT_DISPOSITION, value);
    }
    Ok(response)
}

fn export_stream(export: Export) -> impl Stream<Item = Result<Bytes>> {
    stream::try_unfold(export, |mut export| async move {
        Ok(export.next_bytes().await?.map(|bytes| (bytes, export)))
    })
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
        let lines = [
            csv_row(&row(Some("He said \"hi\""), Some(json!({ "a": 1 }))).log),
            csv_row(&row(None, None).log),
        ];
        assert_eq!(
            lines[0],
            "00000000-0000-0000-0000-000000000000,info,\"He said \"\"hi\"\"\",\"{\"\"a\"\":1}\",2026-03-01T10:00:00.000Z"
        );
        assert_eq!(
            lines[1],
            "00000000-0000-0000-0000-000000000000,info,\"\",\"\"\"\"\"\",2026-03-01T10:00:00.000Z"
        );
    }

    #[test]
    fn exports_csv_without_formulas() {
        for formula in [
            "=HYPERLINK(\"http://evil.example\",\"click\")",
            "+1+1",
            "-2+3",
            "@SUM(A1)",
            "\t=1+1",
            "\r=1+1",
        ] {
            let line = csv_row(&row(Some(formula), Some(json!(formula))).log);
            let message = format!("\"'{}\"", formula.replace('"', "\"\""));
            assert!(line.contains(&format!(",info,{message},")), "{formula:?}: {line}");
            // A cell that is a formula to a spreadsheet starts with the quote, not with the formula.
            assert!(
                !line.contains(&format!(",\"{}", formula.replace('"', "\"\""))),
                "{formula:?}: {line}"
            );
        }
        // A payload that is a JSON number or text keeps its meaning, and what is ordinary is left alone.
        let line = csv_row(&row(Some("a-b = c"), Some(json!(-5))).log);
        assert!(line.contains(",\"a-b = c\",\"'-5\","), "{line}");
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
    fn a_cursor_survives_its_encoding() {
        let cursor = LogCursor::of(&row(None, None).log);
        assert_eq!(LogCursor::parse(&cursor.encode()), Some(cursor));
        let inside_context = LogCursor {
            after_left: 7,
            ..cursor
        };
        assert!(inside_context.encode().ends_with(".7"));
        assert_eq!(LogCursor::parse(&inside_context.encode()), Some(inside_context));
        for bad in [
            "",
            "nope",
            "1.2",
            "x.00000000-0000-0000-0000-000000000000",
            "1.00000000-0000-0000-0000-000000000000.x",
            "1.00000000-0000-0000-0000-000000000000.1.2",
        ] {
            assert_eq!(LogCursor::parse(bad), None, "{bad}");
        }
    }

    #[tokio::test]
    async fn export_response_sets_download_headers() {
        let db = PgPool::connect_lazy("postgres://localhost/unused").unwrap();
        let response = create_log_export_response(db.clone(), "abc", "csv", "", "info").unwrap();
        assert_eq!(response.headers()[CONTENT_TYPE], "text/csv");
        assert_eq!(
            response.headers()[CONTENT_DISPOSITION],
            "attachment; filename=\"run-abc-logs.csv\""
        );

        let response = create_log_export_response(db, "abc", "x\"y", "", "info").unwrap();
        assert_eq!(response.headers()[CONTENT_TYPE], "application/json");
        assert_eq!(
            response.headers()[CONTENT_DISPOSITION],
            "attachment; filename=\"run-abc-logs.json\""
        );
    }
}
