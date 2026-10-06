use crate::{dom::hide_modal, events::event_value};
use leptos::{ev, html, prelude::*};

pub const RUN_MODAL_ID: &str = "run-modal";
pub const DELETE_MODAL_ID: &str = "delete-modal";

/// Title bar action of an App Bridge modal; App Bridge reads `variant` and `tone` from the button.
#[component]
pub fn ModalButton(
    #[prop(optional)] variant: Option<&'static str>,
    #[prop(optional)] tone: Option<&'static str>,
    #[prop(optional, into)] disabled: Signal<bool>,
    on_click: Callback<()>,
    children: Children,
) -> impl IntoView {
    html::button()
        .attr("variant", variant)
        .attr("tone", tone)
        .disabled(move || disabled.get())
        .on(ev::click, move |_| on_click.run(()))
        .child(children())
}

#[component]
pub fn RunFlowModal(
    selected: RwSignal<Option<String>>,
    input_json: RwSignal<String>,
    input_error: RwSignal<Option<String>>,
    triggering: RwSignal<bool>,
    trigger_run: Callback<()>,
) -> impl IntoView {
    let textarea_style = move || {
        let border = if input_error.with(Option::is_some) {
            "2px solid #d72c0d"
        } else {
            "1px solid #c9cccf"
        };
        format!(
            "width:100%;font-family:ui-monospace, SFMono-Regular, Menlo, monospace;font-size:13px;padding:8px 12px;border-radius:8px;border:{border};resize:vertical;box-sizing:border-box",
        )
    };

    view! {
        <ui-modal id=RUN_MODAL_ID>
            <div style="padding:16px">
                <label
                    for="flow-input-json"
                    style="display:block;margin-bottom:4px;font-weight:600;font-size:13px"
                >
                    "Input (JSON)"
                </label>
                <textarea
                    id="flow-input-json"
                    prop:value=move || input_json.get()
                    on:input=move |event| {
                        input_json.set(event_value(&event));
                        input_error.set(None);
                    }
                    placeholder="{ \"limit\": 10 }"
                    rows="6"
                    aria-invalid=move || input_error.with(Option::is_some).to_string()
                    aria-describedby=move || input_error.with(Option::is_some).then_some("flow-input-json-error")
                    style=textarea_style
                ></textarea>
                {move || {
                    input_error
                        .get()
                        .map(|error| {
                            view! {
                                <span id="flow-input-json-error" style="color:#d72c0d;font-size:12px">
                                    {error}
                                </span>
                            }
                        })
                }}
            </div>
            <ui-title-bar title=move || format!("Run {}", selected.get().unwrap_or_default())>
                <ModalButton variant="primary" on_click=trigger_run disabled=triggering>
                    {move || if triggering.get() { "Running…" } else { "Run" }}
                </ModalButton>
                <ModalButton on_click=Callback::new(|()| hide_modal(RUN_MODAL_ID))>"Cancel"</ModalButton>
            </ui-title-bar>
        </ui-modal>
    }
}

#[component]
pub fn DeleteRunsModal(
    #[prop(into)] checked_count: Signal<usize>,
    deleting: RwSignal<bool>,
    bulk_delete: Callback<()>,
) -> impl IntoView {
    view! {
        <ui-modal id=DELETE_MODAL_ID>
            <div style="padding:16px">
                <s-text>
                    "Are you sure you want to delete " {move || checked_count.get()}
                    " run(s)? This action cannot be undone."
                </s-text>
            </div>
            <ui-title-bar title="Delete runs">
                <ModalButton variant="primary" tone="critical" on_click=bulk_delete disabled=deleting>
                    {move || if deleting.get() { "Deleting…" } else { "Delete" }}
                </ModalButton>
                <ModalButton on_click=Callback::new(|()| hide_modal(DELETE_MODAL_ID))>"Cancel"</ModalButton>
            </ui-title-bar>
        </ui-modal>
    }
}
