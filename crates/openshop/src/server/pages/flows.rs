//! `/flows` and `/flows/:name`: the flows of the application with the crons
//! that run them, each with a "Run" button that opens a form for its JSON
//! input; and the runs, of every flow or of the one the URL names, searched,
//! filtered by status and date and paged by the query of the URL. Runs are
//! selected with checkboxes and deleted together, after a confirmation. The
//! page is fetched again every 5 seconds while a run it lists is active.

mod format;
mod runs;
mod schema_example;

use anyhow::Result;
use chrono::{DateTime, Utc};
use maud::{Markup, html};
use serde_json::Value;

use super::{BuiltinPage, field};
use crate::admin::{AdminPublicError, FormState, Outcome};
use crate::admin_paths::rpc::{decode_uri_component, encode_uri_component};
use crate::db::schema::FlowRun;
use crate::server::api::runs::{NotStarted, delete_run_batch, query_runs, start_flow};
use crate::server::mcp::core::flow_items;
use runs::{RunsQuery, delete_modal, runs_section};
use schema_example::schema_to_example;

/// The form of a "Run" modal: starts the flow of its field `flow`.
const RUN: &str = "run";
/// The form of the selected runs: deletes them.
const DELETE: &str = "deleteRuns";
/// The field of the JSON input of a run.
const INPUT: &str = "input";
/// The field of a selected run, once per run.
const IDS: &str = "ids";
/// The form the checkboxes of the runs belong to, in the confirmation modal.
const DELETE_FORM: &str = "delete-runs";
const DELETE_MODAL: &str = "delete-runs-modal";
/// Runs per page.
const PAGE_SIZE: usize = 25;
/// How long the page waits before it is fetched again while a run it lists
/// is active, in milliseconds: how often the Leptos page polled.
const REFRESH_MS: &str = "5000";

/// A flow of the application, as its row and its "Run" form show it.
#[derive(Debug, Clone, PartialEq)]
struct FlowItem {
    name: String,
    /// The schedules of the crons of the shop that run it.
    schedules: Vec<String>,
    /// The input its "Run" form starts with: an example of its input schema, or `{}`.
    example: String,
}

/// The flows, as `/api/flows` lists them.
fn flow_list(page: &BuiltinPage<'_>) -> Vec<FlowItem> {
    flow_items(page.app, &page.session.shop, |cron| Value::from(cron.schedule.clone()))
        .into_iter()
        .map(|flow| {
            let example = schema_to_example(flow.get("inputSchema")).map_or_else(
                || "{}".to_string(),
                |example| serde_json::to_string_pretty(&Value::Object(example)).unwrap_or_default(),
            );
            FlowItem {
                name: flow["name"].as_str().unwrap_or_default().to_string(),
                schedules: flow["crons"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect(),
                example,
            }
        })
        .collect()
}

/// The flow whose runs `pathname` lists: `None` for `/flows`, the decoded
/// name for `/flows/orders%2Fsync`. `None` outside: not a page of flows.
/// A name the application does not declare lists its runs all the same, as
/// in the Leptos page: the page of a run links to the page of its flow, which
/// may have been removed from the code since.
fn selected_flow(pathname: &str) -> Option<Option<String>> {
    let rest = pathname.strip_prefix("/flows")?;
    if rest.is_empty() {
        return Some(None);
    }
    match rest.strip_prefix('/')? {
        "" => Some(None),
        name if name.contains('/') => None,
        name => decode_uri_component(name).map(Some),
    }
}

/// The page of the runs of `name`.
fn flow_href(name: &str) -> String {
    format!("/flows/{}", encode_uri_component(name))
}

fn run_modal_id(index: usize) -> String {
    format!("run-flow-{index}")
}

pub(super) async fn render(page: &BuiltinPage<'_>) -> Result<Markup> {
    let Some(selected) = selected_flow(page.pathname) else {
        return Err(AdminPublicError::new("NOT_FOUND", "Page not found").status(404).into());
    };
    let flows = flow_list(page);
    let query = RunsQuery::of(page);
    // A runs query refused (a page too deep) is said above the table, as the
    // Leptos page said a list that failed.
    let (mut runs, load_error) = match query_runs(page.app, page.session, selected.as_deref(), &query.api()).await? {
        Ok(runs) => (runs, None),
        Err(refused) => (Vec::new(), Some(refused)),
    };
    let has_next = runs.len() > PAGE_SIZE;
    runs.truncate(PAGE_SIZE);
    Ok(view(&FlowsView {
        flows: &flows,
        selected: selected.as_deref(),
        pathname: page.pathname,
        query: &query,
        runs: &runs,
        has_next,
        load_error: load_error.as_deref(),
        now: Utc::now(),
        form: page.form.of(RUN),
    }))
}

/// What the page shows.
struct FlowsView<'a> {
    flows: &'a [FlowItem],
    /// The flow whose runs are listed; `None`, every flow.
    selected: Option<&'a str>,
    /// The path of the page, as the links of its pages and filters keep it.
    pathname: &'a str,
    query: &'a RunsQuery,
    /// The runs of the page.
    runs: &'a [FlowRun],
    /// Another page of runs follows.
    has_next: bool,
    /// Why the runs could not be listed.
    load_error: Option<&'a str>,
    /// When the page is rendered: how long ago a run started.
    now: DateTime<Utc>,
    /// The "Run" form the page refused, shown again in its modal.
    form: &'a FormState,
}

