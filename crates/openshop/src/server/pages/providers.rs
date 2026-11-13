//! `/providers`: the configuration of a provider of the application for the
//! shop. The provider shown is the one of the URL (`?provider=<name>`, the
//! first one otherwise), chosen with a select when there are several. The
//! page shows its connection status and a form of its fields, which saves
//! them (a field refused shows its error under it); a second form tests the
//! connection; then when it was last checked.

use anyhow::Result;
use maud::{Markup, html};

use super::{BuiltinPage, field, ui};
use crate::admin::{AdminPublicError, FormState, Outcome};
use crate::server::api::providers::{
    BlankPassword, CheckOutcome, ProviderItem, SaveOutcome, check_provider_connection, provider_items,
    save_provider_config,
};

/// The form of the configuration.
const SAVE: &str = "save";
/// The form of the connection check.
const CHECK: &str = "check";
/// The field, a query parameter or a hidden field of a form, that names the provider.
const PROVIDER: &str = "provider";
/// The prefix of the name of each field of the configuration in its form:
/// `config.endpoint`. The fields of the page are not named after the fields of a provider.
const CONFIG: &str = "config.";

/// The badge of the connection of a provider, `(tone, label)`: whether it is
/// configured, then what its last check said.
pub(super) fn connection_status(configured: bool, last_check_ok: Option<bool>) -> (&'static str, &'static str) {
    match (configured, last_check_ok) {
        (false, _) => ("warning", "Not configured"),
        (true, Some(true)) => ("success", "Connected"),
        (true, Some(false)) => ("critical", "Error"),
        (true, None) => ("info", "Configured"),
    }
}

fn not_found() -> anyhow::Error {
    AdminPublicError::new("NOT_FOUND", "Provider not found")
        .status(404)
        .into()
}

pub(super) async fn render(page: &BuiltinPage<'_>) -> Result<Markup> {
    let providers = provider_items(page.app, page.session).await?;
    let selected = match page.param(PROVIDER).filter(|name| !name.is_empty()) {
        None => 0,
        Some(name) => providers
            .iter()
            .position(|provider| provider.name == name)
            .ok_or_else(not_found)?,
    };
    Ok(view(&providers, selected, page.form))
}

