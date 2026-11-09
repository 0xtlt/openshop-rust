//! The HTML of the operator dashboard: pages rendered by the server, without
//! script, in a frame of its own: a menu on the side, a trail and a period on
//! top, then panels of charts and dense tables. Everything a page shows goes
//! through [`escape`].

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::server::http::escape;

/// The longest JSON value a page shows in full.
const MAX_JSON_CHARS: usize = 20_000;

const STYLE: &str = "
:root { color-scheme: light dark; --bg: #f4f6f5; --side: #fbfcfc; --panel: #fff; --line: #dde3e1; --ink: #16201e;
  --muted: #5d6b67; --faint: #eef2f1; --accent: #0b7785; --ok: #1c7a4e; --bad: #b5382b; --warn: #8a5a00;
  --chart: #0b7785; --chart-fill: rgba(11, 119, 133, .12); }
@media (prefers-color-scheme: dark) { :root { --bg: #0c1110; --side: #101615; --panel: #131a18; --line: #222c29;
  --ink: #e3e9e7; --muted: #8a9793; --faint: #182120; --accent: #62c7d4; --ok: #4cc38a; --bad: #f0776a;
  --warn: #e2ae55; --chart: #62c7d4; --chart-fill: rgba(98, 199, 212, .14); } }
* { box-sizing: border-box; }
html, body { height: 100%; }
body { margin: 0; background: var(--bg); color: var(--ink); font: 13px/1.45 system-ui, -apple-system, 'Segoe UI', sans-serif; }
a { color: var(--accent); text-decoration: none; }
a:hover { text-decoration: underline; }
.app { display: grid; grid-template-columns: 220px minmax(0, 1fr); min-height: 100%; }
.side { background: var(--side); border-right: 1px solid var(--line); padding: 16px 12px; display: flex;
  flex-direction: column; gap: 16px; position: sticky; top: 0; height: 100vh; }
.brand { padding: 4px 10px; font-weight: 600; font-size: 14px; color: var(--ink); }
.brand span { color: var(--ok); }
.side nav { display: flex; flex-direction: column; gap: 2px; flex: 1; }
.side nav a { color: var(--ink); padding: 6px 10px; border-left: 2px solid transparent; }
.side nav a:hover { background: var(--faint); text-decoration: none; }
.side nav a[aria-current] { background: var(--faint); font-weight: 600; border-left-color: var(--ok); }
.side form button { width: 100%; }
main { min-width: 0; }
.top { display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: 8px 16px;
  padding: 14px 24px; border-bottom: 1px solid var(--line); }
.trail { display: flex; flex-wrap: wrap; gap: 8px; align-items: baseline; color: var(--muted); font-size: 14px; }
.trail a { color: var(--muted); }
.trail strong { color: var(--ink); font-size: 16px; font-weight: 600; overflow-wrap: anywhere; }
.ranges { display: flex; border: 1px solid var(--line); border-radius: 6px; overflow: hidden; }
.ranges a { padding: 4px 10px; color: var(--muted); border-right: 1px solid var(--line); }
.ranges a:last-child { border-right: 0; }
.ranges a[aria-current] { background: var(--faint); color: var(--ink); font-weight: 600; }
.content { padding: 20px 24px 40px; display: flex; flex-direction: column; gap: 16px; }
.panel { background: var(--panel); border: 1px solid var(--line); border-radius: 8px; min-width: 0; overflow: hidden; }
.panel > h2 { margin: 0; padding: 10px 14px; font-size: 13px; font-weight: 600; }
.panel > .body { padding: 4px 14px 14px; }
.cols { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(420px, 100%), 1fr)); gap: 16px; }
.kpis { display: grid; grid-template-columns: repeat(auto-fit, minmax(150px, 1fr)); gap: 12px; }
.kpi { background: var(--panel); border: 1px solid var(--line); border-radius: 8px; padding: 12px 14px; }
.kpi .label, .kpi .hint { color: var(--muted); font-size: 12px; }
.kpi .value { font-size: 24px; font-weight: 600; line-height: 1.3; font-variant-numeric: tabular-nums; }
.kpi.bad .value { color: var(--bad); }
.scroll { overflow-x: auto; }
table { border-collapse: collapse; width: 100%; font-variant-numeric: tabular-nums; }
th { text-align: left; font-weight: 500; font-size: 12px; color: var(--muted); padding: 7px 14px;
  background: var(--faint); white-space: nowrap; }
td { padding: 7px 14px; border-top: 1px solid var(--line); white-space: nowrap; vertical-align: top; }
tr:hover td { background: var(--faint); }
td.wide { white-space: normal; overflow-wrap: anywhere; min-width: 260px; }
td.go { padding: 0; }
td.go > a { display: block; padding: 7px 14px; color: inherit; }
td.go > a:hover { text-decoration: none; }
td.go > a:empty::before { content: '\\00a0'; }
tr.link { cursor: pointer; }
.mono { font-family: ui-monospace, 'SF Mono', Menlo, Consolas, monospace; font-size: 12px; }
.muted, .empty { color: var(--muted); }
.empty { padding: 14px; margin: 0; }
.bad { color: var(--bad); }
.st { display: inline-flex; align-items: center; gap: 6px; }
.st svg { width: 14px; height: 14px; flex: none; fill: none; stroke: currentColor; stroke-width: 1.5;
  stroke-linecap: round; stroke-linejoin: round; }
.st.ok { color: var(--ok); }
.st.bad { color: var(--bad); }
.st.warn { color: var(--warn); }
.st.idle { color: var(--muted); }
.st span { color: var(--ink); }
.filters { display: flex; flex-wrap: wrap; gap: 8px; padding: 10px 14px; border-top: 1px solid var(--line); }
input, select, button { font: inherit; color: inherit; background: var(--bg); border: 1px solid var(--line);
  border-radius: 6px; padding: 5px 9px; }
input[type=search] { flex: 1; min-width: 180px; }
button { cursor: pointer; background: var(--faint); font-weight: 600; }
.chips { display: flex; flex-wrap: wrap; gap: 6px; padding: 0 14px 10px; }
.chip { display: inline-flex; gap: 6px; align-items: center; padding: 2px 8px; border: 1px solid var(--line);
  border-radius: 6px; background: var(--faint); color: var(--ink); font-size: 12px; }
.chip b { color: var(--ok); font-weight: 600; }
.chart { display: block; width: 100%; height: auto; }
.chart.narrow { display: none; }
.chart .grid { stroke: var(--line); stroke-width: 1; }
.chart .axis { fill: var(--muted); font-size: 11px; }
.chart .line { fill: none; stroke-width: 2; stroke-linejoin: round; }
.chart .line.total { stroke: var(--chart); }
.chart .line.bad { stroke: var(--bad); }
.chart .area { fill: var(--chart-fill); }
.chart .hit { fill: var(--ink); opacity: 0; }
.chart .hit:hover { opacity: .07; }
.legend { display: flex; gap: 16px; padding: 0 14px 12px; color: var(--muted); font-size: 12px; }
.legend i { display: inline-block; width: 12px; height: 2px; vertical-align: middle; margin-right: 6px;
  background: var(--chart); }
.legend i.bad { background: var(--bad); }
.note { color: var(--muted); font-size: 12px; margin: 0; }
.more { padding: 10px 14px; border-top: 1px solid var(--line); }
pre { margin: 0; padding: 12px 14px; overflow-x: auto; white-space: pre-wrap; overflow-wrap: anywhere;
  font: 12px/1.5 ui-monospace, 'SF Mono', Menlo, Consolas, monospace; }
td pre { padding: 6px 0 0; }
summary { cursor: pointer; color: var(--muted); }
dl { display: grid; grid-template-columns: max-content minmax(0, 1fr); gap: 6px 24px; margin: 0; }
dt { color: var(--muted); }
dd { margin: 0; overflow-wrap: anywhere; }
.login { max-width: 340px; margin: 14vh auto; padding: 24px; background: var(--panel); border: 1px solid var(--line);
  border-radius: 10px; display: flex; flex-direction: column; gap: 14px; }
.login h1 { margin: 0; font-size: 16px; }
.login p { margin: 0; }
.login form { display: flex; flex-direction: column; gap: 10px; }
label { display: flex; flex-direction: column; gap: 4px; color: var(--muted); font-size: 12px; }
.error { color: var(--bad); }
:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
@media (max-width: 760px) {
  .app { grid-template-columns: minmax(0, 1fr); }
  .side { position: static; height: auto; flex-direction: row; flex-wrap: wrap; align-items: center; gap: 8px;
    padding: 10px 12px; border-right: 0; border-bottom: 1px solid var(--line); }
  .brand { flex: 1; }
  .side form button { width: auto; }
  .side nav { order: 3; flex: 1 0 100%; flex-direction: row; gap: 4px; overflow-x: auto; }
  .side nav a { white-space: nowrap; border-left: 0; border-bottom: 2px solid transparent; }
  .side nav a[aria-current] { border-bottom-color: var(--ok); }
  .top { padding: 12px; }
  .content { padding: 12px 12px 32px; gap: 12px; }
  .chart.wide { display: none; }
  .chart.narrow { display: block; }
  .filters > * { flex: 1 1 140px; min-width: 0; }
  /* A table becomes a list of cards: a line per cell, under its column name. */
  .scroll { overflow: visible; }
  table, tbody, tr, td { display: block; }
  thead { display: none; }
  tr { padding: 8px 0; border-top: 1px solid var(--line); }
  tr:hover td { background: none; }
  td, td.go > a { display: grid; grid-template-columns: 104px minmax(0, 1fr); gap: 8px; padding: 3px 14px;
    white-space: normal; overflow-wrap: anywhere; }
  td { border: 0; }
  td.wide { min-width: 0; }
  td.go { display: block; padding: 0; }
  td::before, td.go > a::before { content: attr(data-label); color: var(--muted); font-size: 12px; }
  td.go::before { content: none; }
  td.go > a:empty::before { content: attr(data-label); }
  dl { grid-template-columns: minmax(0, 1fr); gap: 0; }
  dd { margin-bottom: 8px; }
}
";

/// The pages of the menu, with their address.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Section {
    Overview,
    Runs,
    Logs,
    Shops,
    Flows,
    Crons,
}

