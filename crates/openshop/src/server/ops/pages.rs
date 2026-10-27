//! The pages of the operator dashboard: what happens in every shop, over a
//! period, with the runs, the log lines, the flows and the shops behind it.

use axum::extract::{Path, RawQuery, State};
use axum::http::StatusCode;
use axum::response::Response;
use chrono::{DateTime, Utc};
use indexmap::IndexMap;
use uuid::Uuid;

use super::chart::{Series, area_chart, grouped};
use super::html::{
    Cell, Page, Row, Section, body, columns, definitions, duration, empty, error_text, full_time, json_block, kpi,
    kpis, legend, level, link, message_page, mono, muted, note, page, panel, row, status, status_as, table, time, wide,
};
use super::html_response;
use super::queries::{self, Counts, Cursor, LogFilter, LogLine, Range, RunFilter, SHOP_STATES, Shop, Tally};
use crate::app::OpenShop;
use crate::db::{FlowRun, Installation, LogRow, StepResult};
use crate::engine::scheduler::next_occurrence;
use crate::server::http::{ApiResult, escape, parse_query};
use crate::types::{ALL_RUN_STATUSES, CronShops};
use openshop_shared::rpc::encode_uri_component;

const LOG_LEVELS: [&str; 3] = ["error", "warn", "info"];
/// Rows of a list of the overview, and of the page of a shop.
const FEW: i64 = 10;
/// Rows of a page of runs or of log lines.
const PER_PAGE: i64 = 100;
const MAX_SHOPS: i64 = 500;
const MAX_LOG_LINES: i64 = 500;
const REFRESH_SECONDS: u32 = 30;

const RUNS: &str = "/ops/runs";
const LOGS: &str = "/ops/logs";
const SHOPS: &str = "/ops/shops";
const FLOWS: &str = "/ops/flows";

fn ok(html: String) -> Response {
    html_response(StatusCode::OK, html)
}

pub(super) fn not_found_page() -> Response {
    html_response(
        StatusCode::NOT_FOUND,
        message_page("Not found", "The dashboard has no page at this address."),
    )
}

/// Whether rows name their Shopify app: only an application that serves several does.
fn several_apps(app: &OpenShop) -> bool {
    app.apps().all().len() > 1
}

/// The query string of a request, by name.
struct Params(IndexMap<String, String>);

impl Params {
    fn new(raw: Option<&str>) -> Self {
        Self(parse_query(raw))
    }

    /// The value of a parameter, when it has one.
    fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(|value| value.trim()).filter(|value| !value.is_empty())
    }

    fn owned(&self, name: &str) -> Option<String> {
        self.get(name).map(str::to_string)
    }

    /// Where a page of results ends: the row the previous one stopped at.
    fn before(&self) -> Option<Cursor> {
        self.get("before").and_then(Cursor::parse)
    }

    /// The address of `path` with the parameters `keep` of this request, and
    /// `changes` applied to them: a value replaces one, `None` drops it.
    fn href(&self, path: &str, keep: &[&str], changes: &[(&str, Option<&str>)]) -> String {
        let changed = |name: &str| changes.iter().find(|(changed, _)| *changed == name).map(|(_, value)| *value);
        let kept = keep
            .iter()
            .filter(|name| changed(name).is_none())
            .filter_map(|name| Some((*name, self.get(name)?)));
        let set = changes.iter().filter_map(|(name, value)| Some((*name, (*value)?)));
        let query: Vec<String> = kept
            .chain(set)
            .map(|(name, value)| format!("{name}={}", encode_uri_component(value)))
            .collect();
        if query.is_empty() {
            path.to_string()
        } else {
            format!("{path}?{}", query.join("&"))
        }
    }
}

/// The periods of a page, the current one marked. Changing the period
/// starts from the first page of results.
fn ranges(params: &Params, path: &str, keep: &[&str], current: Range) -> String {
    let links: String = Range::ALL
        .iter()
        .map(|range| {
            let marked = if *range == current { " aria-current=\"true\"" } else { "" };
            let href = params.href(path, keep, &[("range", Some(range.key())), ("before", None)]);
            format!("<a href=\"{}\"{marked}>{}</a>", escape(&href), range.key())
        })
        .collect();
    format!("<nav class=\"ranges\" aria-label=\"Period\">{links}</nav>")
}

/// The filters in force, each a link that removes it.
fn chips(params: &Params, path: &str, keep: &[&str], filters: &[(&str, &str)]) -> String {
    let chips: String = filters
        .iter()
        .filter_map(|(name, label)| {
            let value = params.get(name)?;
            let href = params.href(path, keep, &[(*name, None), ("before", None)]);
            Some(format!(
                "<a class=\"chip\" href=\"{}\" title=\"Remove this filter\">{label} <b>{}</b> ×</a>",
                escape(&href),
                escape(value)
            ))
        })
        .collect();
    if chips.is_empty() {
        chips
    } else {
        format!("<div class=\"chips\">{chips}</div>")
    }
}

