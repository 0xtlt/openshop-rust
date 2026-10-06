use crate::{
    events::{event_checked, event_value},
    types::ConfigField,
};
use leptos::prelude::*;

/// Renders one provider or function configuration field. Values are edited as strings;
/// checkboxes use `"true"` / `"false"`.
#[component]
pub fn ConfigFieldRenderer(
    field_key: String,
    field: ConfigField,
    #[prop(into)] value: Signal<String>,
    on_change: Callback<(String, String)>,
) -> impl IntoView {
    let ConfigField {
        kind,
        label,
        placeholder,
        options,
        has_value,
    } = field;
    let on_input = {
        let field_key = field_key.clone();
        move |event: web_sys::Event| on_change.run((field_key.clone(), event_value(&event)))
    };

    match (kind.as_str(), options) {
        ("password", _) => {
            let placeholder = if has_value == Some(true) {
                Some("Saved; leave blank to keep".to_string())
            } else {
                placeholder
            };
            view! {
                <s-password-field
                    label=label
                    placeholder=placeholder
                    prop:value=move || value.get()
                    on:input=on_input
                />
            }
            .into_any()
        }
        ("number", _) => view! {
            <s-number-field
                label=label
                placeholder=placeholder
                prop:value=move || value.get()
                on:input=on_input
            />
        }
        .into_any(),
        ("checkbox", _) => view! {
            <s-checkbox
                label=label
                prop:checked=move || value.get() == "true"
                on:change=move |event: web_sys::Event| {
                    on_change.run((field_key.clone(), event_checked(&event).to_string()))
                }
            />
        }
        .into_any(),
        ("select", Some(options)) => view! {
            <s-select label=label prop:value=move || value.get() on:change=on_input>
                {options
                    .into_iter()
                    .map(|option| view! { <s-option value=option.value>{option.label}</s-option> })
                    .collect_view()}
            </s-select>
        }
        .into_any(),
        _ => view! {
            <s-text-field
                label=label
                placeholder=placeholder
                prop:value=move || value.get()
                on:input=on_input
            />
        }
        .into_any(),
    }
}
