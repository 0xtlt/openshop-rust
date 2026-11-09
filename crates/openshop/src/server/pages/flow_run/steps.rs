//! The steps of a run: a row each, numbered, with its status, its duration,
//! its error, and its output in a modal that shows the beginning of it, copies
//! all of it and downloads it as a JSON file.

use std::collections::HashMap;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use maud::{Markup, html};
use serde_json::Value;

use super::super::ui;
use super::{status_tone, to_fixed_1};
use crate::db::schema::StepResult;

/// What the modal of an output shows of it: the rest is copied and downloaded.
const MAX_OUTPUT_CHARS: usize = 2000;
const LABEL_STYLE: &str = "display:inline-flex;align-items:center;justify-content:center;min-width:20px;height:20px;border-radius:999px;padding:0 6px;background:#f1f1f1;font-size:11px;font-weight:700;color:#616161;flex-shrink:0";

/// A step with its position: `2` for the second step of the run, `2.3` for
/// the third attempt of that step.
pub(super) struct StepRow<'a> {
    step: &'a StepResult,
    label: String,
    is_retry: bool,
}

/// The steps in the order they started, an attempt after the first labelled
/// by the position of its step and its number.
pub(super) fn step_rows(steps: &[StepResult]) -> Vec<StepRow<'_>> {
    let mut sorted: Vec<&StepResult> = steps.iter().collect();
    // As the page of the browser compared them: the date to the millisecond, then the id.
    sorted.sort_by(|left, right| {
        (left.created_at.timestamp_millis(), left.id.to_string())
            .cmp(&(right.created_at.timestamp_millis(), right.id.to_string()))
    });
    let mut positions: HashMap<&str, usize> = HashMap::new();
    let mut attempts: HashMap<&str, usize> = HashMap::new();
    sorted
        .into_iter()
        .map(|step| {
            let next = positions.len() + 1;
            let position = *positions.entry(&step.step_name).or_insert(next);
            let attempt = attempts.entry(&step.step_name).or_insert(0);
            *attempt += 1;
            StepRow {
                step,
                label: if *attempt == 1 {
                    position.to_string()
                } else {
                    format!("{position}.{attempt}")
                },
                is_retry: *attempt > 1,
            }
        })
        .collect()
}

fn output_of(step: &StepResult) -> Option<&Value> {
    step.output.as_ref().filter(|output| !output.is_null())
}

fn modal_id(step: &StepResult) -> String {
    format!("output-{}", step.id)
}