fn select(name: &str, label: &str, values: &[&str], selected: Option<&str>) -> String {
    let options: String = values
        .iter()
        .map(|value| {
            let marked = if selected == Some(*value) { " selected" } else { "" };
            format!("<option value=\"{0}\"{marked}>{0}</option>", escape(value))
        })
        .collect();
    format!("<select name=\"{name}\" aria-label=\"{label}\"><option value=\"\">{label}: any</option>{options}</select>")
}

fn search(name: &str, placeholder: &str, value: Option<&str>) -> String {
    format!(
        "<input type=\"search\" name=\"{name}\" value=\"{}\" placeholder=\"{placeholder}\" aria-label=\"{placeholder}\">",
        escape(value.unwrap_or_default())
    )
}

/// A form that filters `path`, keeping the period.
fn filters(path: &str, range: Range, fields: &[String]) -> String {
    format!(
        "<form class=\"filters\" method=\"get\" action=\"{path}\">\
         <input type=\"hidden\" name=\"range\" value=\"{}\">{}<button type=\"submit\">Filter</button></form>",
        range.key(),
        fields.concat()
    )
}

/// A chart of counts over the period, above what its lines stand for. `bad`
/// names the line of what went wrong; a page already filtered on it has none.
fn chart(start: DateTime<Utc>, range: Range, counts: &Counts, total: &str, bad: Option<&str>) -> String {
    let mut series = vec![Series {
        name: total,
        class: "total",
        values: &counts.total,
    }];
    if let Some(bad) = bad {
        series.push(Series {
            name: bad,
            class: "bad",
            values: &counts.bad,
        });
    }
    format!(
        "{}{}",
        body(&area_chart(start, range.bucket(), &series)),
        legend(total, bad)
    )
}

/// `2.5%` of `whole`, or a dash when there is nothing to take a part of.
fn share(part: i64, whole: i64) -> String {
    if whole == 0 {
        "—".to_string()
    } else {
        format!("{:.1}%", part as f64 * 100.0 / whole as f64)
    }
}

fn count(value: i64) -> Cell {
    grouped(value).into()
}

/// A count that calls for a look when it is not zero.
fn failures(value: i64) -> Cell {
    if value > 0 {
        format!("<span class=\"bad\">{}</span>", grouped(value)).into()
    } else {
        "0".to_string().into()
    }
}

fn shop_href(shop: &str) -> String {
    format!("{SHOPS}/{}", encode_uri_component(shop))
}

fn shop_link(shop: &str) -> String {
    link(&shop_href(shop), shop)
}

fn run_href(run: Uuid) -> String {
    format!("{RUNS}/{run}")
}

fn runs_of_flow_href(flow: &str, range: Range) -> String {
    format!("{RUNS}?range={}&flow={}", range.key(), encode_uri_component(flow))
}

fn runs_of_flow_link(flow: &str, range: Range) -> String {
    format!(
        "<a class=\"mono\" href=\"{}\">{}</a>",
        escape(&runs_of_flow_href(flow, range)),
        escape(flow)
    )
}

fn run_duration(run: &FlowRun) -> String {
    match (run.started_at, run.completed_at) {
        (Some(started), Some(completed)) => duration((completed - started).num_milliseconds()),
        _ => "—".to_string(),
    }
}

fn run_headers(with_app: bool) -> Vec<&'static str> {
    let mut headers = vec!["Status", "Flow", "Details", "Shop"];
    if with_app {
        headers.push("App");
    }
    headers.extend(["Duration", "Created (UTC)"]);
    headers
}

fn run_row(run: &FlowRun, with_app: bool) -> Row {
    let details = match run.error.as_deref().filter(|error| !error.is_empty()) {
        Some(error) => error_text(Some(error)),
        None => muted(&format!("attempt {}", run.attempts.max(1))),
    };
    let mut cells: Vec<Cell> = vec![
        status(&run.status).into(),
        format!(
            "<a class=\"mono\" href=\"{}\">{}</a>",
            run_href(run.id),
            escape(&run.flow_name)
        )
        .into(),
        wide(details),
        shop_link(&run.shop).into(),
    ];
    if with_app {
        cells.push(escape(&run.app_handle).into());
    }
    cells.extend([run_duration(run).into(), time(&run.created_at).into()]);
    row(cells).to(run_href(run.id))
}

fn run_table(runs: &[FlowRun], with_app: bool, empty: &str) -> String {
    let rows: Vec<Row> = runs.iter().map(|run| run_row(run, with_app)).collect();
    table(&run_headers(with_app), &rows, empty)
}