const SECTIONS: [(Section, &str, &str); 6] = [
    (Section::Overview, "/ops", "Overview"),
    (Section::Runs, "/ops/runs", "Runs"),
    (Section::Logs, "/ops/logs", "Logs"),
    (Section::Shops, "/ops/shops", "Shops"),
    (Section::Flows, "/ops/flows", "Flows"),
    (Section::Crons, "/ops/crons", "Crons"),
];

fn document(title: &str, head: &str, body: &str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <meta name=\"robots\" content=\"noindex,nofollow\">{head}\
         <title>{} · OpenShop ops</title><style>{STYLE}</style></head><body>{body}</body></html>",
        escape(title)
    )
}

/// A page of the dashboard, in its frame.
pub(crate) struct Page<'a> {
    pub section: Section,
    /// Where the page is, from the menu entry down: the text of each step,
    /// and the address of all but the last.
    pub trail: &'a [(&'a str, Option<&'a str>)],
    /// Markup at the end of the top bar: the periods of a page that has some.
    pub toolbar: &'a str,
    /// Reloads the page on its own.
    pub refresh_seconds: Option<u32>,
    pub content: &'a str,
}

pub(crate) fn page(page: Page<'_>) -> String {
    let menu: String = SECTIONS
        .iter()
        .map(|(candidate, href, label)| {
            let current = if *candidate == page.section {
                " aria-current=\"page\""
            } else {
                ""
            };
            format!("<a href=\"{href}\"{current}>{label}</a>")
        })
        .collect();
    let last = page.trail.len().saturating_sub(1);
    let trail: String = page
        .trail
        .iter()
        .enumerate()
        .map(|(index, (text, href))| match href {
            Some(href) if index < last => format!("<a href=\"{}\">{}</a><span>/</span>", escape(href), escape(text)),
            _ => format!("<strong>{}</strong>", escape(text)),
        })
        .collect();
    let title = page.trail.last().map_or("Overview", |(text, _)| text);
    let head = page
        .refresh_seconds
        .map(|seconds| format!("<meta http-equiv=\"refresh\" content=\"{seconds}\">"))
        .unwrap_or_default();
    let body = format!(
        "<div class=\"app\"><aside class=\"side\"><a class=\"brand\" href=\"/ops\">OpenShop <span>ops</span></a>\
         <nav>{menu}</nav>\
         <form method=\"post\" action=\"/ops/logout\"><button type=\"submit\">Sign out</button></form></aside>\
         <main><header class=\"top\"><div class=\"trail\">{trail}</div>{}</header>\
         <div class=\"content\">{}</div></main></div>",
        page.toolbar, page.content
    );
    document(title, &head, &body)
}