/// The page, showing the provider at `selected`; `form`, the form the page refused.
fn view(providers: &[ProviderItem<'_>], selected: usize, form: &FormState) -> Markup {
    let Some(current) = providers.get(selected) else {
        return html! {
            s-page heading="Providers" {
                s-section { s-text color="subdued" { "No providers registered." } }
            }
        };
    };
    // The form refused is shown again on the provider it was sent for only.
    let none = FormState::default();
    let refused = form.of(SAVE);
    let saving = if refused.value(PROVIDER) == Some(current.name) {
        refused
    } else {
        &none
    };
    let (tone, label) = connection_status(current.configured, current.last_check_ok);
    html! {
        s-page heading="Providers" {
            @if providers.len() > 1 {
                s-section {
                    // Sent when the select changes; "Show" without the script.
                    form method="get" data-openshop-autosubmit {
                        s-stack direction="inline" gap="base" alignItems="end" {
                            s-select name=(PROVIDER) label="Provider" value=(current.name) {
                                @for provider in providers {
                                    s-option value=(provider.name) selected[provider.name == current.name] { (provider.name) }
                                }
                            }
                            s-button type="submit" variant="secondary" { "Show" }
                        }
                    }
                }
            }
            s-section heading=(current.name) {
                s-box paddingBlockEnd="base" {
                    s-stack direction="inline" gap="base" alignItems="center" {
                        s-badge tone=(tone) { (label) }
                        form method="post" {
                            input type="hidden" name="_action" value=(CHECK);
                            input type="hidden" name=(PROVIDER) value=(current.name);
                            s-button type="submit" variant="secondary" { "Test connection" }
                        }
                    }
                }
                form method="post" {
                    input type="hidden" name="_action" value=(SAVE);
                    input type="hidden" name=(PROVIDER) value=(current.name);
                    s-stack gap="base" {
                        @for (name, config_field) in &current.provider.fields {
                            @let input = format!("{CONFIG}{name}");
                            @let saved = ui::config_value(current.config.get(name));
                            (ui::config_field(
                                &input,
                                config_field,
                                saving.value_or(&input, &saved),
                                current.has_saved_value(name),
                                saving.error(&input),
                            ))
                        }
                        s-box {
                            s-button type="submit" variant="primary" { "Save" }
                        }
                    }
                }
            }
            @if let Some(checked_at) = current.last_checked_at.as_deref().filter(|at| !at.is_empty()) {
                s-section {
                    s-text color="subdued" { "Last checked: " (ui::time(checked_at)) }
                }
            }
        }
    }
}

/// The forms of the page: the configuration saved, or refused field by
/// field; the connection tested, said in a toast. The page is rendered again
/// after either, with the status and the time of the last check.
pub(super) async fn submit(page: &BuiltinPage<'_>, action: &str, fields: &[(String, String)]) -> Result<Outcome> {
    let name = field(fields, PROVIDER).unwrap_or_default();
    match action {
        SAVE => {
            let Some(provider) = page.app.config().providers.get(name) else {
                return Err(not_found());
            };
            let config = ui::config_from_form(&provider.fields, fields, CONFIG);
            match save_provider_config(
                page.app,
                page.session.into(),
                name,
                Some(&config),
                BlankPassword::KeepsSaved,
            )
            .await?
            {
                SaveOutcome::Saved => Ok(Outcome::toast("Saved.")),
                SaveOutcome::NotFound => Err(not_found()),
                SaveOutcome::Refused(error) => Err(ui::config_refusal(error, CONFIG)),
            }
        }
        CHECK => check(page, name).await,
        _ => Err(super::no_action()),
    }
}

async fn check(page: &BuiltinPage<'_>, name: &str) -> Result<Outcome> {
    let outcome = check_provider_connection(page.app, page.session.into(), name).await?;
    match (&outcome, outcome.error()) {
        (CheckOutcome::NotFound, _) => Err(not_found()),
        (CheckOutcome::NoChecker, _) => Err(AdminPublicError::new("NO_CHECKER", "No checker defined")
            .status(400)
            .into()),
        (CheckOutcome::Answered(true), _) => Ok(Outcome::toast("Connection OK!")),
        // The status and the time of the check are shown again with the page.
        (CheckOutcome::Answered(false), _) => Ok(Outcome::error("Failed")),
        (CheckOutcome::TimedOut | CheckOutcome::Failed(_), error) => Ok(Outcome::error(error.unwrap_or_default())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Field, FieldOption, JsonMap, Provider};
    use indexmap::IndexMap;
    use serde_json::{Value, json};

    fn warehouse() -> Provider {
        Provider::new("warehouse")
            .field("endpoint", Field::text("Endpoint"))
            .field("apiKey", Field::password("API key"))
            .field("sandbox", Field::checkbox("Sandbox").optional())
            .field(
                "zone",
                Field::select("Zone", [FieldOption::new("Europe", "eu"), FieldOption::new("US", "us")]),
            )
    }

    fn object(value: Value) -> JsonMap {
        value.as_object().cloned().unwrap_or_default()
    }

    fn item<'a>(name: &'a str, provider: &'a Provider) -> ProviderItem<'a> {
        ProviderItem {
            name,
            provider,
            fields: object(json!({ "apiKey": { "type": "password", "label": "API key", "hasValue": true } })),
            config: object(json!({ "endpoint": "https://warehouse.test", "sandbox": true, "zone": "us" })),
            configured: true,
            last_checked_at: Some("2026-11-09T14:56:46.123Z".to_string()),
            last_check_ok: Some(false),
        }
    }

    fn refused(values: &[(&str, &str)], errors: &[(&str, &str)]) -> FormState {
        FormState {
            action: Some(SAVE.to_string()),
            values: values
                .iter()
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect(),
            errors: errors
                .iter()
                .map(|(name, error)| (name.to_string(), error.to_string()))
                .collect::<IndexMap<_, _>>(),
            message: None,
        }
    }

    #[test]
    fn renders_the_status_the_form_and_the_check_of_a_provider() {
        let provider = warehouse();
        let html = view(&[item("warehouse", &provider)], 0, &FormState::default()).into_string();
        assert!(html.contains(r#"<s-section heading="warehouse">"#), "{html}");
        assert!(html.contains(r#"<s-badge tone="critical">Error</s-badge>"#), "{html}");
        // Two forms: the check, then the configuration, each naming the provider.
        let check = html
            .find(r#"<input type="hidden" name="_action" value="check">"#)
            .unwrap();
        let save = html
            .find(r#"<input type="hidden" name="_action" value="save">"#)
            .unwrap();
        assert!(check < save, "{html}");
        assert_eq!(
            html.matches(r#"<input type="hidden" name="provider" value="warehouse">"#)
                .count(),
            2,
            "{html}"
        );
        assert!(html.contains(r#"<s-button type="submit" variant="secondary">Test connection</s-button>"#));
        assert!(html.contains(r#"<s-button type="submit" variant="primary">Save</s-button>"#));
        // The fields, with what is saved; a password only says that it is.
        assert!(
            html.contains(r#"<s-text-field name="config.endpoint" label="Endpoint" value="https://warehouse.test"></s-text-field>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<s-password-field name="config.apiKey" label="API key" placeholder="Saved; leave blank to keep"></s-password-field>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"name="config.sandbox" value="true" label="Sandbox" checked>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<s-option value="us" selected>US</s-option>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"Last checked: <time datetime="2026-11-09T14:56:46.123Z">2026-11-09 14:56:46 UTC</time>"#),
            "{html}"
        );
        // One provider: nothing to choose.
        assert!(!html.contains(r#"<form method="get""#), "{html}");
    }

    #[test]
    fn chooses_the_provider_with_a_form_that_changes_the_url() {
        let (first, second) = (
            warehouse(),
            Provider::new("erp<x>").field("token", Field::password("Token")),
        );
        let mut erp = item("erp<x>", &second);
        erp.fields = JsonMap::new();
        erp.config = JsonMap::new();
        erp.configured = false;
        erp.last_checked_at = None;
        let providers = [item("warehouse", &first), erp];
        let html = view(&providers, 1, &FormState::default()).into_string();
        assert!(
            html.contains(r#"<form method="get" data-openshop-autosubmit>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<s-select name="provider" label="Provider" value="erp&lt;x&gt;">"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<s-option value="warehouse">warehouse</s-option>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<s-option value="erp&lt;x&gt;" selected>erp&lt;x&gt;</s-option>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<s-badge tone="warning">Not configured</s-badge>"#),
            "{html}"
        );
        assert!(html.contains(r#"<s-password-field name="config.token" label="Token"></s-password-field>"#));
        assert!(!html.contains("Last checked"), "{html}");
        assert!(!html.contains("config.endpoint"), "{html}");
    }

    #[test]
    fn shows_a_refused_form_again_with_the_error_of_each_field() {
        let provider = warehouse();
        let providers = [item("warehouse", &provider)];
        let form = refused(
            &[
                ("_action", "save"),
                ("provider", "warehouse"),
                ("config.endpoint", "nope"),
                ("config.zone", "eu"),
            ],
            &[("config.endpoint", "Must be a URL"), ("config.apiKey", "Required")],
        );
        let html = view(&providers, 0, &form).into_string();
        assert!(
            html.contains(r#"<s-text-field name="config.endpoint" label="Endpoint" value="nope" error="Must be a URL"></s-text-field>"#),
            "{html}"
        );
        assert!(
            html.contains(
                r#"name="config.apiKey" label="API key" placeholder="Saved; leave blank to keep" error="Required">"#
            ),
            "{html}"
        );
        // A checkbox that was not sent was not checked.
        assert!(!html.contains(r#"label="Sandbox" checked"#), "{html}");
        assert!(
            html.contains(r#"<s-option value="eu" selected>Europe</s-option>"#),
            "{html}"
        );

        // The form of another provider is not shown on this one.
        let other = refused(
            &[("provider", "erp"), ("config.endpoint", "nope")],
            &[("config.endpoint", "Bad")],
        );
        let html = view(&providers, 0, &other).into_string();
        assert!(
            html.contains(r#"value="https://warehouse.test"></s-text-field>"#),
            "{html}"
        );
        assert!(!html.contains("Bad"), "{html}");
    }

    #[test]
    fn says_when_there_is_no_provider() {
        let html = view(&[], 0, &FormState::default()).into_string();
        assert!(html.contains("No providers registered."), "{html}");
        assert!(!html.contains("<form"), "{html}");
    }

    #[test]
    fn names_the_connection_of_a_provider() {
        assert_eq!(connection_status(false, Some(true)), ("warning", "Not configured"));
        assert_eq!(connection_status(true, None), ("info", "Configured"));
        assert_eq!(connection_status(true, Some(true)), ("success", "Connected"));
        assert_eq!(connection_status(true, Some(false)), ("critical", "Error"));
    }
}
