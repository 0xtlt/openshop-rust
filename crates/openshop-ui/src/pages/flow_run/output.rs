use crate::{
    dom::{self, element_by_id, hide_modal, show_modal},
    format::{format_size, parse_timestamp},
    json,
    types::{StepResult, status_tone},
};
use leptos::{prelude::*, task::spawn_local};
use std::collections::HashMap;

const OUTPUT_MODAL_ID: &str = "output-modal";
const OUTPUT_PRE_ID: &str = "output-modal-pre";
const MAX_OUTPUT_CHARS: usize = 2000;

/// A step with its position label: `2` for the second distinct step, `2.3` for its third attempt.
#[derive(Debug, Clone, PartialEq)]
pub struct StepRowData {
    pub step: StepResult,
    pub label: String,
    pub is_retry: bool,
}

pub fn build_step_rows(steps: &[StepResult]) -> Vec<StepRowData> {
    let created = |step: &StepResult| step.created_at.as_deref().and_then(parse_timestamp).unwrap_or(0);
    let mut sorted: Vec<&StepResult> = steps.iter().collect();
    sorted.sort_by(|left, right| created(left).cmp(&created(right)).then_with(|| left.id.cmp(&right.id)));

    let mut base_index_by_name: HashMap<&str, usize> = HashMap::new();
    let mut occurrences_by_name: HashMap<&str, usize> = HashMap::new();
    sorted
        .into_iter()
        .map(|step| {
            let next_index = base_index_by_name.len() + 1;
            let base_index = *base_index_by_name.entry(&step.step_name).or_insert(next_index);
            let occurrence = occurrences_by_name.entry(&step.step_name).or_insert(0);
            *occurrence += 1;
            StepRowData {
                step: step.clone(),
                label: if *occurrence == 1 {
                    base_index.to_string()
                } else {
                    format!("{base_index}.{occurrence}")
                },
                is_retry: *occurrence > 1,
            }
        })
        .collect()
}

/// Shortens long outputs for display; lengths are UTF-16 units like JavaScript strings.
fn preview(full: &str) -> (String, bool) {
    if full.encode_utf16().count() <= MAX_OUTPUT_CHARS {
        return (full.to_string(), false);
    }
    let mut units = 0;
    let end = full
        .char_indices()
        .find(|(_, character)| {
            units += character.len_utf16();
            units > MAX_OUTPUT_CHARS
        })
        .map_or(full.len(), |(index, _)| index);
    (format!("{}\n…", &full[..end]), true)
}

fn show_output(step: &StepResult) {
    let Some(output) = &step.output else { return };
    if element_by_id(OUTPUT_MODAL_ID).is_none() {
        return;
    }
    if let Some(pre) = element_by_id(OUTPUT_PRE_ID) {
        let full = json::pretty(output);
        let (shown, truncated) = preview(&full);
        let size = format_size(full.len());
        pre.set_text_content(Some(&shown));
        let _ = pre.set_attribute("data-full", &full);
        let _ = pre.set_attribute("data-step-name", &step.step_name);
        let _ = pre.set_attribute("data-truncated", &truncated.to_string());
        let _ = pre.set_attribute("data-size", &size);
        if let Some(size_element) = element_by_id("output-modal-size") {
            size_element.set_text_content(Some(&size));
        }
    }
    if let Some(title) = element_by_id("output-modal-title") {
        let _ = title.set_attribute("title", &format!("Output — {}", step.step_name));
    }
    show_modal(OUTPUT_MODAL_ID);
}

fn stored_output() -> Option<(String, Option<String>)> {
    let pre = element_by_id(OUTPUT_PRE_ID)?;
    let full = pre.get_attribute("data-full").filter(|full| !full.is_empty())?;
    Some((full, pre.get_attribute("data-step-name")))
}