/// The sign-in form, with what went wrong at the last attempt.
pub(crate) fn login_page(problem: Option<&str>) -> String {
    let problem = problem
        .map(|problem| format!("<p class=\"error\" role=\"alert\">{}</p>", escape(problem)))
        .unwrap_or_default();
    let body = format!(
        "<main class=\"login\"><h1>OpenShop ops</h1>{problem}\
         <form method=\"post\" action=\"/ops/login\">\
         <label>Password<input type=\"password\" name=\"password\" autocomplete=\"current-password\" required autofocus></label>\
         <button type=\"submit\">Sign in</button></form></main>"
    );
    document("Sign in", "", &body)
}

/// A page without menu, for what the dashboard does not have.
pub(crate) fn message_page(title: &str, message: &str) -> String {
    let body = format!(
        "<main class=\"login\"><h1>{}</h1><p>{}</p><p><a href=\"/ops\">Back to the overview</a></p></main>",
        escape(title),
        escape(message)
    );
    document(title, "", &body)
}

/// `Oct 11, 09:42:07`, in UTC: a line of a table.
pub(crate) fn time(value: &DateTime<Utc>) -> String {
    value.format("%b %d, %H:%M:%S").to_string()
}

/// `2026-10-11 09:42:07 UTC`: the summary of a run.
pub(crate) fn full_time(value: Option<&DateTime<Utc>>) -> String {
    value
        .map(|value| value.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_else(|| "—".to_string())
}

/// `850 ms`, `4.2 s`, `3 min 12 s`, `2 h 05 min`.
pub(crate) fn duration(milliseconds: i64) -> String {
    let seconds = milliseconds / 1000;
    match milliseconds {
        ..1_000 => format!("{milliseconds} ms"),
        1_000..60_000 => format!("{:.1} s", milliseconds as f64 / 1000.0),
        60_000..3_600_000 => format!("{} min {:02} s", seconds / 60, seconds % 60),
        _ => format!("{} h {:02} min", seconds / 3600, seconds % 3600 / 60),
    }
}

const CHECK: &str = "<path d=\"M5 8.2l2 2 4-4.4\"/>";
const CROSS: &str = "<path d=\"M5.6 5.6l4.8 4.8M10.4 5.6l-4.8 4.8\"/>";
const DOT: &str = "<circle cx=\"8\" cy=\"8\" r=\"2.2\" fill=\"currentColor\" stroke=\"none\"/>";
const CLOCK: &str = "<path d=\"M8 4.8V8l2 1.4\"/>";
const DASH: &str = "<path d=\"M5 8h6\"/>";
const BANG: &str = "<path d=\"M8 4.6v4M8 11v.2\"/>";

fn marked(tone: &str, mark: &str, text: &str) -> String {
    format!(
        "<span class=\"st {tone}\"><svg viewBox=\"0 0 16 16\" aria-hidden=\"true\"><circle cx=\"8\" cy=\"8\" r=\"6.5\"/>{mark}</svg>\
         <span>{}</span></span>",
        escape(text)
    )
}

/// The status of a run, of a step or of an installation: a mark and its name.
pub(crate) fn status(status: &str) -> String {
    status_as(status, status)
}

/// The mark of a status, next to other words than its name.
pub(crate) fn status_as(status: &str, text: &str) -> String {
    let (tone, mark) = match status {
        "completed" | "installed" => ("ok", CHECK),
        "failed" => ("bad", CROSS),
        "running" => ("warn", DOT),
        "pending" | "sleeping" => ("warn", CLOCK),
        _ => ("idle", DASH),
    };
    marked(tone, mark, text)
}

/// The level of a log line.
pub(crate) fn level(level: &str) -> String {
    let (tone, mark) = match level {
        "error" => ("bad", CROSS),
        "warn" => ("warn", BANG),
        _ => ("idle", DOT),
    };
    marked(tone, mark, level)
}

/// An error message, or nothing when there is none.
pub(crate) fn error_text(error: Option<&str>) -> String {
    error
        .filter(|error| !error.is_empty())
        .map(|error| format!("<span class=\"bad mono\">{}</span>", escape(error)))
        .unwrap_or_default()
}

pub(crate) fn muted(text: &str) -> String {
    format!("<span class=\"muted\">{}</span>", escape(text))
}

/// Text in the type of a terminal: names of flows and steps, ids, messages.
pub(crate) fn mono(text: &str) -> String {
    format!("<span class=\"mono\">{}</span>", escape(text))
}

pub(crate) fn link(href: &str, text: &str) -> String {
    format!("<a href=\"{}\">{}</a>", escape(href), escape(text))
}

/// A JSON value on several lines, cut when it is very long.
pub(crate) fn json_block(value: &Value) -> String {
    let pretty = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    let shown = match pretty.char_indices().nth(MAX_JSON_CHARS) {
        Some((cut, _)) => format!("{}\n… cut after {MAX_JSON_CHARS} characters", &pretty[..cut]),
        None => pretty,
    };
    format!("<pre>{}</pre>", escape(&shown))
}

/// A panel of the page, under its title. `content` is markup, edge to edge:
/// a table, a chart, or a [`body`].
pub(crate) fn panel(title: &str, content: &str) -> String {
    format!("<section class=\"panel\"><h2>{}</h2>{content}</section>", escape(title))
}

/// Content of a panel that keeps away from its edges.
pub(crate) fn body(content: &str) -> String {
    format!("<div class=\"body\">{content}</div>")
}

/// Panels side by side, when the page is wide enough.
pub(crate) fn columns(panels: &[String]) -> String {
    format!("<div class=\"cols\">{}</div>", panels.concat())
}

/// A number of the page, with what it counts and what it means.
pub(crate) fn kpi(label: &str, value: &str, hint: &str, bad: bool) -> String {
    let class = if bad { "kpi bad" } else { "kpi" };
    format!(
        "<div class=\"{class}\"><div class=\"label\">{}</div><div class=\"value\">{}</div><div class=\"hint\">{}</div></div>",
        escape(label),
        escape(value),
        escape(hint)
    )
}

pub(crate) fn kpis(cards: &[String]) -> String {
    format!("<div class=\"kpis\">{}</div>", cards.concat())
}

/// Label and value pairs, the label being text and the value markup.
pub(crate) fn definitions(pairs: &[(&str, String)]) -> String {
    let pairs: String = pairs
        .iter()
        .map(|(label, value)| format!("<dt>{}</dt><dd>{value}</dd>", escape(label)))
        .collect();
    format!("<dl>{pairs}</dl>")
}

/// A cell that takes the room its text needs, on several lines.
pub(crate) fn wide(content: String) -> Cell {
    Cell { wide: true, content }
}

/// A cell of a table: markup, on one line unless it is [`wide`].
pub(crate) struct Cell {
    wide: bool,
    content: String,
}

impl From<String> for Cell {
    fn from(content: String) -> Self {
        Self { wide: false, content }
    }
}

impl Cell {
    /// Whether the cell has something of its own to click: a link, or
    /// details to open. Text never does: its `<` is escaped.
    fn acts(&self) -> bool {
        self.content.contains("<a ") || self.content.contains("<details")
    }
}

/// A row of a table, which can lead somewhere: the whole row is then a link.
pub(crate) struct Row {
    href: Option<String>,
    cells: Vec<Cell>,
}

pub(crate) fn row(cells: Vec<Cell>) -> Row {
    Row { href: None, cells }
}

impl Row {
    /// Makes the row lead to `href`, wherever it is clicked. A cell that has
    /// a link of its own keeps it.
    pub(crate) fn to(mut self, href: impl Into<String>) -> Self {
        self.href = Some(href.into());
        self
    }

    /// The cells, each named after its column for the narrow screens that
    /// show a row as a card.
    fn render(&self, headers: &[&str]) -> String {
        let href = self.href.as_deref().map(escape);
        // Without a link of its own to where the row leads, its first cell is
        // the one a keyboard reaches; the others repeat it for the pointer.
        let mut reachable = href.as_ref().is_some_and(|href| {
            self.cells
                .iter()
                .any(|cell| cell.content.contains(&format!("href=\"{href}\"")))
        });
        let cells: String = self
            .cells
            .iter()
            .enumerate()
            .map(|(index, cell)| {
                let label = escape(headers.get(index).copied().unwrap_or_default());
                let wide = if cell.wide { " wide" } else { "" };
                match &href {
                    Some(href) if !cell.acts() => {
                        let repeated = if std::mem::replace(&mut reachable, true) {
                            " tabindex=\"-1\" aria-hidden=\"true\""
                        } else {
                            ""
                        };
                        format!(
                            "<td class=\"go{wide}\"><a href=\"{href}\" data-label=\"{label}\"{repeated}>{}</a></td>",
                            cell.content
                        )
                    }
                    _ => {
                        let class = if cell.wide { " class=\"wide\"" } else { "" };
                        format!("<td{class} data-label=\"{label}\">{}</td>", cell.content)
                    }
                }
            })
            .collect();
        let class = if href.is_some() { " class=\"link\"" } else { "" };
        format!("<tr{class}>{cells}</tr>")
    }
}

/// A table that scrolls sideways when it is wider than the page, or `empty`
/// when it has no row. On a narrow screen each row is a card.
pub(crate) fn table(headers: &[&str], rows: &[Row], empty: &str) -> String {
    if rows.is_empty() {
        return self::empty(empty);
    }
    let head: String = headers
        .iter()
        .map(|header| format!("<th>{}</th>", escape(header)))
        .collect();
    let body: String = rows.iter().map(|row| row.render(headers)).collect();
    format!("<div class=\"scroll\"><table><thead><tr>{head}</tr></thead><tbody>{body}</tbody></table></div>")
}

/// What a panel says when it has nothing to show.
pub(crate) fn empty(text: &str) -> String {
    format!("<p class=\"empty\">{}</p>", escape(text))
}

/// What the lines of a chart stand for: the first one, and the one of what
/// went wrong when the chart has it.
pub(crate) fn legend(total: &str, bad: Option<&str>) -> String {
    let bad = bad
        .map(|bad| format!("<span><i class=\"bad\"></i>{}</span>", escape(bad)))
        .unwrap_or_default();
    format!("<div class=\"legend\"><span><i></i>{}</span>{bad}</div>", escape(total))
}

pub(crate) fn note(text: &str) -> String {
    format!("<p class=\"note\">{}</p>", escape(text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    #[test]
    fn escapes_what_html_reads_as_markup() {
        assert_eq!(
            escape(r#"<script>alert("x")</script> & 'y'"#),
            "&lt;script&gt;alert(&quot;x&quot;)&lt;/script&gt; &amp; &#39;y&#39;"
        );
        assert_eq!(escape("café → ok"), "café → ok");
    }

    #[test]
    fn a_page_sits_in_its_frame_and_runs_no_script() {
        let html = page(Page {
            section: Section::Runs,
            trail: &[("Runs", Some("/ops/runs")), ("sync <1>", None)],
            toolbar: "<div class=\"ranges\"></div>",
            refresh_seconds: None,
            content: "<p>content</p>",
        });
        assert!(html.contains("<title>sync &lt;1&gt; · OpenShop ops</title>"), "{html}");
        assert!(
            html.contains("<a href=\"/ops/runs\" aria-current=\"page\">Runs</a>"),
            "{html}"
        );
        assert!(html.contains("<a href=\"/ops/logs\">Logs</a>"), "{html}");
        assert!(
            html.contains("<div class=\"trail\"><a href=\"/ops/runs\">Runs</a><span>/</span><strong>sync &lt;1&gt;</strong></div><div class=\"ranges\"></div>"),
            "{html}"
        );
        assert!(html.contains("<form method=\"post\" action=\"/ops/logout\">"), "{html}");
        assert!(html.contains("<div class=\"content\"><p>content</p></div>"), "{html}");
        assert!(!html.contains("http-equiv"), "{html}");
        assert!(!html.contains("<script"), "{html}");

        let refreshed = page(Page {
            section: Section::Overview,
            trail: &[("Overview", None)],
            toolbar: "",
            refresh_seconds: Some(30),
            content: "",
        });
        assert!(
            refreshed.contains("<meta http-equiv=\"refresh\" content=\"30\">"),
            "{refreshed}"
        );
        assert!(
            refreshed.contains("<title>Overview · OpenShop ops</title>"),
            "{refreshed}"
        );
    }

    #[test]
    fn the_sign_in_form_posts_the_password_and_shows_the_problem() {
        let html = login_page(Some("Wrong <password>."));
        assert!(html.contains("<form method=\"post\" action=\"/ops/login\">"), "{html}");
        assert!(html.contains("<input type=\"password\" name=\"password\""), "{html}");
        assert!(
            html.contains("<p class=\"error\" role=\"alert\">Wrong &lt;password&gt;.</p>"),
            "{html}"
        );
        assert!(!login_page(None).contains("role=\"alert\""));
        assert!(!html.contains("<script"), "{html}");
    }

    #[test]
    fn json_is_escaped_and_cut_when_very_long() {
        assert_eq!(
            json_block(&json!({ "html": "<b>" })),
            "<pre>{\n  &quot;html&quot;: &quot;&lt;b&gt;&quot;\n}</pre>"
        );
        let long = json_block(&json!("é".repeat(MAX_JSON_CHARS + 50)));
        assert!(long.ends_with("… cut after 20000 characters</pre>"));
    }

    #[test]
    fn tables_escape_headers_and_say_when_they_are_empty() {
        assert_eq!(
            table(&["Shop"], &[], "No <shop> yet."),
            "<p class=\"empty\">No &lt;shop&gt; yet.</p>"
        );
        let rows = [row(vec!["1".to_string().into(), wide("<b>2</b>".to_string())])];
        assert_eq!(
            table(&["A & B", "C"], &rows, "empty"),
            "<div class=\"scroll\"><table><thead><tr><th>A &amp; B</th><th>C</th></tr></thead>\
             <tbody><tr><td data-label=\"A &amp; B\">1</td><td class=\"wide\" data-label=\"C\"><b>2</b></td></tr>\
             </tbody></table></div>"
        );
    }

    #[test]
    fn a_row_that_leads_somewhere_is_a_link_wherever_it_is_clicked() {
        // The row has its own link to where it leads: the other cells repeat it for the pointer.
        let with_link = row(vec![
            link("/ops/runs/1", "sync"),
            "failed".to_string(),
            link("/ops/shops/a", "a"),
            "<details><summary>Output</summary></details>".to_string(),
        ]
        .into_iter()
        .map(Cell::from)
        .collect())
        .to("/ops/runs/1");
        assert_eq!(
            with_link.render(&["Flow", "Status", "Shop", "Result"]),
            "<tr class=\"link\"><td data-label=\"Flow\"><a href=\"/ops/runs/1\">sync</a></td>\
             <td class=\"go\"><a href=\"/ops/runs/1\" data-label=\"Status\" tabindex=\"-1\" aria-hidden=\"true\">failed</a></td>\
             <td data-label=\"Shop\"><a href=\"/ops/shops/a\">a</a></td>\
             <td data-label=\"Result\"><details><summary>Output</summary></details></td></tr>"
        );

        // Without one, the first cell is the link a keyboard reaches.
        let plain = row(vec!["sync".to_string().into(), wide("12".to_string())]).to("/ops/runs?flow=a&b");
        assert_eq!(
            plain.render(&["Flow", "Runs"]),
            "<tr class=\"link\"><td class=\"go\"><a href=\"/ops/runs?flow=a&amp;b\" data-label=\"Flow\">sync</a></td>\
             <td class=\"go wide\"><a href=\"/ops/runs?flow=a&amp;b\" data-label=\"Runs\" tabindex=\"-1\" aria-hidden=\"true\">12</a></td></tr>"
        );
    }

    #[test]
    fn a_status_and_a_level_carry_a_mark_and_a_tone() {
        for (name, tone) in [
            ("completed", "ok"),
            ("installed", "ok"),
            ("failed", "bad"),
            ("running", "warn"),
            ("pending", "warn"),
            ("sleeping", "warn"),
            ("canceled", "idle"),
            ("uninstalled", "idle"),
        ] {
            let html = status(name);
            assert!(html.starts_with(&format!("<span class=\"st {tone}\"><svg")), "{html}");
            assert!(html.ends_with(&format!("<span>{name}</span></span>")), "{html}");
        }
        assert!(status("<x>").ends_with("<span>&lt;x&gt;</span></span>"));
        let passed = status_as("completed", "check passed");
        assert!(passed.starts_with("<span class=\"st ok\">"), "{passed}");
        assert!(passed.ends_with("<span>check passed</span></span>"), "{passed}");
        assert!(level("error").starts_with("<span class=\"st bad\">"));
        assert!(level("warn").starts_with("<span class=\"st warn\">"));
        assert!(level("info").starts_with("<span class=\"st idle\">"));
    }

    #[test]
    fn text_helpers_escape_what_they_show() {
        assert_eq!(error_text(None), "");
        assert_eq!(error_text(Some("")), "");
        assert_eq!(
            error_text(Some("502 <Bad>")),
            "<span class=\"bad mono\">502 &lt;Bad&gt;</span>"
        );
        assert_eq!(
            link("/ops/runs?shop=a&flow=b", "a <shop>"),
            "<a href=\"/ops/runs?shop=a&amp;flow=b\">a &lt;shop&gt;</a>"
        );
        assert_eq!(mono("<id>"), "<span class=\"mono\">&lt;id&gt;</span>");
        assert_eq!(muted("<none>"), "<span class=\"muted\">&lt;none&gt;</span>");
        assert_eq!(
            panel("A & B", "<p>x</p>"),
            "<section class=\"panel\"><h2>A &amp; B</h2><p>x</p></section>"
        );
        assert_eq!(
            kpi("Failed <1>", "7", "2.5% of runs", true),
            "<div class=\"kpi bad\"><div class=\"label\">Failed &lt;1&gt;</div><div class=\"value\">7</div>\
             <div class=\"hint\">2.5% of runs</div></div>"
        );
        assert_eq!(
            definitions(&[("Shop <1>", "<b>x</b>".to_string())]),
            "<dl><dt>Shop &lt;1&gt;</dt><dd><b>x</b></dd></dl>"
        );
    }

    #[test]
    fn times_and_durations_are_short() {
        let at = Utc.with_ymd_and_hms(2026, 10, 11, 9, 42, 7).unwrap();
        assert_eq!(time(&at), "Oct 11, 09:42:07");
        assert_eq!(full_time(Some(&at)), "2026-10-11 09:42:07 UTC");
        assert_eq!(full_time(None), "—");
        assert_eq!(duration(850), "850 ms");
        assert_eq!(duration(4_200), "4.2 s");
        assert_eq!(duration(192_000), "3 min 12 s");
        assert_eq!(duration(7_500_000), "2 h 05 min");
    }
}