/// The link to the results that come after `last`, when the page is full.
fn older(params: &Params, path: &str, keep: &[&str], shown: usize, last: Option<Cursor>) -> String {
    match last.filter(|_| shown as i64 == PER_PAGE) {
        Some(last) => {
            let before = last.param();
            let href = params.href(path, keep, &[("before", Some(before.as_str()))]);
            format!("<div class=\"more\">{}</div>", link(&href, "Older"))
        }
        None => String::new(),
    }
}

/// What ran in a period, by flow or by shop. `href` says where a name leads.
fn tally_table(first: &str, tallies: &[Tally], href: impl Fn(&str) -> String, empty: &str) -> String {
    let rows: Vec<Row> = tallies
        .iter()
        .map(|tally| {
            let href = href(&tally.name);
            row(vec![
                format!("<a class=\"mono\" href=\"{}\">{}</a>", escape(&href), escape(&tally.name)).into(),
                failures(tally.failed),
                count(tally.runs),
                share(tally.failed, tally.runs).into(),
            ])
            .to(href)
        })
        .collect();
    table(&[first, "Failed", "Runs", "Failure rate"], &rows, empty)
}

/// `/ops`: the period at a glance, and what calls for a look.
pub(super) async fn overview(State(app): State<OpenShop>, RawQuery(raw): RawQuery) -> ApiResult {
    let db = app.db();
    let params = Params::new(raw.as_deref());
    let range = Range::parse(params.get("range"));
    let now = Utc::now();
    let start = range.start(now);
    let everything = RunFilter::default();

    let installed = Installation::installed(db).count().await?;
    let totals = queries::totals(db, start).await?;
    let queue = queries::queue(db).await?;
    let counts = queries::run_counts(db, &everything, start, range.bucket(), range.buckets(now)).await?;
    let failing_flows = queries::flows(db, start, true, FEW).await?;
    let failing_shops = queries::failing_shops(db, start, FEW).await?;
    let latest_failures = queries::runs(
        db,
        &RunFilter {
            since: Some(start),
            status: Some("failed".to_string()),
            ..Default::default()
        },
        FEW,
    )
    .await?;

    let waiting = match queue.oldest_waiting {
        Some(oldest) => format!("the oldest for {}", duration((now - oldest).num_milliseconds().max(0))),
        None => "none is late".to_string(),
    };
    let content = [
        kpis(&[
            kpi("Installed shops", &grouped(installed), "with the app in place", false),
            kpi("Runs", &grouped(totals.runs), range.label(), false),
            kpi(
                "Failed",
                &grouped(totals.failed),
                &format!("{} of the runs", share(totals.failed, totals.runs)),
                totals.failed > 0,
            ),
            kpi("Waiting", &grouped(queue.waiting), &waiting, false),
            kpi("Running", &grouped(queue.running), "with a worker now", false),
        ]),
        panel(
            &format!("Runs, {}", range.label()),
            &chart(start, range, &counts, "runs", Some("failed")),
        ),
        columns(&[
            panel(
                "Flows with failures",
                &tally_table(
                    "Flow",
                    &failing_flows,
                    |flow| runs_of_flow_href(flow, range),
                    "No flow failed.",
                ),
            ),
            panel(
                "Shops with failures",
                &tally_table("Shop", &failing_shops, shop_href, "No shop had a failure."),
            ),
        ]),
        panel(
            "Latest failures",
            &run_table(&latest_failures, several_apps(&app), "No run failed."),
        ),
        note(&format!(
            "Times are in UTC. This page reloads every {REFRESH_SECONDS} seconds."
        )),
    ]
    .concat();
    Ok(ok(page(Page {
        section: Section::Overview,
        trail: &[("Overview", None)],
        toolbar: &ranges(&params, "/ops", &["range"], range),
        refresh_seconds: Some(REFRESH_SECONDS),
        content: &content,
    })))
}

