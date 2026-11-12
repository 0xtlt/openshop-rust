//! `/runs/:id`: a run of a flow. The flow it belongs to, a link back to its
//! page; its status, its duration, its dates, its input and its error; its
//! steps, each with its status, its duration, its error and its output (in a
//! modal: copied, and downloaded as a JSON file); its logs, filtered by the
//! query and the levels of the URL (`?q=…&levels=…`), counted by level and
//! exported as JSON or CSV. A run that is over is retried, resumed or
//! restarted, or deleted, by the forms of two modals. While the run is
//! active, the page is fetched again every 3 seconds, its logs with it.

mod logs;
mod steps;

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use maud::{Markup, html};

use super::ui::{status_tone, to_fixed_1};
use super::{BuiltinPage, field, ui};
use crate::admin::{AdminPublicError, Outcome};
use crate::admin_paths::rpc::encode_uri_component as encode;
use crate::db::schema::{FlowRun, StepResult};
use crate::runtime::time::to_iso;
use crate::server::api::runs::{RunRefusal, delete_finished_run, find_run, retry_finished_run, run_steps};
use crate::types::ACTIVE_RUN_STATUSES;
use logs::{LogFilters, Logs, help_modal, load_logs, logs_section};
use steps::{output_modals, step_rows, steps_table};

/// The forms of the retry modal: `mode` is `resume` or `reset`.
const RETRY: &str = "retry";
/// The form of the delete modal.
const DELETE: &str = "delete";
/// Keeps the steps that completed.
const RESUME: &str = "resume";
/// Discards every step.
const RESET: &str = "reset";
const RETRY_MODAL: &str = "retry-modal";
const DELETE_MODAL: &str = "delete-modal";
const INPUT_MODAL: &str = "run-input";
/// How often the page of an active run is fetched again, in milliseconds: as
/// often as the WebAssembly admin read the run and its logs.
const REFRESH_MS: i64 = 3_000;
/// How long a new line of the logs stays highlighted.
const NEW_LINE_MS: i64 = 5_000;

fn run_id<'a>(page: &BuiltinPage<'a>) -> &'a str {
    page.pathname.strip_prefix("/runs/").unwrap_or_default()
}

fn run_not_found() -> anyhow::Error {
    AdminPublicError::new("NOT_FOUND", "Run not found").status(404).into()
}

/// A run the JSON API would refuse to retry or delete: the same status and
/// message, in an error toast.
fn refused(refusal: RunRefusal) -> anyhow::Error {
    let code = match refusal {
        RunRefusal::NotFound => "NOT_FOUND",
        RunRefusal::Conflict(_) | RunRefusal::Concurrent(_) => "CONFLICT",
    };
    AdminPublicError::new(code, refusal.message())
        .status(refusal.status().as_u16())
        .into()
}

/// The page of the flow a run belongs to.
fn flow_href(flow_name: &str) -> String {
    format!("/flows/{}", encode(flow_name))
}

fn is_active(status: &str) -> bool {
    ACTIVE_RUN_STATUSES.contains(&status)
}

/// A run that is over: it can be retried and deleted.
fn is_finished(status: &str) -> bool {
    matches!(status, "failed" | "completed" | "canceled")
}

/// How long the run took, `1.3s`; `Running…` while it runs.
fn duration(run: &FlowRun) -> String {
    match (run.started_at, run.completed_at) {
        // To the millisecond, as the dates the browser read.
        (Some(started), Some(completed)) => {
            let ms = completed.timestamp_millis() - started.timestamp_millis();
            format!("{}s", to_fixed_1(ms as f64 / 1_000.0))
        }
        _ if run.status == "running" => "Running…".to_string(),
        _ => "—".to_string(),
    }
}

pub(super) async fn render(page: &BuiltinPage<'_>) -> Result<Markup> {
    let Some(run) = find_run(page.app, page.session, run_id(page)).await? else {
        return Err(run_not_found());
    };
    let steps = run_steps(page.app, &run).await?;
    let filters = LogFilters::read(page.param("q"), page.param("levels"));
    let logs = load_logs(page.app, &run, &filters).await?;
    Ok(view(&run, &steps, &filters, &logs, Utc::now()))
}

