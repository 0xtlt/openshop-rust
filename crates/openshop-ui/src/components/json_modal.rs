use crate::dom::{self, element_by_id, hide_modal, show_modal};
use leptos::{prelude::*, task::spawn_local};

fn pre_id(id: &str) -> String {
    format!("{id}-pre")
}

fn title_id(id: &str) -> String {
    format!("{id}-title")
}

/// The whole document the modal `id` shows, as [`show_json_modal`] gave it.
pub fn json_modal_document(id: &str) -> Option<String> {
    element_by_id(&pre_id(id))?
        .get_attribute("data-full")
        .filter(|full| !full.is_empty())
}

/// Fills the [`JsonModal`] `id` and opens it: `shown` is what it displays, and
/// `full` the document it copies.
pub fn show_json_modal(id: &str, title: &str, shown: &str, full: &str) {
    if element_by_id(id).is_none() {
        return;
    }
    if let Some(pre) = element_by_id(&pre_id(id)) {
        pre.set_text_content(Some(shown));
        let _ = pre.set_attribute("data-full", full);
    }
    if let Some(title_bar) = element_by_id(&title_id(id)) {
        let _ = title_bar.set_attribute("title", title);
    }
    show_modal(id);
}

/// A modal showing a JSON document, with a button to copy it. `children` go
/// next to that button.
#[component]
pub fn JsonModal(id: &'static str, title: &'static str, #[prop(optional)] children: Option<Children>) -> impl IntoView {
    let copy = move |_| {
        if let Some(full) = json_modal_document(id) {
            spawn_local(async move {
                let _ = dom::copy_text(&full).await;
            });
        }
    };

    view! {
        <ui-modal id=id>
            <div style="padding:16px">
                <div style="display:flex;gap:6px;margin-bottom:8px">
                    <s-button variant="secondary" on:click=copy>
                        "Copy"
                    </s-button>
                    {children.map(|children| children())}
                </div>
                <pre
                    id=pre_id(id)
                    style="font-family:ui-monospace, SFMono-Regular, Menlo, monospace;font-size:12px;line-height:1.5;background:#fafafa;border-radius:6px;padding:8px 12px;overflow:auto;max-height:60vh;white-space:pre-wrap;word-break:break-word;margin:0;border:1px solid #e3e3e3"
                ></pre>
            </div>
            <ui-title-bar id=title_id(id) title=title>
                <button on:click=move |_| hide_modal(id)>"Close"</button>
            </ui-title-bar>
        </ui-modal>
    }
}