/// `/ops/runs`: the runs of every shop over the period, filtered by the query.
pub(super) async fn runs(State(app): State<OpenShop>, RawQuery(raw): RawQuery) -> ApiResult {
    const KEEP: [&str; 5] = ["range", "status", "flow", "shop", "q"];
    let db = app.db();
    let params = Params::new(raw.as_deref());
    let range = Range::parse(params.get("range"));
    let now = Utc::now();
    let start = range.start(now);
    let state = params.get("status").filter(|status| ALL_RUN_STATUSES.contains(status));
    let filter = RunFilter {
        since: Some(start),
        before: params.before(),
        status: state.map(str::to_string),
        flow: params.owned("flow"),
        shop: params.owned("shop"),
        text: params.owned("q"),
        exact_shop: false,
    };

    let counts = queries::run_counts(db, &filter, start, range.bucket(), range.buckets(now)).await?;
    let runs = queries::runs(db, &filter, PER_PAGE).await?;

    let mut flows: Vec<&str> = app.config().flows.keys().map(String::as_str).collect();
    // A flow the application no longer registers still has runs to look at.
    if let Some(flow) = params.get("flow").filter(|flow| !flows.contains(flow)) {
        flows.push(flow);
    }
    let form = filters(
        RUNS,
        range,
        &[
            search("q", "Search in errors", params.get("q")),
            select("status", "Status", &ALL_RUN_STATUSES, state),
            select("flow", "Flow", &flows, params.get("flow")),
            search("shop", "Shop", params.get("shop")),
        ],
    );
    let active = chips(
        &params,
        RUNS,
        &KEEP,
        &[
            ("status", "Status is"),
            ("flow", "Flow is"),
            ("shop", "Shop has"),
            ("q", "Error has"),
        ],
    );
    let last = runs.last().map(|run| Cursor::new(run.created_at, run.id));
    let content = [
        panel(
            &format!("Runs, {}", range.label()),
            &format!(
                "{}{form}{active}{}{}",
                chart(start, range, &counts, "runs", state.is_none().then_some("failed")),
                run_table(&runs, several_apps(&app), "No run matches."),
                older(&params, RUNS, &KEEP, runs.len(), last),
            ),
        ),
        note("Times are in UTC."),
    ]
    .concat();
    Ok(ok(page(Page {
        section: Section::Runs,
        trail: &[("Runs", None)],
        toolbar: &ranges(&params, RUNS, &KEEP, range),
        refresh_seconds: None,
        content: &content,
    })))
}

fn details(summary: &str, value: Option<&serde_json::Value>) -> String {
    value
        .map(|value| format!("<details><summary>{summary}</summary>{}</details>", json_block(value)))
        .unwrap_or_default()
}

fn step_row(step: &StepResult) -> Row {
    row(vec![
        status(&step.status).into(),
        mono(&step.step_name).into(),
        step.attempt.to_string().into(),
        wide(format!(
            "{}{}",
            error_text(step.error.as_deref()),
            details("Output", step.output.as_ref())
        )),
        step.duration_ms
            .map(|milliseconds| duration(milliseconds.into()))
            .unwrap_or_else(|| "—".to_string())
            .into(),
        time(&step.created_at).into(),
    ])
}

fn log_row(log: &LogRow) -> Row {
    row(vec![
        level(&log.level).into(),
        wide(format!(
            "{}{}",
            mono(log.message.as_deref().unwrap_or_default()),
            details("Payload", log.payload.as_ref())
        )),
        time(&log.created_at).into(),
    ])
}

/// `/ops/runs/:id`: a run, with its steps and its log lines.
pub(super) async fn run(State(app): State<OpenShop>, Path(id): Path<String>) -> ApiResult {
    let db = app.db();
    let Ok(id) = Uuid::parse_str(&id) else {
        return Ok(not_found_page());
    };
    let Some(run) = FlowRun::all_shops(db).find(id).await? else {
        return Ok(not_found_page());
    };
    let steps = StepResult::of_run(db, id)
        .order_by(StepResult::CREATED_AT.asc())
        .all()
        .await?;
    let logs = LogRow::of_run(db, id)
        .order_by(LogRow::CREATED_AT.asc())
        .limit(MAX_LOG_LINES)
        .all()
        .await?;

    let mut summary = vec![("Status", status(&run.status)), ("Shop", shop_link(&run.shop))];
    if several_apps(&app) {
        summary.push(("App", escape(&run.app_handle)));
    }
    summary.extend([
        ("Flow", runs_of_flow_link(&run.flow_name, Range::Day)),
        ("Attempts", run.attempts.to_string()),
        ("Created", full_time(Some(&run.created_at))),
        ("Started", full_time(run.started_at.as_ref())),
        ("Completed", full_time(run.completed_at.as_ref())),
        ("Duration", run_duration(&run)),
        ("Run", mono(&run.id.to_string())),
    ]);
    if run.error.as_deref().is_some_and(|error| !error.is_empty()) {
        summary.push(("Error", error_text(run.error.as_deref())));
    }

    let step_rows: Vec<Row> = steps.iter().map(step_row).collect();
    let log_rows: Vec<Row> = logs.iter().map(log_row).collect();
    let cut = if logs.len() as i64 == MAX_LOG_LINES {
        format!(
            "<div class=\"more\">{}</div>",
            muted(&format!("The first {MAX_LOG_LINES} lines are shown."))
        )
    } else {
        String::new()
    };

    let content = [
        columns(&[
            panel("Summary", &body(&definitions(&summary))),
            panel(
                "Input",
                &run.input
                    .as_ref()
                    .map(json_block)
                    .unwrap_or_else(|| empty("The run has no input.")),
            ),
        ]),
        panel(
            "Steps",
            &table(
                &["Status", "Step", "Attempt", "Result", "Duration", "At (UTC)"],
                &step_rows,
                "No step ran.",
            ),
        ),
        panel(
            "Logs",
            &format!(
                "{}{cut}",
                table(
                    &["Level", "Message", "At (UTC)"],
                    &log_rows,
                    "The run wrote no log line."
                )
            ),
        ),
        note("Times are in UTC."),
    ]
    .concat();
    Ok(ok(page(Page {
        section: Section::Runs,
        trail: &[("Runs", Some(RUNS)), (&run.flow_name, None)],
        toolbar: "",
        refresh_seconds: None,
        content: &content,
    })))
}