pub fn step_row(row: StepRowData) -> impl IntoView {
    let StepRowData { step, label, is_retry } = row;
    let duration = step.duration_ms.map_or_else(|| "—".to_string(), |ms| format!("{ms}ms"));
    let error = step.error.clone().filter(|error| !error.is_empty());
    let has_output = step.output.is_some();
    view! {
        <s-table-row>
            <s-table-cell>
                <span style="display:inline-flex;align-items:center;gap:8px">
                    <span style="display:inline-flex;align-items:center;justify-content:center;min-width:20px;height:20px;border-radius:999px;padding:0 6px;background:#f1f1f1;font-size:11px;font-weight:700;color:#616161;flex-shrink:0">
                        {label}
                    </span>
                    <strong>{step.step_name.clone()}</strong>
                    {is_retry.then(|| view! { <span style="color:#8c9196;font-size:12px">"retry"</span> })}
                </span>
            </s-table-cell>
            <s-table-cell>
                <s-badge tone=status_tone(&step.status)>{step.status.clone()}</s-badge>
            </s-table-cell>
            <s-table-cell>{duration}</s-table-cell>
            <s-table-cell>
                {error.map(|error| view! { <span style="color:#d72c0d;font-size:13px">{error}</span> })}
                {has_output
                    .then(|| {
                        view! {
                            <s-button variant="secondary" on:click=move |_| show_output(&step)>
                                "Output"
                            </s-button>
                        }
                    })}
            </s-table-cell>
        </s-table-row>
    }
}

#[component]
pub fn OutputModal() -> impl IntoView {
    let copy = |_| {
        if let Some((full, _)) = stored_output() {
            spawn_local(async move {
                let _ = dom::copy_text(&full).await;
            });
        }
    };
    let download = |_| {
        if let Some((full, step_name)) = stored_output() {
            let filename = format!("{}.json", step_name.as_deref().unwrap_or("output"));
            dom::download_text(&full, "application/json", &filename);
        }
    };

    view! {
        <ui-modal id=OUTPUT_MODAL_ID>
            <div style="padding:16px">
                <div style="display:flex;gap:6px;margin-bottom:8px">
                    <s-button variant="secondary" on:click=copy>
                        "Copy"
                    </s-button>
                    <s-button variant="secondary" on:click=download>
                        "Download"
                    </s-button>
                    <span
                        id="output-modal-size"
                        style="color:#8c9196;font-size:12px;align-self:center;margin-left:auto"
                    ></span>
                </div>
                <pre
                    id=OUTPUT_PRE_ID
                    style="font-family:ui-monospace, SFMono-Regular, Menlo, monospace;font-size:12px;line-height:1.5;background:#fafafa;border-radius:6px;padding:8px 12px;overflow:auto;max-height:60vh;white-space:pre-wrap;word-break:break-word;margin:0;border:1px solid #e3e3e3"
                ></pre>
            </div>
            <ui-title-bar id="output-modal-title" title="Output">
                <button on:click=|_| hide_modal(OUTPUT_MODAL_ID)>"Close"</button>
            </ui-title-bar>
        </ui-modal>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(id: &str, name: &str, created_at: &str) -> StepResult {
        StepResult {
            id: id.to_string(),
            step_name: name.to_string(),
            status: "completed".to_string(),
            output: None,
            error: None,
            duration_ms: None,
            created_at: Some(created_at.to_string()),
        }
    }

    #[test]
    fn numbers_steps_and_marks_retries() {
        let steps = [
            step("c", "notify", "2026-10-06T12:00:03.000Z"),
            step("b", "fetch", "2026-10-06T12:00:02.000Z"),
            step("a", "fetch", "2026-10-06T12:00:01.000Z"),
            step("d", "fetch", "2026-10-06T12:00:04.000Z"),
        ];
        let rows = build_step_rows(&steps);
        let summary: Vec<(&str, &str, bool)> = rows
            .iter()
            .map(|row| (row.step.id.as_str(), row.label.as_str(), row.is_retry))
            .collect();
        assert_eq!(
            summary,
            [
                ("a", "1", false),
                ("b", "1.2", true),
                ("c", "2", false),
                ("d", "1.3", true)
            ]
        );
    }

    #[test]
    fn orders_simultaneous_steps_by_id() {
        let steps = [
            step("b", "second", "2026-10-06T12:00:00.000Z"),
            step("a", "first", "2026-10-06T12:00:00.000Z"),
        ];
        let rows = build_step_rows(&steps);
        assert_eq!(rows[0].step.step_name, "first");
        assert_eq!(rows[1].label, "2");
    }

    #[test]
    fn truncates_long_outputs_for_display() {
        assert_eq!(preview("{}"), ("{}".to_string(), false));
        let exact = "x".repeat(MAX_OUTPUT_CHARS);
        assert_eq!(preview(&exact), (exact.clone(), false));
        let (shown, truncated) = preview(&format!("{exact}yz"));
        assert!(truncated);
        assert_eq!(shown, format!("{exact}\n…"));
        let (emoji, _) = preview(&"😀".repeat(MAX_OUTPUT_CHARS));
        assert_eq!(
            emoji.chars().filter(|character| *character == '😀').count(),
            MAX_OUTPUT_CHARS / 2
        );
    }
}
