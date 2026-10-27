use crate::{
    components::json_modal::{JsonModal, json_modal_document, show_json_modal},
    dom::{self, element_by_id},
    format::{format_size, parse_timestamp},
    json,
    types::{StepResult, status_tone},
};
use leptos::prelude::*;
use std::collections::HashMap;

const OUTPUT_MODAL_ID: &str = "output-modal";
const OUTPUT_SIZE_ID: &str = "output-modal-size";
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

fn show_output(step: &StepResult) {
    let Some(output) = &step.output else { return };
    let Some(modal) = element_by_id(OUTPUT_MODAL_ID) else {
        return;
    };
    let full = json::pretty(output);
    // What the download is named after.
    let _ = modal.set_attribute("data-step-name", &step.step_name);
    if let Some(size) = element_by_id(OUTPUT_SIZE_ID) {
        size.set_text_content(Some(&format_size(full.len())));
    }
    show_json_modal(
        OUTPUT_MODAL_ID,
        &format!("Output — {}", step.step_name),
        &preview(&full),
        &full,
    );
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
    let download = |_| {
        if let Some(full) = json_modal_document(OUTPUT_MODAL_ID) {
            let step_name = element_by_id(OUTPUT_MODAL_ID).and_then(|modal| modal.get_attribute("data-step-name"));
            let filename = format!("{}.json", step_name.as_deref().unwrap_or("output"));
            dom::download_text(&full, "application/json", &filename);
        }
    };

    view! {
        <JsonModal id=OUTPUT_MODAL_ID title="Output">
            <s-button variant="secondary" on:click=download>
                "Download"
            </s-button>
            <span id=OUTPUT_SIZE_ID style="color:#8c9196;font-size:12px;align-self:center;margin-left:auto"></span>
        </JsonModal>
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
        assert_eq!(preview("{}"), "{}");
        let exact = "x".repeat(MAX_OUTPUT_CHARS);
        assert_eq!(preview(&exact), exact);
        assert_eq!(preview(&format!("{exact}yz")), format!("{exact}\n…"));
        let emoji = preview(&"😀".repeat(MAX_OUTPUT_CHARS));
        assert_eq!(
            emoji.chars().filter(|character| *character == '😀').count(),
            MAX_OUTPUT_CHARS / 2
        );
    }
}