fn log_line_row(line: &LogLine) -> Row {
    let message = mono(line.message.as_deref().unwrap_or_default());
    let run = queries::run_id(line);
    let cells = row(vec![
        level(&line.level).into(),
        wide(message),
        match (run, &line.flow_name) {
            (Some(run), Some(flow)) => {
                format!("<a class=\"mono\" href=\"{}\">{}</a>", run_href(run), escape(flow)).into()
            }
            _ => muted("—").into(),
        },
        line.shop
            .as_deref()
            .map(shop_link)
            .unwrap_or_else(|| muted("—"))
            .into(),
        time(&line.created_at).into(),
    ]);
    // A line leads to the run that wrote it.
    match run {
        Some(run) => cells.to(run_href(run)),
        None => cells,
    }
}

/// `/ops/logs`: the lines the flows of every shop wrote over the period.
pub(super) async fn logs(State(app): State<OpenShop>, RawQuery(raw): RawQuery) -> ApiResult {
    const KEEP: [&str; 5] = ["range", "level", "flow", "shop", "q"];
    let db = app.db();
    let params = Params::new(raw.as_deref());
    let range = Range::parse(params.get("range"));
    let now = Utc::now();
    let start = range.start(now);
    let severity = params.get("level").filter(|level| LOG_LEVELS.contains(level));
    let filter = LogFilter {
        since: Some(start),
        before: params.before(),
        level: severity.map(str::to_string),
        flow: params.owned("flow"),
        shop: params.owned("shop"),
        text: params.owned("q"),
    };

    let counts = queries::log_counts(db, &filter, start, range.bucket(), range.buckets(now)).await?;
    let lines = queries::logs(db, &filter, PER_PAGE).await?;

    let mut flows: Vec<&str> = app.config().flows.keys().map(String::as_str).collect();
    if let Some(flow) = params.get("flow").filter(|flow| !flows.contains(flow)) {
        flows.push(flow);
    }
    let form = filters(
        LOGS,
        range,
        &[
            search("q", "Search in messages", params.get("q")),
            select("level", "Level", &LOG_LEVELS, severity),
            select("flow", "Flow", &flows, params.get("flow")),
            search("shop", "Shop", params.get("shop")),
        ],
    );
    let active = chips(
        &params,
        LOGS,
        &KEEP,
        &[
            ("level", "Level is"),
            ("flow", "Flow is"),
            ("shop", "Shop has"),
            ("q", "Message has"),
        ],
    );
    let rows: Vec<Row> = lines.iter().map(log_line_row).collect();
    let last = lines.last().map(|line| Cursor::new(line.created_at, line.id));
    let content = [
        panel(
            &format!("Log lines, {}", range.label()),
            &format!(
                "{}{form}{active}{}{}",
                chart(start, range, &counts, "lines", severity.is_none().then_some("errors")),
                table(
                    &["Level", "Message", "Flow", "Shop", "At (UTC)"],
                    &rows,
                    "No log line matches."
                ),
                older(&params, LOGS, &KEEP, lines.len(), last),
            ),
        ),
        note("Times are in UTC. A line leads to the run that wrote it."),
    ]
    .concat();
    Ok(ok(page(Page {
        section: Section::Logs,
        trail: &[("Logs", None)],
        toolbar: &ranges(&params, LOGS, &KEEP, range),
        refresh_seconds: None,
        content: &content,
    })))
}

