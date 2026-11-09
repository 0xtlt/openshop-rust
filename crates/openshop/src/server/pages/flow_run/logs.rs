//! The logs of a run: the lines the query and the levels of the URL keep
//! (`?q=…&levels=…`), newest first, with the number of lines of each level;
//! the search form, a link per level that turns it on or off, the exports,
//! and the syntax of the query in a modal.

use anyhow::Result;
use chrono::{DateTime, Utc};
use maud::{Markup, html};
use serde_json::Value;
use url::form_urlencoded::Serializer;

use super::super::ui;
use crate::app::OpenShop;
use crate::db::schema::FlowRun;
use crate::runtime::time::to_iso;
use crate::server::api::run_logs::{
    DEFAULT_LIMIT, FilteredRunLogs, LogResponseRow, RunLogOptions, get_filtered_run_logs,
};
use crate::server::log_query::InvalidLogQuery;

/// The levels of the toggles, each shown when the URL names none.
const LOG_LEVELS: [&str; 3] = ["info", "warn", "error"];
const EVERY_LEVEL: &str = "info,warn,error";
pub(super) const HELP_MODAL: &str = "log-query-help";
const PLACEHOLDER: &str = r#"|= "text" != "exclude" |~ "regex" C:3"#;
const LOG_BOX_STYLE: &str = "font-family:ui-monospace, SFMono-Regular, Menlo, monospace;font-size:12px;line-height:1.7;max-height:500px;overflow-y:auto;border-radius:8px;background:#fafafa;padding:8px 12px";
/// The background of a line written since the page was last fetched.
const NEW_LINE: &str = "background:#fff3cd;";
/// The background of a line the search matched, among the lines of its context.
const MATCHED_LINE: &str = "background:#fff8e1;";
/// The syntax of a query, what the help modal says of it.
const SYNTAX: [(&str, &str); 8] = [
    (r#"|= "text""#, "Contains (case-insensitive)"),
    (r#"!= "text""#, "Excludes"),
    (r#"|~ "regex""#, "Regex match"),
    ("C:N", "Show N context lines around matches"),
    ("B:N / A:N", "Before / After context only"),
    ("last:5m", "Last 5 minutes (s/m/h/d)"),
    ("from:ISO to:ISO", "Absolute date range"),
    ("between:start,end", "Date range shorthand"),
];

/// The filters of the logs, read from the URL of the page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LogFilters {
    /// The search, `|= "order" last:5m`; empty for every line.
    pub(super) query: String,
    /// The levels shown, in the order they were turned on.
    pub(super) levels: Vec<String>,
}

impl LogFilters {
    /// `q` and `levels` of the URL. Without `levels`, every level is shown;
    /// `levels=`, every one turned off, none.
    pub(super) fn read(query: Option<String>, levels: Option<String>) -> Self {
        let mut shown: Vec<String> = Vec::new();
        let levels = levels.as_deref().unwrap_or(EVERY_LEVEL);
        for level in levels.split(',').filter(|level| !level.is_empty()) {
            if !shown.iter().any(|known| known == level) {
                shown.push(level.to_string());
            }
        }
        Self {
            query: query.unwrap_or_default(),
            levels: shown,
        }
    }

    fn levels_param(&self) -> String {
        self.levels.join(",")
    }

    fn shows_every_level(&self) -> bool {
        self.levels_param() == EVERY_LEVEL
    }

    fn shows(&self, level: &str) -> bool {
        self.levels.iter().any(|shown| shown == level)
    }

    /// The same filters with `level` turned off, or on after the others.
    fn toggled(&self, level: &str) -> Self {
        let mut toggled = self.clone();
        match toggled.levels.iter().position(|shown| shown == level) {
            Some(index) => {
                toggled.levels.remove(index);
            }
            None => toggled.levels.push(level.to_string()),
        }
        toggled
    }

    /// The URL of the page of the run with these filters, the levels left out
    /// when every one is shown.
    pub(super) fn page_url(&self, pathname: &str) -> String {
        let mut params = Serializer::new(String::new());
        if !self.query.is_empty() {
            params.append_pair("q", &self.query);
        }
        if !self.shows_every_level() {
            params.append_pair("levels", &self.levels_param());
        }
        let query = params.finish();
        if query.is_empty() {
            pathname.to_string()
        } else {
            format!("{pathname}?{query}")
        }
    }

    /// The file of the lines these filters keep, all of them: `json` or `csv`.
    fn export_url(&self, run_id: &str, format: &str) -> String {
        let mut params = Serializer::new(String::new());
        if !self.query.is_empty() {
            params.append_pair("q", &self.query);
        }
        params.append_pair("levels", &self.levels_param());
        params.append_pair("format", format);
        format!("/api/runs/{run_id}/logs/export?{}", params.finish())
    }
}

/// The lines of a run that the filters keep, or why the search was refused.
pub(super) struct Logs {
    /// The newest lines, with the counts of the run. No line when the search
    /// was refused.
    pub(super) page: FilteredRunLogs,
    /// Why the search was refused: a bad regular expression, a date past the calendar.
    pub(super) refused: Option<String>,
}

fn options<'a>(query: &'a str, levels: &'a str) -> RunLogOptions<'a> {
    RunLogOptions {
        query,
        levels,
        include_context: true,
        limit: DEFAULT_LIMIT,
        cursor: None,
    }
}

/// The newest lines of the run the filters keep, as the JSON API reads them.
pub(super) async fn load_logs(app: &OpenShop, run: &FlowRun, filters: &LogFilters) -> Result<Logs> {
    let run_id = run.id.to_string();
    let levels = filters.levels_param();
    match get_filtered_run_logs(app.db(), &run_id, options(&filters.query, &levels)).await {
        Ok(page) => Ok(Logs { page, refused: None }),
        Err(error) => {
            let Some(InvalidLogQuery(reason)) = error.downcast_ref::<InvalidLogQuery>() else {
                return Err(error);
            };
            // No level: no line, and the counts of the run, which a refused search does not read.
            let page = get_filtered_run_logs(app.db(), &run_id, options("", "")).await?;
            Ok(Logs {
                page,
                refused: Some(reason.clone()),
            })
        }
    }
}

/// The section of the logs of the run `run_id`, whose page is at `pathname`.
/// The lines written since `fresh_since`, when it is given, are new: the page
/// is fetched again while the run is active, and shows them highlighted.
pub(super) fn logs_section(
    pathname: &str,
    run_id: &str,
    filters: &LogFilters,
    logs: &Logs,
    fresh_since: Option<DateTime<Utc>>,
) -> Markup {
    let lines = &logs.page.logs;
    let searching = !filters.query.is_empty();
    html! {
        s-section heading="Logs" {
            div style="display:flex;gap:8px;align-items:center;margin-bottom:12px" {
                form method="get" style="display:flex;gap:8px;align-items:center;flex:1" {
                    div style="flex:1" {
                        s-search-field
                            name="q"
                            label="Search logs"
                            labelAccessibilityVisibility="exclusive"
                            placeholder=(PLACEHOLDER)
                            value=(filters.query) {}
                    }
                    @if !filters.shows_every_level() {
                        input type="hidden" name="levels" value=(filters.levels_param());
                    }
                    s-button type="submit" variant="secondary" { "Search" }
                }
                s-button variant="secondary" accessibilityLabel="Show log query syntax" commandFor=(HELP_MODAL) command="--show" { "?" }
            }
            @if let Some(reason) = &logs.refused {
                div style="margin-bottom:12px" {
                    s-banner tone="critical" heading="The search was refused" { (reason) }
                }
            }
            div style="display:flex;gap:6px;margin-bottom:12px;align-items:center;flex-wrap:wrap" {
                @for level in LOG_LEVELS {
                    s-button
                        href=(filters.toggled(level).page_url(pathname))
                        variant=(if filters.shows(level) { "primary" } else { "secondary" })
                    {
                        (level.to_uppercase()) " (" (logs.page.level_counts.get(level).copied().unwrap_or(0)) ")"
                    }
                }
                span style="margin-left:auto;font-size:12px;color:#8c9196" {
                    (lines.len()) "/" (logs.page.total) " logs"
                }
                @for (format, label) in [("json", "JSON"), ("csv", "CSV")] {
                    s-button
                        variant="secondary"
                        data-openshop-download=(filters.export_url(run_id, format))
                        data-openshop-filename=(format!("run-{run_id}-logs.{format}"))
                    { (label) }
                }
            }
            @if lines.is_empty() {
                s-text color="subdued" { (if logs.page.total == 0 { "No logs." } else { "No matching logs." }) }
            } @else {
                div tabindex="0" role="log" aria-label="Run logs" style=(LOG_BOX_STYLE) {
                    @for row in lines { (log_line(row, searching, fresh_since)) }
                }
                @if logs.page.next.is_some() {
                    s-text color="subdued" { "Showing the newest lines only. Export the logs for the whole run." }
                }
            }
        }
    }
}

fn level_color(level: &str) -> &'static str {
    match level {
        "info" => "#2e72d2",
        "warn" => "#b98900",
        "error" => "#d72c0d",
        _ => "inherit",
    }
}

/// A line: its time, its level, its message and its payload. A line of the
/// context of a match is faded; a match of a search, and a new line, are
/// highlighted.
fn log_line(row: &LogResponseRow, searching: bool, fresh_since: Option<DateTime<Utc>>) -> Markup {
    let log = &row.log;
    let opacity = if row.matched == Some(false) { "opacity:0.5;" } else { "" };
    let background = if fresh_since.is_some_and(|since| log.created_at >= since) {
        NEW_LINE
    } else if searching && row.matched == Some(true) {
        MATCHED_LINE
    } else {
        ""
    };
    html! {
        div style=(format!("display:flex;gap:10px;padding:1px 4px;border-radius:3px;overflow:hidden;{opacity}{background}")) {
            span style="color:#8c9196;flex-shrink:0;min-width:75px" { (ui::time(&to_iso(&log.created_at))) }
            span style=(format!("color:{};font-weight:700;flex-shrink:0;min-width:40px", level_color(&log.level))) {
                (log.level.to_uppercase())
            }
            span style="white-space:pre-wrap;word-break:break-word" {
                @if let Some(message) = &log.message { (message) }
                @if let Some(payload) = payload_text(log.payload.as_ref()) {
                    span style="color:#8c9196" { " " (payload) }
                }
            }
        }
    }
}

/// JavaScript's truthiness of a JSON value.
fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|number| number != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// `Object.entries(value)`.
fn entries(value: &Value) -> Vec<(String, Value)> {
    match value {
        Value::Object(map) => map.iter().map(|(key, value)| (key.clone(), value.clone())).collect(),
        Value::Array(items) => items
            .iter()
            .enumerate()
            .map(|(index, item)| (index.to_string(), item.clone()))
            .collect(),
        Value::String(text) => text
            .chars()
            .enumerate()
            .map(|(index, character)| (index.to_string(), Value::String(character.to_string())))
            .collect(),
        _ => Vec::new(),
    }
}

/// The `key=value` pairs of the payload of a line, `None` when it has nothing to show.
fn payload_text(payload: Option<&Value>) -> Option<String> {
    let entries = entries(payload.filter(|payload| is_truthy(payload))?);
    (!entries.is_empty()).then(|| {
        entries
            .iter()
            .map(|(key, value)| format!("{key}={}", serde_json::to_string(value).unwrap_or_default()))
            .collect::<Vec<_>>()
            .join(" ")
    })
}

/// The syntax of a query, in the modal its `?` button opens.
pub(super) fn help_modal() -> Markup {
    html! {
        s-modal id=(HELP_MODAL) heading="Query syntax" {
            div style="font-family:monospace;font-size:12px;line-height:2" {
                @for (syntax, meaning) in SYNTAX {
                    div { strong { (syntax) } " — " (meaning) }
                }
                div { "Free text — Simple substring search" }
                div style="margin-top:4px" { "Example: " code { r#"|= "order" != "retry" last:1h C:2"# } }
            }
            s-button slot="primary-action" commandFor=(HELP_MODAL) command="--hide" { "Close" }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use crate::db::schema::LogRow;
    use serde_json::json;
    use uuid::Uuid;

    fn levels(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    fn filters(query: &str, shown: &[&str]) -> LogFilters {
        LogFilters {
            query: query.to_string(),
            levels: levels(shown),
        }
    }

    fn line(id: u128, level: &str, message: &str, created_at: &str) -> LogResponseRow {
        LogResponseRow {
            log: LogRow {
                id: Uuid::from_u128(id),
                flow_run_id: Some("run".to_string()),
                level: level.to_string(),
                message: Some(message.to_string()),
                payload: None,
                created_at: created_at.parse().unwrap(),
            },
            matched: None,
        }
    }

    fn logs(lines: Vec<LogResponseRow>, total: usize, next: Option<&str>) -> Logs {
        Logs {
            page: FilteredRunLogs {
                logs: lines,
                total,
                level_counts: BTreeMap::from([("info".to_string(), 4), ("error".to_string(), 1)]),
                next: next.map(str::to_string),
            },
            refused: None,
        }
    }

    #[test]
    fn reads_the_filters_of_the_url() {
        assert_eq!(LogFilters::read(None, None), filters("", &LOG_LEVELS));
        assert_eq!(
            LogFilters::read(Some("last:5m".to_string()), Some("error,,warn,error".to_string())),
            filters("last:5m", &["error", "warn"])
        );
        // Every level turned off.
        assert_eq!(LogFilters::read(None, Some(String::new())), filters("", &[]));
    }

    #[test]
    fn a_level_is_turned_on_and_off_by_a_link() {
        let every = filters("", &LOG_LEVELS);
        assert_eq!(every.page_url("/runs/1"), "/runs/1");
        assert_eq!(filters("order", &LOG_LEVELS).page_url("/runs/1"), "/runs/1?q=order");
        let without_info = every.toggled("info");
        assert_eq!(without_info.levels, levels(&["warn", "error"]));
        assert_eq!(without_info.toggled("info").levels, levels(&["warn", "error", "info"]));
        assert_eq!(
            filters(r#"|= "x""#, &["warn", "error", "info"]).page_url("/runs/1"),
            "/runs/1?q=%7C%3D+%22x%22&levels=warn%2Cerror%2Cinfo"
        );
        assert_eq!(filters("", &[]).page_url("/runs/1"), "/runs/1?levels=");
        assert_eq!(
            filters("order", &["warn", "info"]).export_url("abc", "csv"),
            "/api/runs/abc/logs/export?q=order&levels=warn%2Cinfo&format=csv"
        );
        assert_eq!(
            every.export_url("abc", "json"),
            "/api/runs/abc/logs/export?levels=info%2Cwarn%2Cerror&format=json"
        );
    }

    #[test]
    fn renders_payloads_as_key_value_pairs() {
        assert_eq!(
            payload_text(Some(&json!({ "orderId": 42, "tags": ["a"], "note": "x y" }))).as_deref(),
            Some(r#"orderId=42 tags=["a"] note="x y""#)
        );
        assert_eq!(payload_text(Some(&json!({}))), None);
        assert_eq!(payload_text(Some(&json!(null))), None);
        assert_eq!(payload_text(None), None);
    }

    #[test]
    fn renders_the_filters_the_counts_the_exports_and_the_lines() {
        let filters = filters("order", &["error", "warn"]);
        let mut first = line(1, "error", "<b>Order</b> failed", "2026-11-09T12:00:02Z");
        first.matched = Some(true);
        first.log.payload = Some(json!({ "orderId": "<42>" }));
        let mut context = line(2, "warn", "before", "2026-11-09T12:00:01Z");
        context.matched = Some(false);
        let html = logs_section(
            "/runs/r1",
            "r1",
            &filters,
            &logs(vec![first, context], 5, Some("1.x")),
            None,
        )
        .into_string();
        // The search is a form of the URL, which keeps the levels.
        assert!(html.contains(r#"<form method="get""#), "{html}");
        assert!(html.contains(r#"name="q" label="Search logs""#), "{html}");
        assert!(html.contains(r#"value="order"></s-search-field>"#), "{html}");
        assert!(
            html.contains(r#"<input type="hidden" name="levels" value="error,warn">"#),
            "{html}"
        );
        assert!(
            html.contains(r#"commandFor="log-query-help" command="--show">?</s-button>"#),
            "{html}"
        );
        // A link per level, which turns it on or off, with its count.
        assert!(
            html.contains(r#"<s-button href="/runs/r1?q=order&amp;levels=error%2Cwarn%2Cinfo" variant="secondary">INFO (4)</s-button>"#),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<s-button href="/runs/r1?q=order&amp;levels=warn" variant="primary">ERROR (1)</s-button>"#
            ),
            "{html}"
        );
        assert!(html.contains(">WARN (0)</s-button>"), "{html}");
        assert!(html.contains("2/5 logs"), "{html}");
        assert!(
            html.contains(r#"data-openshop-download="/api/runs/r1/logs/export?q=order&amp;levels=error%2Cwarn&amp;format=csv" data-openshop-filename="run-r1-logs.csv">CSV</s-button>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"data-openshop-filename="run-r1-logs.json">JSON</s-button>"#),
            "{html}"
        );
        // Lines are data: escaped. The match is highlighted, its context faded.
        assert!(html.contains("&lt;b&gt;Order&lt;/b&gt; failed"), "{html}");
        assert!(html.contains(r#"orderId=&quot;&lt;42&gt;&quot;"#), "{html}");
        assert!(!html.contains("<b>"), "{html}");
        assert!(html.contains(&format!("overflow:hidden;{MATCHED_LINE}\">")), "{html}");
        assert!(html.contains("overflow:hidden;opacity:0.5;\">"), "{html}");
        assert!(html.contains(r#"<time datetime="2026-11-09T12:00:02.000Z">"#), "{html}");
        assert!(html.contains(">ERROR</span>"), "{html}");
        assert!(html.contains("Showing the newest lines only."), "{html}");
        assert!(!html.contains("The search was refused"), "{html}");
    }

    #[test]
    fn highlights_the_lines_written_since_the_page_was_fetched() {
        let lines = vec![
            line(1, "info", "new", "2026-11-09T12:00:05Z"),
            line(2, "info", "old", "2026-11-09T12:00:00Z"),
        ];
        let since = "2026-11-09T12:00:03Z".parse().unwrap();
        let html = logs_section(
            "/runs/r1",
            "r1",
            &LogFilters::read(None, None),
            &logs(lines, 2, None),
            Some(since),
        )
        .into_string();
        assert_eq!(html.matches(NEW_LINE).count(), 1, "{html}");
        assert!(html.find(NEW_LINE).unwrap() < html.find(">new<").unwrap(), "{html}");
        // Every level: the search form does not name them.
        assert!(!html.contains(r#"name="levels""#), "{html}");
        assert!(!html.contains("Showing the newest lines only."), "{html}");
    }

    #[test]
    fn says_why_there_is_no_line() {
        let every = LogFilters::read(None, None);
        let none = logs_section("/runs/r1", "r1", &every, &logs(Vec::new(), 0, None), None).into_string();
        assert!(none.contains("No logs."), "{none}");
        assert!(!none.contains(r#"role="log""#), "{none}");
        let mut refused = logs(Vec::new(), 5, None);
        refused.refused = Some("Invalid regex \"(\": <unclosed>".to_string());
        let html = logs_section("/runs/r1", "r1", &every, &refused, None).into_string();
        assert!(html.contains("No matching logs."), "{html}");
        assert!(
            html.contains(r#"<s-banner tone="critical" heading="The search was refused">Invalid regex &quot;(&quot;: &lt;unclosed&gt;</s-banner>"#),
            "{html}"
        );
        let help = help_modal().into_string();
        assert!(
            help.contains(r#"<s-modal id="log-query-help" heading="Query syntax">"#),
            "{help}"
        );
        assert!(
            help.contains("<strong>last:5m</strong> — Last 5 minutes (s/m/h/d)"),
            "{help}"
        );
    }
}