pub(super) fn steps_table(rows: &[StepRow<'_>]) -> Markup {
    html! {
        s-section padding="none" {
            s-table {
                s-table-header-row {
                    s-table-header listSlot="primary" { "Step" }
                    s-table-header listSlot="inline" { "Status" }
                    s-table-header listSlot="labeled" { "Duration" }
                    s-table-header {}
                }
                s-table-body {
                    @for row in rows { (step_row(row)) }
                }
            }
        }
    }
}

fn step_row(row: &StepRow<'_>) -> Markup {
    let step = row.step;
    let duration = step.duration_ms.map_or_else(|| "—".to_string(), |ms| format!("{ms}ms"));
    let error = step.error.as_deref().filter(|error| !error.is_empty());
    html! {
        s-table-row {
            s-table-cell {
                span style="display:inline-flex;align-items:center;gap:8px" {
                    span style=(LABEL_STYLE) { (row.label) }
                    strong { (step.step_name) }
                    @if row.is_retry { span style="color:#8c9196;font-size:12px" { "retry" } }
                }
            }
            s-table-cell { s-badge tone=(status_tone(&step.status)) { (step.status) } }
            s-table-cell { (duration) }
            s-table-cell {
                @if let Some(error) = error { span style="color:#d72c0d;font-size:13px" { (error) } }
                @if output_of(step).is_some() {
                    s-button variant="secondary" commandFor=(modal_id(step)) command="--show" { "Output" }
                }
            }
        }
    }
}

/// The modal of the output of each step that has one.
pub(super) fn output_modals(rows: &[StepRow<'_>]) -> Markup {
    html! {
        @for row in rows {
            @if let Some(output) = output_of(row.step) { (output_modal(row.step, output)) }
        }
    }
}

/// The beginning of an output, what its modal shows: at most
/// `MAX_OUTPUT_CHARS` UTF-16 units, as the browser counted them, then `…`.
fn preview(full: &str) -> String {
    if full.encode_utf16().count() <= MAX_OUTPUT_CHARS {
        return full.to_string();
    }
    let mut units = 0;
    let end = full
        .char_indices()
        .find(|(_, character)| {
            units += character.len_utf16();
            units > MAX_OUTPUT_CHARS
        })
        .map_or(full.len(), |(index, _)| index);
    format!("{}\n…", &full[..end])
}

/// A size in bytes, as the merchant reads it: `512 B`, `1.5 KB`, `5.0 MB`.
fn format_size(bytes: usize) -> String {
    const KIB: usize = 1024;
    if bytes < KIB {
        format!("{bytes} B")
    } else if bytes < KIB * KIB {
        format!("{} KB", to_fixed_1(bytes as f64 / KIB as f64))
    } else {
        format!("{} MB", to_fixed_1(bytes as f64 / (KIB * KIB) as f64))
    }
}

/// The output as a file `openshop.js` downloads: the document itself, in a
/// `data:` URL, which needs no request to the server.
fn download_url(full: &str) -> String {
    format!("data:application/json;base64,{}", STANDARD.encode(full))
}

fn output_modal(step: &StepResult, output: &Value) -> Markup {
    let id = modal_id(step);
    let full = serde_json::to_string_pretty(output).unwrap_or_default();
    html! {
        s-modal id=(id) heading=(format!("Output — {}", step.step_name)) size="large" {
            s-stack gap="small-200" {
                s-text color="subdued" { (format_size(full.len())) }
                pre style=(ui::PRE) { (preview(&full)) }
            }
            s-button slot="secondary-actions" data-openshop-copy=(full) { "Copy" }
            s-button
                slot="secondary-actions"
                data-openshop-download=(download_url(&full))
                data-openshop-filename=(format!("{}.json", step.step_name))
            { "Download" }
            s-button slot="primary-action" commandFor=(id) command="--hide" { "Close" }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use serde_json::json;
    use uuid::Uuid;

    fn step(id: u128, name: &str, created_at: &str) -> StepResult {
        StepResult {
            id: Uuid::from_u128(id),
            step_name: name.to_string(),
            attempt: 1,
            status: "completed".to_string(),
            output: None,
            error: None,
            duration_ms: None,
            created_at: created_at.parse::<DateTime<Utc>>().unwrap(),
            flow_run_id: "run".to_string(),
        }
    }

    #[test]
    fn numbers_steps_and_marks_retries() {
        let steps = [
            step(3, "notify", "2026-10-06T12:00:03Z"),
            step(2, "fetch", "2026-10-06T12:00:02Z"),
            step(1, "fetch", "2026-10-06T12:00:01Z"),
            step(4, "fetch", "2026-10-06T12:00:04Z"),
        ];
        let rows = step_rows(&steps);
        let summary: Vec<(u128, &str, bool)> = rows
            .iter()
            .map(|row| (row.step.id.as_u128(), row.label.as_str(), row.is_retry))
            .collect();
        assert_eq!(summary, [(1, "1", false), (2, "1.2", true), (3, "2", false), (4, "1.3", true)]);
        // Steps of the same millisecond, by id.
        let steps = [
            step(2, "second", "2026-10-06T12:00:00.0004Z"),
            step(1, "first", "2026-10-06T12:00:00.0009Z"),
        ];
        let rows = step_rows(&steps);
        assert_eq!(rows[0].step.step_name, "first");
        assert_eq!(rows[1].label, "2");
    }

    #[test]
    fn shows_the_beginning_of_a_long_output() {
        assert_eq!(preview("{}"), "{}");
        let exact = "x".repeat(MAX_OUTPUT_CHARS);
        assert_eq!(preview(&exact), exact);
        assert_eq!(preview(&format!("{exact}yz")), format!("{exact}\n…"));
        let emoji = preview(&"😀".repeat(MAX_OUTPUT_CHARS));
        assert_eq!(
            emoji.chars().filter(|character| *character == '😀').count(),
            MAX_OUTPUT_CHARS / 2
        );
        assert_eq!(format_size(1023), "1023 B");
        assert_eq!(format_size(1536), "1.5 KB");
        assert_eq!(format_size(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn a_step_with_an_output_opens_it_in_a_modal_that_copies_and_downloads_all_of_it() {
        let mut long = step(1, "fetch<all>", "2026-10-06T12:00:01Z");
        long.output = Some(json!({ "note": "<b>&</b>", "items": "y".repeat(3000) }));
        long.duration_ms = Some(42);
        long.error = Some("<script>boom</script>".to_string());
        let mut empty = step(2, "notify", "2026-10-06T12:00:02Z");
        empty.output = Some(Value::Null);
        let steps = [long, empty];
        let rows = step_rows(&steps);
        let table = steps_table(&rows).into_string();
        let id = format!("output-{}", Uuid::from_u128(1));
        assert!(
            table.contains(&format!(r#"<s-button variant="secondary" commandFor="{id}" command="--show">Output</s-button>"#)),
            "{table}"
        );
        assert!(table.contains("<strong>fetch&lt;all&gt;</strong>"), "{table}");
        assert!(table.contains("&lt;script&gt;boom&lt;/script&gt;"), "{table}");
        assert!(table.contains("<s-table-cell>42ms</s-table-cell>"), "{table}");
        // A step whose output is `null` has nothing to show.
        assert_eq!(table.matches("Output</s-button>").count(), 1, "{table}");

        let modals = output_modals(&rows).into_string();
        assert_eq!(modals.matches("<s-modal ").count(), 1, "{modals}");
        assert!(
            modals.contains(&format!(r#"<s-modal id="{id}" heading="Output — fetch&lt;all&gt;" size="large">"#)),
            "{modals}"
        );
        let full = serde_json::to_string_pretty(steps[0].output.as_ref().unwrap()).unwrap();
        // The modal shows the beginning; the buttons copy and download all of it.
        assert!(modals.contains("\n…</pre>"), "{modals}");
        assert!(modals.contains("&quot;note&quot;: &quot;&lt;b&gt;&amp;&lt;/b&gt;&quot;"), "{modals}");
        let escaped = html! { (full) }.into_string();
        assert!(modals.contains(&format!(r#"data-openshop-copy="{escaped}""#)), "{modals}");
        assert!(modals.contains(&format!(r#"data-openshop-download="{}""#, download_url(&full))), "{modals}");
        assert!(modals.contains(r#"data-openshop-filename="fetch&lt;all&gt;.json""#), "{modals}");
        assert!(modals.contains(&format_size(full.len())), "{modals}");
        let encoded = download_url(&full);
        let decoded = STANDARD.decode(encoded.strip_prefix("data:application/json;base64,").unwrap()).unwrap();
        assert_eq!(String::from_utf8(decoded).unwrap(), full);
    }
}