/// `/ops/flows`: the flows of the application, with how they ran over the period.
pub(super) async fn flows(State(app): State<OpenShop>, RawQuery(raw): RawQuery) -> ApiResult {
    let params = Params::new(raw.as_deref());
    let range = Range::parse(params.get("range"));
    let start = range.start(Utc::now());
    let mut tallies = queries::flows(app.db(), start, false, MAX_SHOPS).await?;
    // A registered flow that did not run has its line too.
    let registered = &app.config().flows;
    for flow in registered.keys() {
        if !tallies.iter().any(|tally| &tally.name == flow) {
            tallies.push(Tally {
                name: flow.clone(),
                runs: 0,
                failed: 0,
                mean_ms: None,
                latest: None,
            });
        }
    }

    let rows: Vec<Row> = tallies
        .iter()
        .map(|tally| {
            let name = if registered.contains_key(&tally.name) {
                runs_of_flow_link(&tally.name, range)
            } else {
                format!("{} {}", runs_of_flow_link(&tally.name, range), muted("not registered"))
            };
            row(vec![
                name.into(),
                count(tally.runs),
                failures(tally.failed),
                share(tally.failed, tally.runs).into(),
                tally
                    .mean_ms
                    .map(|milliseconds| duration(milliseconds.round() as i64))
                    .unwrap_or_else(|| "—".to_string())
                    .into(),
                tally
                    .latest
                    .as_ref()
                    .map(time)
                    .unwrap_or_else(|| "—".to_string())
                    .into(),
            ])
            .to(runs_of_flow_href(&tally.name, range))
        })
        .collect();
    let content = [
        panel(
            &format!("Flows, {}", range.label()),
            &table(
                &["Flow", "Runs", "Failed", "Failure rate", "Mean duration", "Latest run (UTC)"],
                &rows,
                "The application has no flow.",
            ),
        ),
        note("Times are in UTC. A flow leads to its runs."),
    ]
    .concat();
    Ok(ok(page(Page {
        section: Section::Flows,
        trail: &[("Flows", None)],
        toolbar: &ranges(&params, FLOWS, &["range"], range),
        refresh_seconds: None,
        content: &content,
    })))
}

fn shop_headers(with_app: bool) -> Vec<&'static str> {
    let mut headers = vec!["Shop"];
    if with_app {
        headers.push("App");
    }
    headers.extend(["State", "Since (UTC)", "Runs", "Failed", "Latest run (UTC)", "Scopes"]);
    headers
}

fn shop_row(shop: &Shop, with_app: bool) -> Row {
    let (state, since) = shop.state();
    let mut cells: Vec<Cell> = vec![shop_link(&shop.shop).into()];
    if with_app {
        cells.push(escape(&shop.app_handle).into());
    }
    cells.extend([
        status(state).into(),
        time(since).into(),
        count(shop.runs),
        failures(shop.failed),
        shop.latest
            .as_ref()
            .map(time)
            .unwrap_or_else(|| "—".to_string())
            .into(),
        wide(mono(shop.scopes.as_deref().unwrap_or_default())),
    ]);
    row(cells).to(shop_href(&shop.shop))
}

/// `/ops/shops`: the installations, with what each shop ran over the period.
pub(super) async fn shops(State(app): State<OpenShop>, RawQuery(raw): RawQuery) -> ApiResult {
    const KEEP: [&str; 3] = ["range", "q", "state"];
    let params = Params::new(raw.as_deref());
    let range = Range::parse(params.get("range"));
    let start = range.start(Utc::now());
    let state = params.get("state").filter(|state| SHOP_STATES.contains(state));
    let shops = queries::shops(app.db(), start, params.get("q"), false, state, MAX_SHOPS).await?;

    let with_app = several_apps(&app);
    let rows: Vec<Row> = shops.iter().map(|shop| shop_row(shop, with_app)).collect();
    let form = filters(
        SHOPS,
        range,
        &[
            search("q", "Search a shop", params.get("q")),
            select("state", "State", &SHOP_STATES, state),
        ],
    );
    let active = chips(&params, SHOPS, &KEEP, &[("state", "State is"), ("q", "Shop has")]);
    let cut = if shops.len() as i64 == MAX_SHOPS {
        format!(
            "<div class=\"more\">{}</div>",
            muted(&format!("The first {MAX_SHOPS} installations are shown."))
        )
    } else {
        String::new()
    };
    let content = [
        panel(
            &format!("Installations, with their runs of {}", range.label()),
            &format!(
                "{form}{active}{}{cut}",
                table(&shop_headers(with_app), &rows, "No installation matches.")
            ),
        ),
        note("Times are in UTC. The shops with the most failures come first."),
    ]
    .concat();
    Ok(ok(page(Page {
        section: Section::Shops,
        trail: &[("Shops", None)],
        toolbar: &ranges(&params, SHOPS, &KEEP, range),
        refresh_seconds: None,
        content: &content,
    })))
}

