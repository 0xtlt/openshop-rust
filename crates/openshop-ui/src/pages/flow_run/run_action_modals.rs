use crate::dom::hide_overlay;
use leptos::{prelude::*, task::spawn_local};
use std::{future::Future, pin::Pin};

const RETRY_MODAL_ID: &str = "retry-modal";
const DELETE_MODAL_ID: &str = "delete-modal";

pub type ActionFuture = Pin<Box<dyn Future<Output = ()>>>;

#[component]
pub fn RunActionModals(
    retrying: RwSignal<bool>,
    retry_run: Callback<&'static str, ActionFuture>,
    delete_run: Callback<(), ActionFuture>,
) -> impl IntoView {
    let retry = move |mode: &'static str| {
        let retried = retry_run.run(mode);
        spawn_local(async move {
            retried.await;
            hide_overlay(RETRY_MODAL_ID);
        });
    };

    view! {
        <s-modal id=RETRY_MODAL_ID heading="Retry run" accessibility-label="Choose retry mode">
            <s-stack direction="block" gap="base">
                <s-text>"How do you want to retry this run?"</s-text>
                <s-button variant="primary" disabled=move || retrying.get() on:click=move |_| retry("resume")>
                    "Resume — pick up where it failed"
                </s-button>
                <s-button variant="secondary" disabled=move || retrying.get() on:click=move |_| retry("reset")>
                    "Restart — discard steps and start fresh"
                </s-button>
            </s-stack>
            <s-button slot="secondary-actions" variant="secondary" commandFor=RETRY_MODAL_ID command="--hide">
                "Cancel"
            </s-button>
        </s-modal>

        <s-modal id=DELETE_MODAL_ID heading="Delete run" accessibility-label="Confirm run deletion">
            <s-text>
                "This will permanently delete this run, its steps, and all logs. This cannot be undone."
            </s-text>
            <s-button
                slot="primary-action"
                variant="primary"
                tone="critical"
                on:click=move |_| {
                    hide_overlay(DELETE_MODAL_ID);
                    spawn_local(delete_run.run(()));
                }
            >
                "Delete"
            </s-button>
            <s-button slot="secondary-actions" variant="secondary" commandFor=DELETE_MODAL_ID command="--hide">
                "Cancel"
            </s-button>
        </s-modal>
    }
}