fn view(page: &FlowsView<'_>) -> Markup {
    html! {
        s-page heading="Flows" {
            (flow_table(page.flows))
            @if let Some(error) = page.load_error {
                s-banner tone="critical" heading="Runs could not be loaded" { (error) }
            }
            (runs_section(page))
            @for (index, flow) in page.flows.iter().enumerate() {
                (run_modal(index, flow, page.form))
            }
            @if !page.runs.is_empty() {
                (delete_modal())
            }
        }
    }
}

fn flow_table(flows: &[FlowItem]) -> Markup {
    html! {
        s-section padding="none" {
            s-table {
                s-table-header-row {
                    s-table-header listSlot="primary" { "Flow" }
                    s-table-header listSlot="secondary" { "Schedule" }
                    s-table-header listSlot="inline" { "Type" }
                    s-table-header {}
                }
                s-table-body {
                    @if flows.is_empty() {
                        s-table-row {
                            s-table-cell { s-text color="subdued" { "No flows registered" } }
                            s-table-cell {} s-table-cell {} s-table-cell {}
                        }
                    }
                    @for (index, flow) in flows.iter().enumerate() {
                        (flow_row(index, flow))
                    }
                }
            }
        }
    }
}

fn flow_row(index: usize, flow: &FlowItem) -> Markup {
    let scheduled = !flow.schedules.is_empty();
    let last = flow.schedules.len().saturating_sub(1);
    html! {
        s-table-row {
            s-table-cell { s-link href=(flow_href(&flow.name)) { (flow.name) } }
            s-table-cell {
                @if !scheduled { "—" }
                @for (position, schedule) in flow.schedules.iter().enumerate() {
                    code style=(format!(
                        "display:block;font-size:13px;background:#f1f1f1;padding:2px 6px;border-radius:4px;margin-bottom:{}",
                        if position < last { "4px" } else { "0" },
                    )) { (schedule) }
                }
            }
            s-table-cell {
                s-badge tone=(if scheduled { "info" } else { "neutral" }) {
                    (if scheduled { "Scheduled" } else { "Manual" })
                }
            }
            s-table-cell {
                s-button variant="primary" commandFor=(run_modal_id(index)) command="--show" { "Run" }
            }
        }
    }
}

/// The form that starts a flow, in a modal: its input, an example of its
/// schema, or what was sent with the error under it when it was refused,
/// the modal then opened again.
fn run_modal(index: usize, flow: &FlowItem, form: &FormState) -> Markup {
    let id = run_modal_id(index);
    let refused = form.is_refused() && form.value("flow") == Some(flow.name.as_str());
    let (input, error) = if refused {
        (form.value(INPUT).unwrap_or_default(), form.error(INPUT))
    } else {
        (flow.example.as_str(), None)
    };
    html! {
        form method="post" {
            input type="hidden" name="_action" value=(RUN);
            input type="hidden" name="flow" value=(flow.name);
            s-modal id=(id) heading=(format!("Run {}", flow.name)) data-openshop-open[refused] {
                s-text-area
                    name=(INPUT)
                    label="Input (JSON)"
                    rows="6"
                    placeholder="{ \"limit\": 10 }"
                    value=(input)
                    error=[error] {}
                s-button slot="primary-action" variant="primary" type="submit" { "Run" }
                s-button slot="secondary-actions" commandFor=(id) command="--hide" { "Cancel" }
            }
        }
    }
}

/// The forms of the page: a "Run" modal, and the deletion of the selected runs.
pub(super) async fn submit(page: &BuiltinPage<'_>, action: &str, fields: &[(String, String)]) -> Result<Outcome> {
    match action {
        RUN => run(page, fields).await,
        DELETE => delete(page, fields).await,
        _ => Err(super::no_action()),
    }
}