/// `/ops/shops/:shop`: a shop, with its installation, its providers and its runs.
pub(super) async fn shop(State(app): State<OpenShop>, Path(domain): Path<String>, RawQuery(raw): RawQuery) -> ApiResult {
    let db = app.db();
    let params = Params::new(raw.as_deref());
    let range = Range::parse(params.get("range"));
    let now = Utc::now();
    let start = range.start(now);
    let installations = queries::shops(db, start, Some(&domain), true, None, MAX_SHOPS).await?;
    if installations.is_empty() {
        return Ok(not_found_page());
    }
    let of_shop = RunFilter {
        since: Some(start),
        shop: Some(domain.clone()),
        exact_shop: true,
        ..Default::default()
    };
    let counts = queries::run_counts(db, &of_shop, start, range.bucket(), range.buckets(now)).await?;
    let latest = queries::runs(db, &of_shop, FEW).await?;
    let saved = queries::providers_of(db, &domain).await?;
    let crons_off = queries::crons_off_for(db, &domain).await?;

    let with_app = several_apps(&app);
    let (runs, failed) = installations
        .iter()
        .fold((0, 0), |(runs, failed), shop| (runs + shop.runs, failed + shop.failed));

    let installation_rows: Vec<Row> = installations
        .iter()
        .map(|shop| {
            let (state, since) = shop.state();
            let mut cells: Vec<Cell> = Vec::new();
            if with_app {
                cells.push(escape(&shop.app_handle).into());
            }
            cells.extend([
                status(state).into(),
                full_time(Some(since)).into(),
                wide(mono(shop.scopes.as_deref().unwrap_or_default())),
            ]);
            row(cells)
        })
        .collect();
    let mut installation_headers = vec!["State", "Since", "Scopes"];
    if with_app {
        installation_headers.insert(0, "App");
    }

    // What the shop saved for each provider of the application: whether, never what.
    let provider_rows: Vec<Row> = installations
        .iter()
        .flat_map(|installation| {
            app.config()
                .providers
                .keys()
                .map(move |provider| (installation, provider))
        })
        .map(|(installation, provider)| {
            let saved = saved
                .iter()
                .find(|state| state.app_handle == installation.app_handle && &state.provider_name == provider);
            let mut cells: Vec<Cell> = vec![mono(provider).into()];
            if with_app {
                cells.push(escape(&installation.app_handle).into());
            }
            cells.extend(match saved {
                None => [
                    status("not configured").into(),
                    muted("—").into(),
                    muted("—").into(),
                ],
                Some(state) => [
                    match state.last_check_ok {
                        Some(true) => status_as("completed", "check passed"),
                        Some(false) => status_as("failed", "check failed"),
                        None => status_as("pending", "never checked"),
                    }
                    .into(),
                    full_time(state.last_checked_at.as_ref()).into(),
                    full_time(Some(&state.updated_at)).into(),
                ],
            } as [Cell; 3]);
            row(cells)
        })
        .collect();
    let mut provider_headers = vec!["Provider", "State", "Last check", "Saved"];
    if with_app {
        provider_headers.insert(1, "App");
    }

    let crons_off_rows: Vec<Row> = crons_off.iter().map(|key| row(vec![mono(key).into()])).collect();
    let shop_query = encode_uri_component(&domain);
    let content = [
        kpis(&[
            kpi("Runs", &grouped(runs), range.label(), false),
            kpi(
                "Failed",
                &grouped(failed),
                &format!("{} of the runs", share(failed, runs)),
                failed > 0,
            ),
        ]),
        panel(
            &format!("Runs, {}", range.label()),
            &chart(start, range, &counts, "runs", Some("failed")),
        ),
        columns(&[
            panel(
                "Installation",
                &table(&installation_headers, &installation_rows, "No installation."),
            ),
            panel(
                "Providers",
                &table(
                    &provider_headers,
                    &provider_rows,
                    "The application has no provider.",
                ),
            ),
            panel(
                "Crons turned off",
                &table(&["Cron"], &crons_off_rows, "The shop turned no cron off."),
            ),
        ]),
        panel(
            "Latest runs",
            &format!(
                "{}<div class=\"more\">{} · {}</div>",
                run_table(&latest, with_app, "The shop ran nothing over the period."),
                link(
                    &format!("{RUNS}?range={}&shop={shop_query}", range.key()),
                    "Every run of this shop"
                ),
                link(
                    &format!("{LOGS}?range={}&shop={shop_query}", range.key()),
                    "Its log lines"
                ),
            ),
        ),
        note("Times are in UTC."),
    ]
    .concat();
    let path = format!("{SHOPS}/{shop_query}");
    Ok(ok(page(Page {
        section: Section::Shops,
        trail: &[("Shops", Some(SHOPS)), (&domain, None)],
        toolbar: &ranges(&params, &path, &["range"], range),
        refresh_seconds: None,
        content: &content,
    })))
}