fn view(run: &FlowRun, steps: &[StepResult], filters: &LogFilters, logs: &Logs, now: DateTime<Utc>) -> Markup {
    let id = run.id.to_string();
    let active = is_active(&run.status);
    let finished = is_finished(&run.status);
    let rows = step_rows(steps);
    let error = run.error.as_deref().filter(|error| !error.is_empty());
    let input = run.input.as_ref().filter(|input| ui::is_truthy(Some(input)));
    let fresh_since = active.then(|| now - Duration::milliseconds(NEW_LINE_MS));
    html! {
        s-page heading=(format!("Run #{id}")) data-openshop-refresh=[active.then_some(REFRESH_MS)] {
            s-link slot="breadcrumb-actions" href=(flow_href(&run.flow_name)) { (run.flow_name) }
            @if finished {
                s-button slot="secondary-actions" variant="secondary" tone="critical" commandFor=(DELETE_MODAL) command="--show" {
                    "Delete"
                }
                s-button slot="primary-action" variant="primary" commandFor=(RETRY_MODAL) command="--show" { "Retry" }
            }
            @if let Some(error) = error {
                s-banner tone="critical" heading="Error" { (error) }
            }
            (summary(run, input.is_some()))
            (steps_table(&rows))
            (logs_section(&format!("/runs/{id}"), &id, filters, logs, fresh_since))

            (output_modals(&rows))
            @if let Some(input) = input { (ui::json_modal(INPUT_MODAL, "Input", input)) }
            (help_modal())
            @if finished {
                (retry_modal())
                (delete_modal())
            }
        }
    }
}

fn summary(run: &FlowRun, has_input: bool) -> Markup {
    html! {
        s-section heading="Summary" {
            s-stack direction="inline" gap="large-200" alignItems="center" {
                s-stack direction="inline" gap="small" alignItems="center" {
                    s-text color="subdued" { "Status" }
                    s-badge tone=(status_tone(&run.status)) { (run.status) }
                }
                s-stack direction="inline" gap="small" alignItems="center" {
                    s-text color="subdued" { "Duration" }
                    s-text type="strong" { (duration(run)) }
                }
                s-stack direction="inline" gap="small" alignItems="center" {
                    s-text color="subdued" { "Started" }
                    s-text {
                        @match run.started_at {
                            Some(started) => { (ui::time(&to_iso(&started))) }
                            None => { "—" }
                        }
                    }
                }
                @if let Some(completed) = run.completed_at {
                    s-stack direction="inline" gap="small" alignItems="center" {
                        s-text color="subdued" { "Completed" }
                        s-text { (ui::time(&to_iso(&completed))) }
                    }
                }
                @if has_input {
                    s-button variant="secondary" commandFor=(INPUT_MODAL) command="--show" { "Input" }
                }
            }
        }
    }
}

fn retry_form(mode: &str, variant: &str, label: &str) -> Markup {
    html! {
        form method="post" {
            input type="hidden" name="_action" value=(RETRY);
            input type="hidden" name="mode" value=(mode);
            s-button type="submit" variant=(variant) { (label) }
        }
    }
}

fn retry_modal() -> Markup {
    html! {
        s-modal id=(RETRY_MODAL) heading="Retry run" accessibilityLabel="Choose retry mode" {
            s-stack direction="block" gap="base" {
                s-text { "How do you want to retry this run?" }
                (retry_form(RESUME, "primary", "Resume — pick up where it failed"))
                (retry_form(RESET, "secondary", "Restart — discard steps and start fresh"))
            }
            s-button slot="secondary-actions" variant="secondary" commandFor=(RETRY_MODAL) command="--hide" { "Cancel" }
        }
    }
}

/// The form holds the modal: its submit button is one of the modal's actions.
fn delete_modal() -> Markup {
    html! {
        form method="post" {
            input type="hidden" name="_action" value=(DELETE);
            s-modal id=(DELETE_MODAL) heading="Delete run" accessibilityLabel="Confirm run deletion" {
                s-text { "This will permanently delete this run, its steps, and all logs. This cannot be undone." }
                s-button slot="primary-action" type="submit" variant="primary" tone="critical" { "Delete" }
                s-button slot="secondary-actions" variant="secondary" commandFor=(DELETE_MODAL) command="--hide" { "Cancel" }
            }
        }
    }
}