/// Starts the flow and goes to its run, as the Leptos page did. An input
/// that is not JSON, or that the flow refuses, is said under its field.
async fn run(page: &BuiltinPage<'_>, fields: &[(String, String)]) -> Result<Outcome> {
    let Some(flow) = field(fields, "flow").filter(|flow| !flow.is_empty()) else {
        return Err(AdminPublicError::new("INVALID_INPUT", "flow is required").into());
    };
    let Ok(input) = serde_json::from_str::<Value>(field(fields, INPUT).unwrap_or_default()) else {
        return Ok(Outcome::invalid([(INPUT, "Invalid JSON")]));
    };
    match start_flow(page.app, page.session, flow, input).await? {
        Ok(started) => {
            Ok(Outcome::redirect(format!("/runs/{}", started.run_id)).with_toast(format!("Flow \"{flow}\" started")))
        }
        Err(refused @ NotStarted::Invalid(_)) => Ok(Outcome::invalid([(INPUT, refused.message())])),
        Err(refused @ NotStarted::Conflict(_)) => {
            Err(AdminPublicError::new("CONFLICT", refused.message()).status(409).into())
        }
        Err(refused @ NotStarted::Unknown(_)) => {
            Err(AdminPublicError::new("NOT_FOUND", refused.message()).status(404).into())
        }
    }
}

/// Deletes the runs checked that are over, and says how many; the page
/// again, with every run unselected.
async fn delete(page: &BuiltinPage<'_>, fields: &[(String, String)]) -> Result<Outcome> {
    let ids: Vec<&str> = fields
        .iter()
        .filter(|(name, _)| name == IDS)
        .map(|(_, id)| id.as_str())
        .collect();
    if ids.is_empty() {
        return Err(AdminPublicError::new("INVALID_INPUT", "Select the runs to delete").into());
    }
    let batch = match delete_run_batch(page.app, page.session, &ids).await? {
        Ok(batch) => batch,
        Err(refused) => return Err(AdminPublicError::new("INVALID_INPUT", refused).into()),
    };
    let message = deleted_message(batch.deleted, batch.skipped);
    let query = RunsQuery::of(page);
    Ok(if query.all_selected {
        Outcome::redirect(query.href(page.pathname, query.page, false)).with_toast(message)
    } else {
        Outcome::toast(message)
    })
}

