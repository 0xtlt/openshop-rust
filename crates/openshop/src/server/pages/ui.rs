//! What several pages of the framework are made of: a field of a
//! configuration and the form it is read from, a date, a JSON document in a
//! modal.

// The pages that use them are ported next.
#![allow(dead_code)]

use chrono::{DateTime, Utc};
use maud::{Markup, html};
use serde_json::Value;

use crate::admin::AdminPublicError;
use crate::server::provider_config::ConfigError;
use crate::types::{Field, FieldType, Fields, JsonMap};

/// The style of a `<pre>` that shows JSON or logs.
pub(super) const PRE: &str = "font-family:ui-monospace,SFMono-Regular,Menlo,monospace;font-size:12px;line-height:1.5;background:#fafafa;border-radius:6px;padding:8px 12px;overflow:auto;max-height:60vh;white-space:pre-wrap;word-break:break-word;margin:0;border:1px solid #e3e3e3";

/// A date, written by the server in UTC, which `openshop.js` writes again in
/// the language and the time zone of the browser.
pub(super) fn time(iso: &str) -> Markup {
    let shown = DateTime::parse_from_rfc3339(iso)
        .map(|date| date.with_timezone(&Utc).format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_else(|_| iso.to_string());
    html! { time datetime=(iso) { (shown) } }
}

/// A JSON document in a modal, which a button opens with
/// `commandFor=(id) command="--show"`, and a button that copies it.
pub(super) fn json_modal(id: &str, heading: &str, value: &Value) -> Markup {
    let text = serde_json::to_string_pretty(value).unwrap_or_default();
    html! {
        s-modal id=(id) heading=(heading) size="large" {
            pre style=(PRE) { (text) }
            s-button slot="secondary-actions" data-openshop-copy=(text) { "Copy" }
            s-button slot="primary-action" commandFor=(id) command="--hide" { "Close" }
        }
    }
}

/// A value of a configuration as its field shows it: text as it is, a
/// number or a boolean written out (`3`, `true`), nothing for `null`.
pub(super) fn config_value(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
    }
}

/// A field of the configuration of a provider or of a function instance, in
/// its form: named `name`, showing `value`, with `error` under it. `saved`
/// says that a password has a value saved, which is never shown.
pub(super) fn config_field(name: &str, field: &Field, value: &str, saved: bool, error: Option<&str>) -> Markup {
    let label = &field.label;
    let details = field.description.as_deref();
    let placeholder = field.placeholder.as_deref();
    match field.field_type {
        FieldType::Password => {
            let placeholder = if saved { Some("Saved; leave blank to keep") } else { placeholder };
            html! {
                s-password-field name=(name) label=(label) details=[details] placeholder=[placeholder] error=[error] {}
            }
        }
        FieldType::Number => html! {
            s-number-field name=(name) label=(label) details=[details] placeholder=[placeholder] value=(value) error=[error] {}
        },
        FieldType::Checkbox => html! {
            s-checkbox name=(name) value="true" label=(label) details=[details] checked[value == "true"] error=[error] {}
        },
        FieldType::Select => html! {
            s-select name=(name) label=(label) details=[details] value=(value) error=[error] {
                @for option in field.options.iter().flatten() {
                    s-option value=(option.value) selected[option.value == value] { (option.label) }
                }
            }
        },
        FieldType::Text => html! {
            s-text-field name=(name) label=(label) details=[details] placeholder=[placeholder] value=(value) error=[error] {}
        },
    }
}

/// The configuration a form sent, as the API receives it: the field
/// `{prefix}{name}` of each field of `fields`, a checkbox that was not
/// checked `false` (a form sends nothing for it).
pub(super) fn config_from_form(fields: &Fields, form: &[(String, String)], prefix: &str) -> Value {
    let mut config = JsonMap::new();
    for (name, field) in fields {
        let sent = super::field(form, &format!("{prefix}{name}"));
        let value = match (field.field_type, sent) {
            (FieldType::Checkbox, sent) => Value::from(if sent == Some("true") { "true" } else { "false" }),
            (_, Some(sent)) => Value::from(sent),
            (_, None) => continue,
        };
        config.insert(name.clone(), value);
    }
    Value::Object(config)
}