/// `/ops/crons`: the schedules of the application, and when each fires next.
pub(super) async fn crons(State(app): State<OpenShop>) -> ApiResult {
    let config = app.config();
    let turned_off = queries::crons_off(app.db()).await?;
    let now = Utc::now();
    let rows: Vec<Row> = config
        .crons
        .iter()
        .map(|cron| {
            // The scheduler reads a schedule in UTC, or in the time zone of the cron.
            let next = Some(&cron.flow)
                .filter(|flow| config.flows.contains_key(*flow))
                .and_then(|_| next_occurrence(cron, now))
                .map(|next| time(&next))
                .unwrap_or_else(|| "never: the flow is not registered".to_string());
            let shops = match cron.shops.clone() {
                CronShops::Global => "once, without a shop".to_string(),
                CronShops::All => "every installed shop".to_string(),
                CronShops::Shop(shop) => shop,
                CronShops::Shops(shops) => shops.join(", "),
            };
            let key = cron.key();
            let off = turned_off
                .iter()
                .find(|(turned_off, _)| *turned_off == key)
                .map_or(0, |(_, shops)| *shops);
            row(vec![
                escape(cron.name.as_deref().unwrap_or(&cron.flow)).into(),
                runs_of_flow_link(&cron.flow, Range::Day).into(),
                mono(&cron.schedule).into(),
                wide(escape(&shops)),
                escape(&next).into(),
                match off {
                    0 => muted("no shop").into(),
                    1 => "1 shop".to_string().into(),
                    shops => format!("{} shops", grouped(shops)).into(),
                },
            ])
            .to(runs_of_flow_href(&cron.flow, Range::Day))
        })
        .collect();

    let content = [
        panel(
            "Schedules",
            &table(
                &["Cron", "Flow", "Schedule", "Runs for", "Next run (UTC)", "Turned off by"],
                &rows,
                "The application has no cron.",
            ),
        ),
        note("A shop that turned a cron off is skipped: the next run is the one of the schedule."),
    ]
    .concat();
    Ok(ok(page(Page {
        section: Section::Crons,
        trail: &[("Crons", None)],
        toolbar: "",
        refresh_seconds: None,
        content: &content,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_keeps_the_filters_of_the_request_and_applies_the_changes() {
        let params = Params::new(Some("range=7d&status=failed&shop=one.myshopify.com&q=+&other=x"));
        let keep = ["range", "status", "flow", "shop", "q"];
        assert_eq!(
            params.href("/ops/runs", &keep, &[]),
            "/ops/runs?range=7d&status=failed&shop=one.myshopify.com"
        );
        assert_eq!(
            params.href("/ops/runs", &keep, &[("status", None), ("before", None)]),
            "/ops/runs?range=7d&shop=one.myshopify.com"
        );
        assert_eq!(
            params.href("/ops/runs", &keep, &[("range", Some("1h")), ("before", Some("2026-10-11T08:00:00Z"))]),
            "/ops/runs?status=failed&shop=one.myshopify.com&range=1h&before=2026-10-11T08%3A00%3A00Z"
        );
        assert_eq!(Params::new(None).href("/ops", &["range"], &[]), "/ops");
    }

    #[test]
    fn the_active_filters_are_links_that_remove_them() {
        let params = Params::new(Some("range=7d&status=failed&q=%3Cb%3E"));
        let keep = ["range", "status", "q"];
        assert_eq!(
            chips(&params, "/ops/runs", &keep, &[("status", "Status is"), ("flow", "Flow is"), ("q", "Error has")]),
            "<div class=\"chips\">\
             <a class=\"chip\" href=\"/ops/runs?range=7d&amp;q=%3Cb%3E\" title=\"Remove this filter\">Status is <b>failed</b> ×</a>\
             <a class=\"chip\" href=\"/ops/runs?range=7d&amp;status=failed\" title=\"Remove this filter\">Error has <b>&lt;b&gt;</b> ×</a>\
             </div>"
        );
        assert_eq!(chips(&Params::new(None), "/ops/runs", &keep, &[("status", "Status is")]), "");
    }

    #[test]
    fn the_periods_keep_the_filters_and_mark_the_current_one() {
        let params = Params::new(Some("status=failed&before=2026-10-11T08:00:00Z"));
        let html = ranges(&params, "/ops/runs", &["range", "status"], Range::Day);
        assert!(html.contains("<a href=\"/ops/runs?status=failed&amp;range=24h\" aria-current=\"true\">24h</a>"), "{html}");
        assert!(html.contains("<a href=\"/ops/runs?status=failed&amp;range=7d\">7d</a>"), "{html}");
        assert!(!html.contains("before"), "{html}");
    }

    #[test]
    fn a_share_is_a_percentage_of_what_there_is() {
        assert_eq!(share(1, 40), "2.5%");
        assert_eq!(share(0, 40), "0.0%");
        assert_eq!(share(0, 0), "—");
    }
}