fn deleted_message(deleted: usize, skipped: usize) -> String {
    let skipped = match skipped {
        0 => String::new(),
        skipped => format!(", {skipped} skipped (active)"),
    };
    format!("Deleted {deleted} run(s){skipped}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use format::tests::run;
    use indexmap::IndexMap;

    fn flows() -> Vec<FlowItem> {
        vec![
            FlowItem {
                name: "syncOrders".to_string(),
                schedules: vec!["*/5 * * * *".to_string(), "0 0 * * *".to_string()],
                example: "{\n  \"limit\": 1\n}".to_string(),
            },
            FlowItem {
                name: "a/b <c>".to_string(),
                schedules: Vec::new(),
                example: "{}".to_string(),
            },
        ]
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-06T12:05:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn render(
        flows: &[FlowItem],
        selected: Option<&str>,
        query: &RunsQuery,
        runs: &[FlowRun],
        has_next: bool,
        form: &FormState,
    ) -> String {
        let pathname = selected.map_or_else(|| "/flows".to_string(), flow_href);
        view(&FlowsView {
            flows,
            selected,
            pathname: &pathname,
            query,
            runs,
            has_next,
            load_error: None,
            now: now(),
            form,
        })
        .into_string()
    }

    fn first_page() -> RunsQuery {
        RunsQuery {
            page: 1,
            ..RunsQuery::default()
        }
    }

    #[test]
    fn lists_the_flows_with_their_crons_and_a_run_form_each() {
        let html = render(&flows(), None, &first_page(), &[], false, &FormState::default());
        assert!(
            html.contains(r#"<s-link href="/flows/syncOrders">syncOrders</s-link>"#),
            "{html}"
        );
        // A name is escaped in the markup and encoded in its link.
        assert!(
            html.contains(r#"<s-link href="/flows/a%2Fb%20%3Cc%3E">a/b &lt;c&gt;</s-link>"#),
            "{html}"
        );
        assert!(html.contains(">*/5 * * * *</code>"), "{html}");
        assert!(html.contains(r#"<s-badge tone="info">Scheduled</s-badge>"#), "{html}");
        assert!(html.contains(r#"<s-badge tone="neutral">Manual</s-badge>"#), "{html}");
        assert!(
            html.contains(r#"commandFor="run-flow-1" command="--show">Run</s-button>"#),
            "{html}"
        );
        // The form of each flow, in its modal, starts with the example of its schema.
        assert!(html.contains(r#"<input type="hidden" name="_action" value="run"><input type="hidden" name="flow" value="syncOrders"><s-modal id="run-flow-0" heading="Run syncOrders">"#), "{html}");
        assert!(html.contains("value=\"{\n  &quot;limit&quot;: 1\n}\""), "{html}");
        assert!(
            html.contains(r#"<s-button slot="primary-action" variant="primary" type="submit">Run</s-button>"#),
            "{html}"
        );
        assert!(!html.contains("data-openshop-open"), "{html}");
        // Without runs: nothing to select, nothing to delete, nothing to refresh.
        assert!(html.contains("No runs yet"), "{html}");
        assert!(!html.contains(DELETE_MODAL), "{html}");
        assert!(!html.contains("data-openshop-refresh"), "{html}");
        assert!(!html.contains("Previous page"), "{html}");

        let empty = render(&[], None, &first_page(), &[], false, &FormState::default());
        assert!(empty.contains("No flows registered"), "{empty}");
    }

    #[test]
    fn opens_the_refused_form_again_with_its_error() {
        let form = FormState {
            action: Some(RUN.to_string()),
            values: vec![
                ("_action".to_string(), RUN.to_string()),
                ("flow".to_string(), "syncOrders".to_string()),
                (INPUT.to_string(), "{ nope".to_string()),
            ],
            errors: IndexMap::from([(INPUT.to_string(), "Invalid JSON".to_string())]),
            message: None,
        };
        let html = render(&flows(), None, &first_page(), &[], false, &form);
        assert!(
            html.contains(r#"<s-modal id="run-flow-0" heading="Run syncOrders" data-openshop-open>"#),
            "{html}"
        );
        assert!(html.contains(r#"value="{ nope" error="Invalid JSON""#), "{html}");
        // The other flow's form is as it was.
        assert!(
            html.contains(r#"<s-modal id="run-flow-1" heading="Run a/b &lt;c&gt;">"#),
            "{html}"
        );
        assert_eq!(html.matches("error=").count(), 1, "{html}");
    }

    #[test]
    fn lists_the_runs_with_their_status_duration_and_start() {
        let mut completed = run("completed", Some(0), Some(1_250));
        completed.id = uuid::Uuid::parse_str("0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0").unwrap();
        let running = run("running", Some(60_000), None);
        let pending = run("pending", None, None);
        let runs = [completed, running, pending];
        let html = render(&flows(), None, &first_page(), &runs, false, &FormState::default());
        // The checkbox of a run belongs to the form of the confirmation.
        assert!(html.contains(r#"<s-checkbox form="delete-runs" name="ids" value="0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0" label="Select run 0f1e2d3c" labelAccessibilityVisibility="exclusive"></s-checkbox>"#), "{html}");
        assert!(
            html.contains(r#"<s-table-row clickDelegate="run-link-0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0">"#),
            "{html}"
        );
        assert!(html.contains(r#"<s-link id="run-link-0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0" href="/runs/0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0">#0f1e2d3c</s-link>"#), "{html}");
        assert!(
            html.contains(r#"<s-badge tone="success">completed</s-badge>"#),
            "{html}"
        );
        assert!(html.contains(r#"<s-badge tone="warning">running</s-badge>"#), "{html}");
        assert!(html.contains("<s-table-cell>1.3s</s-table-cell>"), "{html}");
        assert!(html.contains("<s-table-cell>…</s-table-cell>"), "{html}");
        assert!(html.contains(r#"<s-text>5m ago</s-text><s-text color="subdued"><time datetime="2026-10-06T12:00:00.000Z">2026-10-06 12:00:00 UTC</time></s-text>"#), "{html}");
        assert!(html.contains(r#"<s-text>4m ago</s-text>"#), "{html}");
        // Every flow: the column of the flow.
        assert!(
            html.contains(r#"<s-table-header listSlot="secondary">Flow</s-table-header>"#),
            "{html}"
        );
        assert!(html.contains("<s-table-cell>syncOrders</s-table-cell>"), "{html}");
        // A run is active: the page is fetched again.
        assert!(
            html.contains(r#"<s-section heading="Runs" data-openshop-refresh="5000">"#),
            "{html}"
        );
        // The selection and its deletion.
        assert!(
            html.contains(r#"<s-button variant="tertiary" href="/flows?select=all">Select all</s-button>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"commandFor="delete-runs-modal" command="--show">Delete selected</s-button>"#),
            "{html}"
        );
        assert!(html.contains(r#"<form id="delete-runs" method="post"><input type="hidden" name="_action" value="deleteRuns"><s-modal id="delete-runs-modal" heading="Delete runs">"#), "{html}");
        assert!(
            html.contains(
                r#"<s-button slot="primary-action" variant="primary" tone="critical" type="submit">Delete</s-button>"#
            ),
            "{html}"
        );

        // Over: no refresh.
        let html = render(&flows(), None, &first_page(), &runs[..1], false, &FormState::default());
        assert!(html.contains(r#"<s-section heading="Runs">"#), "{html}");
        assert!(!html.contains("data-openshop-refresh"), "{html}");
    }

    #[test]
    fn keeps_the_filters_in_the_links_of_the_pages_and_of_the_selection() {
        let query = RunsQuery {
            search: "a&b".to_string(),
            status: "failed".to_string(),
            from: "2026-11-01".to_string(),
            to: String::new(),
            page: 2,
            all_selected: true,
        };
        let runs = [run("failed", Some(0), Some(125_400))];
        let html = render(&flows(), Some("syncOrders"), &query, &runs, true, &FormState::default());
        assert!(html.contains(r#"<s-section heading="Runs — syncOrders">"#), "{html}");
        assert!(
            html.contains(r#"<s-link href="/flows">Runs of every flow</s-link>"#),
            "{html}"
        );
        // The runs of one flow: no column of the flow.
        assert!(
            !html.contains(r#"listSlot="secondary">Flow</s-table-header>"#),
            "{html}"
        );
        assert!(html.contains("<s-table-cell>2m 5s</s-table-cell>"), "{html}");
        // Every run of the page selected, until the selection is cleared.
        assert!(
            html.contains(r#"labelAccessibilityVisibility="exclusive" checked></s-checkbox>"#),
            "{html}"
        );
        assert!(html.contains(r#"href="/flows/syncOrders?q=a%26b&amp;status=failed&amp;from=2026-11-01&amp;page=2">Clear selection</s-button>"#), "{html}");
        // The pages before and after, with the filters and without the selection.
        assert!(html.contains(r#"accessibilityLabel="Previous page" href="/flows/syncOrders?q=a%26b&amp;status=failed&amp;from=2026-11-01">"#), "{html}");
        assert!(html.contains(r#"accessibilityLabel="Next page" href="/flows/syncOrders?q=a%26b&amp;status=failed&amp;from=2026-11-01&amp;page=3">"#), "{html}");
        // The filters: a form that navigates, with what the URL asked.
        assert!(html.contains(r#"<form method="get" slot="filters">"#), "{html}");
        assert!(html.contains(r#"name="q" label="Search runs" labelAccessibilityVisibility="exclusive" placeholder="Search by flow name, status, or ID..." value="a&amp;b">"#), "{html}");
        assert!(
            html.contains(r#"<s-option value="failed" selected>Failed</s-option>"#),
            "{html}"
        );
        assert!(html.contains(r#"<s-date-field name="from" label="From" labelAccessibilityVisibility="exclusive" placeholder="From" value="2026-11-01">"#), "{html}");
        assert!(
            html.contains(r#"<s-button variant="tertiary" href="/flows/syncOrders">Clear</s-button>"#),
            "{html}"
        );

        // The last page: no page after.
        let html = render(&flows(), Some("syncOrders"), &query, &[], false, &FormState::default());
        assert!(html.contains(r#"accessibilityLabel="Next page" disabled>"#), "{html}");
        assert!(html.contains("No runs on this page"), "{html}");
        let filtered = RunsQuery { page: 1, ..query };
        let html = render(&flows(), None, &filtered, &[], false, &FormState::default());
        assert!(html.contains("No runs match these filters"), "{html}");
        assert!(!html.contains("Previous page"), "{html}");
    }

    #[test]
    fn reads_the_flow_of_the_path() {
        assert_eq!(selected_flow("/flows"), Some(None));
        assert_eq!(selected_flow("/flows/"), Some(None));
        assert_eq!(selected_flow("/flows/syncOrders"), Some(Some("syncOrders".to_string())));
        assert_eq!(
            selected_flow("/flows/orders%2Fsync%20now"),
            Some(Some("orders/sync now".to_string()))
        );
        assert_eq!(selected_flow("/flows/a/b"), None);
        assert_eq!(selected_flow("/flows/%zz"), None);
        assert_eq!(selected_flow("/runs"), None);
        assert_eq!(selected_flow("/flowsy"), None);
    }

    #[test]
    fn summarizes_a_deletion() {
        assert_eq!(deleted_message(3, 0), "Deleted 3 run(s)");
        assert_eq!(deleted_message(1, 2), "Deleted 1 run(s), 2 skipped (active)");
    }
}