/// A configuration refused, as the form shows it: `422`, with each error
/// under the field it is about (`{prefix}{name}`).
pub(super) fn config_refusal(error: ConfigError, prefix: &str) -> anyhow::Error {
    let refusal = AdminPublicError::new("INVALID_CONFIG", error.message).status(422);
    if error.fields.is_empty() {
        return refusal.into();
    }
    refusal
        .field_errors(
            error
                .fields
                .into_iter()
                .map(|(name, shown)| (format!("{prefix}{name}"), shown)),
        )
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::provider_config::parse_provider_config;
    use crate::types::FieldOption;
    use crate::validators;
    use indexmap::IndexMap;
    use serde_json::json;

    fn fields() -> Fields {
        IndexMap::from([
            ("endpoint".to_string(), Field::text("Endpoint").validate(validators::url())),
            ("apiKey".to_string(), Field::password("API key")),
            ("retries".to_string(), Field::number("Retries").optional()),
            ("sandbox".to_string(), Field::checkbox("Sandbox")),
            (
                "zone".to_string(),
                Field::select("Zone", [FieldOption::new("Europe", "eu"), FieldOption::new("US", "us")]),
            ),
        ])
    }

    #[test]
    fn renders_each_kind_of_field_with_its_value_and_its_error() {
        let fields = fields();
        let render = |name: &str, value: &str, saved: bool, error: Option<&str>| {
            config_field(&format!("config.{name}"), &fields[name], value, saved, error).into_string()
        };
        assert_eq!(
            render("endpoint", "https://x", false, Some("Must be a URL")),
            r#"<s-text-field name="config.endpoint" label="Endpoint" value="https://x" error="Must be a URL"></s-text-field>"#
        );
        // A password never shows its value, only that one is saved.
        assert_eq!(
            render("apiKey", "secret", true, None),
            r#"<s-password-field name="config.apiKey" label="API key" placeholder="Saved; leave blank to keep"></s-password-field>"#
        );
        assert!(render("sandbox", "true", false, None).contains(r#"value="true" label="Sandbox" checked>"#));
        assert!(!render("sandbox", "false", false, None).contains("checked"));
        let zone = render("zone", "us", false, None);
        assert!(zone.contains(r#"<s-option value="us" selected>US</s-option>"#), "{zone}");
        assert!(zone.contains(r#"<s-option value="eu">Europe</s-option>"#), "{zone}");
        assert_eq!(config_value(Some(&json!(3))), "3");
        assert_eq!(config_value(Some(&json!(null))), "");
    }

    #[test]
    fn reads_a_configuration_from_its_form() {
        let form = [
            ("_action".to_string(), "save".to_string()),
            ("config.endpoint".to_string(), "not a url".to_string()),
            ("config.retries".to_string(), "3".to_string()),
            ("config.zone".to_string(), "eu".to_string()),
        ];
        let config = config_from_form(&fields(), &form, "config.");
        assert_eq!(
            config,
            json!({ "endpoint": "not a url", "retries": "3", "sandbox": "false", "zone": "eu" })
        );
        let refused = parse_provider_config(&fields(), None, Some(&config), &JsonMap::new()).unwrap_err();
        let error = config_refusal(refused, "config.");
        let public = error.downcast_ref::<AdminPublicError>().unwrap();
        assert_eq!(public.status, 422);
        let errors = public.field_errors.as_ref().unwrap();
        assert_eq!(errors["config.apiKey"], "Required");
        let sent = json!({ "endpoint": "not a url", "apiKey": "k", "sandbox": "false", "zone": "eu" });
        let refused = parse_provider_config(&fields(), None, Some(&sent), &JsonMap::new()).unwrap_err();
        let error = config_refusal(refused, "config.");
        let errors = error.downcast_ref::<AdminPublicError>().unwrap().field_errors.clone().unwrap();
        assert_eq!(errors["config.endpoint"], "Must be a URL string (was \"not a url\")");
    }

    #[test]
    fn writes_a_date_in_utc_for_the_browser_to_localize() {
        assert_eq!(
            time("2026-11-09T15:56:46.123+01:00").into_string(),
            r#"<time datetime="2026-11-09T15:56:46.123+01:00">2026-11-09 14:56:46 UTC</time>"#
        );
        assert_eq!(time("soon").into_string(), r#"<time datetime="soon">soon</time>"#);
    }
}
