//! `/crons`: the crons of the shop, each enabled or disabled by a form.

use anyhow::Result;
use maud::{Markup, html};
use serde_json::Value;

use crate::admin::{AdminPublicError, Outcome};
use crate::app::OpenShop;
use crate::server::mcp::core::{cron_items, set_cron_enabled};
use crate::server::shop::ShopSession;

/// The action of the form of a row.
const TOGGLE: &str = "toggle";

fn text<'a>(item: &'a Value, key: &str) -> &'a str {
    item.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn shops_label(shops: Option<&Value>) -> String {
    match shops {
        Some(Value::Array(shops)) => shops.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "),
        Some(Value::String(shop)) => shop.clone(),
        _ => "global".to_string(),
    }
}

fn is_truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(value)) => *value,
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Object(object)) => !object.is_empty(),
        Some(Value::Array(items)) => !items.is_empty(),
        Some(Value::Number(_)) => true,
    }
}

pub(super) async fn render(app: &OpenShop, session: &ShopSession) -> Result<Markup> {
    let crons = cron_items(app, &session.shopify_app, &session.shop).await?;
    Ok(view(&crons))
}

fn view(crons: &[Value]) -> Markup {
    html! {
        s-page heading="Crons" {
            s-section padding="none" {
                s-table {
                    s-table-header-row {
                        s-table-header listSlot="primary" { "Name" }
                        s-table-header listSlot="secondary" { "Schedule" }
                        s-table-header listSlot="labeled" { "Shops" }
                        s-table-header listSlot="inline" { "Status" }
                        s-table-header {}
                    }
                    s-table-body {
                        @if crons.is_empty() {
                            s-table-row {
                                s-table-cell { s-text color="subdued" { "No crons configured" } }
                                s-table-cell {} s-table-cell {} s-table-cell {} s-table-cell {}
                            }
                        }
                        @for (index, cron) in crons.iter().enumerate() {
                            (row(index, cron))
                        }
                    }
                }
            }
            @for (index, cron) in crons.iter().enumerate().filter(|(_, cron)| is_truthy(cron.get("input"))) {
                s-modal id=(format!("cron-input-{index}")) heading=(format!("Input — {}", name(cron))) {
                    pre style="font-family:ui-monospace,SFMono-Regular,Menlo,monospace;font-size:12px;white-space:pre-wrap;margin:0" {
                        (serde_json::to_string_pretty(cron.get("input").unwrap_or(&Value::Null)).unwrap_or_default())
                    }
                }
            }
        }
    }
}

fn name(cron: &Value) -> &str {
    Some(text(cron, "name")).filter(|name| !name.is_empty()).unwrap_or_else(|| text(cron, "flow"))
}

fn row(index: usize, cron: &Value) -> Markup {
    let enabled = cron.get("enabled").and_then(Value::as_bool).unwrap_or(false);
    let named = !text(cron, "name").is_empty();
    html! {
        s-table-row {
            s-table-cell {
                strong { (name(cron)) }
                @if named { div style="font-size:12px;color:#8c9196" { (text(cron, "flow")) } }
            }
            s-table-cell {
                code style="font-size:13px;background:#f1f1f1;padding:2px 6px;border-radius:4px" { (text(cron, "schedule")) }
            }
            s-table-cell { (shops_label(cron.get("shops"))) }
            s-table-cell {
                s-badge tone=(if enabled { "success" } else { "neutral" }) {
                    (if enabled { "Enabled" } else { "Disabled" })
                }
            }
            s-table-cell {
                s-stack direction="inline" gap="small-200" {
                    @if is_truthy(cron.get("input")) {
                        s-button variant="secondary" commandFor=(format!("cron-input-{index}")) command="--show" { "Input" }
                    }
                    form method="post" {
                        input type="hidden" name="_action" value=(TOGGLE);
                        input type="hidden" name="key" value=(text(cron, "key"));
                        input type="hidden" name="enabled" value=(!enabled);
                        s-button type="submit" variant=(if enabled { "secondary" } else { "primary" }) {
                            (if enabled { "Disable" } else { "Enable" })
                        }
                    }
                }
            }
        }
    }
}

/// The form of a row: the cron enabled or disabled, said in a toast.
pub(super) async fn submit(
    app: &OpenShop,
    session: &ShopSession,
    action: &str,
    fields: &[(String, String)],
) -> Result<Outcome> {
    let field = |name: &str| {
        fields
            .iter()
            .find(|(field, _)| field == name)
            .map(|(_, value)| value.as_str())
    };
    let (TOGGLE, Some(key), Some(enabled)) = (action, field("key").filter(|key| !key.is_empty()), field("enabled"))
    else {
        return Err(AdminPublicError::new("INVALID_INPUT", "key and enabled are required").into());
    };
    let enabled = enabled == "true";
    if !set_cron_enabled(app, &session.shopify_app, &session.shop, key, enabled).await? {
        return Err(AdminPublicError::new("NOT_FOUND", "Cron not found").status(404).into());
    }
    Ok(Outcome::toast(if enabled { "Cron enabled" } else { "Cron disabled" }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn renders_a_form_per_cron() {
        let crons = [
            json!({ "key": "a", "flow": "syncOrders", "name": "Quick sync", "schedule": "*/5 * * * *",
                    "input": { "limit": 10 }, "shops": null, "enabled": true }),
            json!({ "key": "b<x>", "flow": "other", "name": null, "schedule": "0 * * * *",
                    "input": null, "shops": ["a.myshopify.com", "b.myshopify.com"], "enabled": false }),
        ];
        let html = view(&crons).into_string();
        assert!(html.contains(r#"<input type="hidden" name="key" value="a">"#), "{html}");
        assert!(html.contains(r#"<input type="hidden" name="enabled" value="false">"#), "{html}");
        assert!(html.contains(r#"<input type="hidden" name="key" value="b&lt;x&gt;">"#), "{html}");
        assert!(html.contains("a.myshopify.com, b.myshopify.com"), "{html}");
        assert!(html.contains(r#"<s-modal id="cron-input-0" heading="Input — Quick sync">"#), "{html}");
        assert!(!html.contains("cron-input-1"), "{html}");
        assert!(view(&[]).into_string().contains("No crons configured"));
    }
}