/// The forms of the modals: the run retried, said in a toast, or deleted, and
/// the page of its flow shown.
pub(super) async fn submit(page: &BuiltinPage<'_>, action: &str, fields: &[(String, String)]) -> Result<Outcome> {
    let id = run_id(page);
    match action {
        RETRY => {
            let mode = field(fields, "mode").unwrap_or(RESUME);
            if mode != RESUME && mode != RESET {
                return Err(AdminPublicError::new("INVALID_INPUT", "mode must be resume or reset")
                    .status(422)
                    .into());
            }
            retry_finished_run(page.app, page.session, id, mode)
                .await?
                .map_err(refused)?;
            Ok(Outcome::toast(if mode == RESET {
                "Run restarted"
            } else {
                "Run resumed"
            }))
        }
        DELETE => {
            let Some(run) = find_run(page.app, page.session, id).await? else {
                return Err(run_not_found());
            };
            delete_finished_run(page.app, page.session, id)
                .await?
                .map_err(refused)?;
            Ok(Outcome::redirect(flow_href(&run.flow_name)).with_toast("Run deleted"))
        }
        _ => Err(super::no_action()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use crate::server::api::run_logs::FilteredRunLogs;
    use serde_json::json;
    use uuid::Uuid;

    const NOW: &str = "2026-11-09T12:00:10Z";

    fn date(iso: &str) -> DateTime<Utc> {
        iso.parse().unwrap()
    }

    fn run(status: &str) -> FlowRun {
        FlowRun {
            id: Uuid::from_u128(7),
            app_handle: "default".to_string(),
            shop: "shop.myshopify.com".to_string(),
            flow_name: "sync orders".to_string(),
            status: status.to_string(),
            input: Some(json!({})),
            error: None,
            deadline_at: None,
            parent_run_id: None,
            attempts: 1,
            available_at: None,
            worker_id: None,
            retry_policy: None,
            started_at: Some(date("2026-11-09T12:00:00.123Z")),
            completed_at: None,
            created_at: date("2026-11-09T11:59:59Z"),
        }
    }

    fn no_logs() -> Logs {
        Logs {
            page: FilteredRunLogs {
                logs: Vec::new(),
                total: 0,
                level_counts: BTreeMap::new(),
                next: None,
            },
            refused: None,
        }
    }

    fn render(run: &FlowRun) -> String {
        view(run, &[], &LogFilters::read(None, None), &no_logs(), date(NOW)).into_string()
    }

    #[test]
    fn a_run_in_progress_is_fetched_again_and_has_no_action() {
        for status in ACTIVE_RUN_STATUSES {
            let html = render(&run(status));
            assert!(html.contains(r#"data-openshop-refresh="3000""#), "{status}: {html}");
            assert!(!html.contains(r#"name="_action""#), "{status}: {html}");
            assert!(!html.contains("retry-modal"), "{status}: {html}");
        }
        let html = render(&run("running"));
        assert!(
            html.contains(r#"<s-page heading="Run #00000000-0000-0000-0000-000000000007""#),
            "{html}"
        );
        assert!(html.contains(r#"<s-badge tone="warning">running</s-badge>"#), "{html}");
        assert!(html.contains(r#"<s-text type="strong">Running…</s-text>"#), "{html}");
        assert!(html.contains(r#"<time datetime="2026-11-09T12:00:00.123Z">"#), "{html}");
        // The link back to the flow.
        assert!(
            html.contains(r#"<s-link slot="breadcrumb-actions" href="/flows/sync%20orders">sync orders</s-link>"#),
            "{html}"
        );
        // An empty input has nothing to show.
        assert!(!html.contains("run-input"), "{html}");
    }

    #[test]
    fn a_run_that_is_over_is_retried_or_deleted_by_a_form() {
        let mut failed = run("failed");
        failed.completed_at = Some(date("2026-11-09T12:00:01.373Z"));
        failed.error = Some("<script>alert(1)</script>".to_string());
        failed.input = Some(json!({ "limit": "<10>" }));
        let html = render(&failed);
        assert!(!html.contains("data-openshop-refresh"), "{html}");
        assert!(html.contains(r#"<s-badge tone="critical">failed</s-badge>"#), "{html}");
        assert!(html.contains(r#"<s-text type="strong">1.3s</s-text>"#), "{html}");
        assert!(html.contains(r#"<time datetime="2026-11-09T12:00:01.373Z">"#), "{html}");
        assert!(
            html.contains(
                r#"<s-banner tone="critical" heading="Error">&lt;script&gt;alert(1)&lt;/script&gt;</s-banner>"#
            ),
            "{html}"
        );
        // The header opens the modals, whose forms run the actions.
        assert!(
            html.contains(r#"<s-button slot="primary-action" variant="primary" commandFor="retry-modal" command="--show">Retry</s-button>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"commandFor="delete-modal" command="--show">Delete</s-button>"#),
            "{html}"
        );
        for mode in [RESUME, RESET] {
            assert!(
                html.contains(&format!(
                    r#"<form method="post"><input type="hidden" name="_action" value="retry"><input type="hidden" name="mode" value="{mode}"><s-button type="submit""#
                )),
                "{html}"
            );
        }
        assert!(html.contains("Resume — pick up where it failed"), "{html}");
        assert!(html.contains("Restart — discard steps and start fresh"), "{html}");
        assert!(
            html.contains(r#"<form method="post"><input type="hidden" name="_action" value="delete"><s-modal id="delete-modal" heading="Delete run""#),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<s-button slot="primary-action" type="submit" variant="primary" tone="critical">Delete</s-button>"#
            ),
            "{html}"
        );
        // The input, in a modal that copies it.
        assert!(
            html.contains(r#"<s-button variant="secondary" commandFor="run-input" command="--show">Input</s-button>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<s-modal id="run-input" heading="Input" size="large">"#),
            "{html}"
        );
        assert!(html.contains("&quot;limit&quot;: &quot;&lt;10&gt;&quot;"), "{html}");
        assert!(!html.contains("<10>"), "{html}");
    }

    #[test]
    fn new_lines_are_highlighted_only_while_the_run_is_active() {
        let line = |status: &str| {
            let logs = Logs {
                page: FilteredRunLogs {
                    logs: vec![crate::server::api::run_logs::LogResponseRow {
                        log: crate::db::schema::LogRow {
                            id: Uuid::from_u128(1),
                            flow_run_id: Some(Uuid::from_u128(7).to_string()),
                            level: "info".to_string(),
                            message: Some("fetched".to_string()),
                            payload: None,
                            created_at: date("2026-11-09T12:00:08Z"),
                        },
                        matched: None,
                    }],
                    total: 1,
                    level_counts: BTreeMap::from([("info".to_string(), 1)]),
                    next: None,
                },
                refused: None,
            };
            view(&run(status), &[], &LogFilters::read(None, None), &logs, date(NOW)).into_string()
        };
        assert!(line("running").contains("background:#fff3cd"));
        assert!(!line("completed").contains("background:#fff3cd"));
    }

    #[test]
    fn writes_durations_as_the_browser_did() {
        assert_eq!(to_fixed_1(0.25), "0.3");
        assert_eq!(to_fixed_1(1.15), "1.1");
        assert_eq!(to_fixed_1(59.999), "60.0");
        let mut pending = run("pending");
        pending.started_at = None;
        assert_eq!(duration(&pending), "—");
        let mut done = run("completed");
        done.completed_at = Some(date("2026-11-09T12:01:05.163Z"));
        assert_eq!(duration(&done), "65.0s");
        assert!(render(&pending).contains("<s-text>—</s-text>"));
        assert_eq!(status_tone("sleeping"), "info");
        assert_eq!(status_tone("canceled"), "neutral");
        assert_eq!(status_tone("skipped"), "auto");
    }
}
