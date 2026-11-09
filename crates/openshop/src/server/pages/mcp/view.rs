//! The markup of `/mcp`: the URL of the server, the tokens, the secret
//! revealed once, and the modals that create a token, edit its permissions
//! and confirm its revocation.

use indexmap::IndexMap;
use maud::{Markup, html};
use serde_json::Value;

use super::super::ui;
use super::{CREATE, REVOKE, ROTATE, SET_PERMISSIONS, Secret, UPDATE};
use crate::admin::FormState;
use crate::server::mcp::registry::RegisteredMcpPermission;

const CREATE_MODAL_ID: &str = "create-mcp-token-modal";
const DEFAULT_EXPIRATION: &str = "90";
const EXPIRATIONS: [(&str, &str); 4] = [
    ("30", "30 days"),
    ("90", "90 days"),
    ("365", "1 year"),
    ("none", "No expiration"),
];
const SUBDUED: &str = "color:#6d7175";

/// Permissions by group, in the order of their groups' names.
type Groups<'a> = Vec<(&'a str, Vec<&'a RegisteredMcpPermission>)>;

fn text<'a>(item: &'a Value, key: &str) -> &'a str {
    item.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn strings<'a>(item: &'a Value, key: &str) -> Vec<&'a str> {
    item.get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect()
}

/// A date, or `Never`.
fn date(value: Option<&Value>) -> Markup {
    match value.and_then(Value::as_str).filter(|value| !value.is_empty()) {
        Some(iso) => ui::time(iso),
        None => html! { "Never" },
    }
}

fn token_tone(status: &str) -> &'static str {
    match status {
        "active" => "success",
        "revoked" => "critical",
        _ => "neutral",
    }
}

fn audit_tone(status: &str) -> &'static str {
    match status {
        "success" => "success",
        "denied" => "warning",
        _ => "critical",
    }
}

fn edit_modal_id(id: &str) -> String {
    format!("edit-mcp-token-{id}")
}

fn revoke_modal_id(id: &str) -> String {
    format!("revoke-mcp-token-{id}")
}

/// Permissions by group, groups in the order of their names, permissions in
/// the order they are declared.
fn permission_groups(permissions: &IndexMap<String, RegisteredMcpPermission>) -> Groups<'_> {
    let mut groups: Groups<'_> = Vec::new();
    for permission in permissions.values() {
        match groups.iter_mut().find(|(group, _)| *group == permission.group) {
            Some((_, members)) => members.push(permission),
            None => groups.push((&permission.group, vec![permission])),
        }
    }
    groups.sort_by(|(left, _), (right, _)| {
        left.to_lowercase()
            .cmp(&right.to_lowercase())
            .then_with(|| left.cmp(right))
    });
    groups
}

/// The page: `url`, where MCP clients reach the server; the `permissions` a
/// token may be granted; the `tokens` of the shop, as the API lists them,
/// with their recent audits; the `secret` an action revealed; the `form` the
/// page refused.
pub(super) fn view(
    url: &str,
    permissions: &IndexMap<String, RegisteredMcpPermission>,
    tokens: &[Value],
    secret: Option<Secret<'_>>,
    form: &FormState,
) -> Markup {
    let groups = permission_groups(permissions);
    html! {
        s-page heading="MCP" {
            s-button slot="primary-action" variant="primary" commandFor=(CREATE_MODAL_ID) command="--show" {
                "Create token"
            }
            @if let Some(secret) = secret {
                (secret_banner(secret))
            }
            s-section heading="MCP endpoint" accessibilityLabel="MCP endpoint" {
                s-grid gridTemplateColumns="@container (inline-size > 700px) 1fr auto, 1fr" gap="base" alignItems="center" {
                    s-box padding="base" background="subdued" borderRadius="base" borderWidth="base" borderColor="base" {
                        code style="word-break:break-all" { (url) }
                    }
                    s-button variant="secondary" data-openshop-copy=(url) { "Copy URL" }
                }
            }
            @if tokens.is_empty() {
                (empty_state())
            } @else {
                (token_table(tokens))
            }
        }
        (create_modal(&groups, form.of(CREATE)))
        @for token in tokens {
            (edit_modal(token, &groups, permissions, form.of(SET_PERMISSIONS)))
            @if text(token, "status") != "revoked" {
                (revoke_modal(token))
            }
        }
    }
}

/// The secret of a token, once: what happened, and the value to copy.
fn secret_banner(secret: Secret<'_>) -> Markup {
    html! {
        s-banner tone="success" { (secret.message) }
        s-banner tone="warning" heading="Copy this token now" {
            s-stack gap="small" {
                s-text { "This value is shown once. Store it before leaving this page." }
                s-box padding="base" background="subdued" borderRadius="base" borderWidth="base" borderColor="base" {
                    code style="word-break:break-all" { (secret.token) }
                }
                s-button variant="secondary" data-openshop-copy=(secret.token) { "Copy token" }
            }
        }
    }
}

fn empty_state() -> Markup {
    html! {
        s-section accessibilityLabel="MCP tokens empty state" {
            s-grid justifyItems="center" paddingBlock="large-300" {
                s-box maxInlineSize="420px" {
                    s-stack gap="base" alignItems="center" {
                        s-text type="strong" { "No MCP tokens yet" }
                        s-text color="subdued" {
                            "Create a scoped token to let an MCP client call OpenShop tools and resources."
                        }
                        s-button variant="primary" commandFor=(CREATE_MODAL_ID) command="--show" { "Create token" }
                    }
                }
            }
        }
    }
}

fn token_table(tokens: &[Value]) -> Markup {
    html! {
        s-section padding="none" accessibilityLabel="MCP tokens" {
            s-table {
                s-table-header-row {
                    s-table-header listSlot="primary" { "Name" }
                    s-table-header listSlot="inline" { "Status" }
                    s-table-header listSlot="labeled" { "Expires" }
                    s-table-header listSlot="labeled" { "Last used" }
                    s-table-header {}
                }
                s-table-body {
                    @for token in tokens {
                        (token_row(token))
                    }
                }
            }
        }
    }
}

fn token_metadata(token: &Value, font_size: &str) -> Markup {
    html! {
        span style=(format!("{SUBDUED};font-size:{font_size}")) {
            "Token ID " code { (text(token, "tokenId")) }
            " · Fingerprint " code { (text(token, "tokenFingerprint")) }
        }
    }
}

/// A hidden field of a form: the action it runs, the token it is about.
fn hidden(name: &str, value: &str) -> Markup {
    html! { input type="hidden" name=(name) value=(value); }
}

fn token_row(token: &Value) -> Markup {
    let id = text(token, "id");
    let status = text(token, "status");
    let revoked = status == "revoked";
    let toggle = match status {
        "active" => Some(("Disable", "disabled")),
        "disabled" => Some(("Enable", "active")),
        _ => None,
    };
    let stale = strings(token, "stalePermissions");
    html! {
        s-table-row {
            s-table-cell {
                s-text type="strong" { (text(token, "name")) }
                div { (token_metadata(token, "12px")) }
                @if !stale.is_empty() {
                    div style="color:#8a6116;font-size:12px" { "Stale: " (stale.join(", ")) }
                }
            }
            s-table-cell { s-badge tone=(token_tone(status)) { (status) } }
            s-table-cell { (date(token.get("expiresAt"))) }
            s-table-cell { (date(token.get("lastUsedAt"))) }
            s-table-cell {
                div style="display:flex;gap:6px;flex-wrap:wrap" {
                    s-button variant="secondary" commandFor=(edit_modal_id(id)) command="--show" { "Edit" }
                    @if let Some((label, next)) = toggle {
                        form method="post" {
                            (hidden("_action", UPDATE))
                            (hidden("id", id))
                            (hidden("status", next))
                            s-button type="submit" variant="secondary" { (label) }
                        }
                    }
                    form method="post" {
                        (hidden("_action", ROTATE))
                        (hidden("id", id))
                        s-button type="submit" variant="secondary" disabled[revoked] { "Rotate" }
                    }
                    s-button variant="secondary" tone="critical" commandFor=(revoke_modal_id(id)) command="--show" disabled[revoked] {
                        "Revoke"
                    }
                }
            }
        }
    }
}

/// The permissions as checkboxes of one field, `permissions`, by group:
/// those `checked` are.
fn permission_list(groups: &Groups<'_>, checked: &[&str], disabled: bool, max_height: &str) -> Markup {
    html! {
        div style=(format!("max-height:{max_height};overflow-y:auto;padding-right:4px")) {
            @for (group, permissions) in groups {
                div style="margin-top:12px" {
                    div style="font-weight:600;margin-bottom:6px" { (group) }
                    div style="display:grid;gap:6px" {
                        @for permission in permissions {
                            div style="display:grid;grid-template-columns:auto 1fr;gap:8px;align-items:start" {
                                s-checkbox
                                    name="permissions"
                                    value=(permission.key)
                                    label=(permission.label)
                                    labelAccessibilityVisibility="exclusive"
                                    checked[checked.contains(&permission.key.as_str())]
                                    disabled[disabled] {}
                                span {
                                    strong { (permission.label) }
                                    span style=(SUBDUED) { " — " (permission.key) }
                                    @if let Some(description) = permission.description.as_deref().filter(|description| !description.is_empty()) {
                                        div style=(format!("{SUBDUED};font-size:12px")) { (description) }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn create_modal(groups: &Groups<'_>, form: &FormState) -> Markup {
    let expiration = form.value_or("expiresInDays", DEFAULT_EXPIRATION);
    let checked: Vec<&str> = form.values("permissions").collect();
    html! {
        form method="post" {
            (hidden("_action", CREATE))
            s-modal id=(CREATE_MODAL_ID) heading="Create token" accessibilityLabel="Create MCP token" data-openshop-open[form.is_refused()] {
                s-stack direction="block" gap="base" {
                    @if let Some(error) = form.error("permissions") {
                        s-banner tone="critical" { (error) }
                    }
                    s-grid gridTemplateColumns="@container (inline-size > 700px) 2fr 1fr, 1fr" gap="base" {
                        s-text-field
                            name="name"
                            label="Name"
                            placeholder="Claude Desktop"
                            value=(form.value_or("name", ""))
                            error=[form.error("name")] {}
                        s-select name="expiresInDays" label="Expiration" value=(expiration) error=[form.error("expiresInDays")] {
                            @for (value, label) in EXPIRATIONS {
                                s-option value=(value) selected[value == expiration] { (label) }
                            }
                        }
                    }
                    s-box {
                        s-text type="strong" { "Permissions" }
                        (permission_list(groups, &checked, false, "55vh"))
                    }
                }
                s-button slot="primary-action" type="submit" variant="primary" { "Create token" }
                s-button slot="secondary-actions" variant="secondary" commandFor=(CREATE_MODAL_ID) command="--hide" {
                    "Cancel"
                }
            }
        }
    }
}

/// The edit modal of a token: its permissions, which a revoked token keeps,
/// and its recent audit. `form`, the form of the edit modals, refused for
/// this token or another one.
fn edit_modal(
    token: &Value,
    groups: &Groups<'_>,
    permissions: &IndexMap<String, RegisteredMcpPermission>,
    form: &FormState,
) -> Markup {
    let id = text(token, "id");
    let modal = edit_modal_id(id);
    let status = text(token, "status");
    let revoked = status == "revoked";
    let refused = (form.is_refused() && form.value("id") == Some(id)).then_some(form);
    // What was sent, else what the token has; only the permissions still declared.
    let checked: Vec<&str> = match refused {
        Some(form) => form.values("permissions").collect(),
        None => strings(token, "permissions"),
    }
    .into_iter()
    .filter(|key| permissions.contains_key(*key))
    .collect();
    let audits = token
        .get("recentAudits")
        .and_then(Value::as_array)
        .filter(|audits| !audits.is_empty());
    html! {
        form method="post" {
            (hidden("_action", SET_PERMISSIONS))
            (hidden("id", id))
            s-modal
                id=(modal)
                heading=(format!("Edit {}", text(token, "name")))
                accessibilityLabel="Edit MCP token"
                size="large"
                data-openshop-open[refused.is_some()]
            {
                s-stack direction="block" gap="base" {
                    @if let Some(error) = refused.and_then(|form| form.error("permissions")) {
                        s-banner tone="critical" { (error) }
                    }
                    s-box paddingBlockEnd="base" {
                        s-stack gap="small" {
                            s-stack direction="inline" gap="small" alignItems="center" {
                                s-badge tone=(token_tone(status)) { (status) }
                                (token_metadata(token, "14px"))
                            }
                            s-text color="subdued" {
                                "Expires " (date(token.get("expiresAt")))
                                " · Last used " (date(token.get("lastUsedAt")))
                            }
                        }
                    }
                    s-box {
                        s-stack direction="inline" justifyContent="space-between" alignItems="center" {
                            s-text type="strong" { "Permissions" }
                            s-text color="subdued" { (checked.len()) " of " (permissions.len()) " selected" }
                        }
                        (permission_list(groups, &checked, revoked, "45vh"))
                    }
                    @if let Some(audits) = audits {
                        (audit_table(audits))
                    }
                }
                s-button slot="primary-action" type="submit" variant="primary" disabled[revoked] { "Save permissions" }
                s-button slot="secondary-actions" variant="secondary" commandFor=(modal) command="--hide" { "Cancel" }
            }
        }
    }
}

fn audit_table(audits: &[Value]) -> Markup {
    html! {
        s-box paddingBlockStart="base" {
            s-text type="strong" { "Recent audit" }
            s-table {
                s-table-header-row {
                    s-table-header listSlot="primary" { "Capability" }
                    s-table-header listSlot="inline" { "Status" }
                    s-table-header listSlot="labeled" { "When" }
                }
                s-table-body {
                    @for audit in audits {
                        @let capability = Some(text(audit, "capabilityName")).filter(|name| !name.is_empty());
                        @let shop = text(audit, "targetShop");
                        s-table-row {
                            s-table-cell {
                                (capability.unwrap_or("auth"))
                                @if !shop.is_empty() {
                                    div style=(format!("{SUBDUED};font-size:12px")) { (shop) }
                                }
                            }
                            s-table-cell {
                                s-badge tone=(audit_tone(text(audit, "status"))) { (text(audit, "status")) }
                            }
                            s-table-cell { (date(audit.get("createdAt"))) }
                        }
                    }
                }
            }
        }
    }
}

/// The confirmation of a revocation, which cannot be undone.
fn revoke_modal(token: &Value) -> Markup {
    let id = text(token, "id");
    let modal = revoke_modal_id(id);
    html! {
        form method="post" {
            (hidden("_action", REVOKE))
            (hidden("id", id))
            s-modal id=(modal) heading=(format!("Revoke {}?", text(token, "name"))) accessibilityLabel="Revoke MCP token" {
                s-paragraph {
                    "MCP clients that use this token lose access at once. A revoked token cannot be used again."
                }
                s-button slot="primary-action" type="submit" variant="primary" tone="critical" { "Revoke token" }
                s-button slot="secondary-actions" variant="secondary" commandFor=(modal) command="--hide" { "Cancel" }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admin::parse_fields;
    use crate::server::mcp::registry::{McpSource, core_mcp_permissions};
    use crate::types::McpRiskLevel;
    use serde_json::json;

    fn permissions() -> IndexMap<String, RegisteredMcpPermission> {
        let mut permissions = core_mcp_permissions();
        permissions.insert(
            "demo:read_status".to_string(),
            RegisteredMcpPermission {
                label: "Read <demo> status".to_string(),
                description: None,
                group: "demo".to_string(),
                risk_level: McpRiskLevel::Medium,
                key: "demo:read_status".to_string(),
                source: McpSource::Custom,
            },
        );
        permissions
    }

    fn token(id: &str, name: &str, status: &str) -> Value {
        json!({
            "id": id, "tokenId": format!("mtk_{id}"), "name": name, "tokenFingerprint": "ab12",
            "status": status, "expiresAt": "2027-02-07T10:00:00.000Z", "lastUsedAt": null,
            "permissions": ["read_logs", "gone"], "stalePermissions": ["gone"], "recentAudits": [],
        })
    }

    fn render(tokens: &[Value], secret: Option<Secret<'_>>, form: &FormState) -> String {
        view("https://app.example.com/mcp", &permissions(), tokens, secret, form).into_string()
    }

    /// The part of `html` from `start` to the end of its form.
    fn form_of<'a>(html: &'a str, start: &str) -> &'a str {
        let from = html.find(start).unwrap_or_else(|| panic!("{start} in {html}"));
        let to = html[from..].find("</form>").unwrap();
        &html[from..from + to]
    }

    #[test]
    fn groups_permissions_and_sorts_the_groups() {
        let permissions = permissions();
        let groups: Vec<(&str, Vec<&str>)> = permission_groups(&permissions)
            .into_iter()
            .map(|(group, members)| (group, members.iter().map(|member| member.key.as_str()).collect()))
            .collect();
        assert_eq!(
            groups,
            [
                ("Automation", vec!["read_crons", "manage_crons"]),
                ("demo", vec!["demo:read_status"]),
                ("Flows", vec!["read_flows", "run_flows"]),
                ("Observability", vec!["read_logs"]),
                ("Shopify Admin", vec!["shopify_admin_graphql"]),
            ]
        );
    }

    #[test]
    fn renders_the_url_to_copy_and_the_empty_state() {
        let html = render(&[], None, &FormState::default());
        assert!(html.contains(r#"<s-page heading="MCP">"#), "{html}");
        assert!(
            html.contains(r#"<code style="word-break:break-all">https://app.example.com/mcp</code>"#),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<s-button variant="secondary" data-openshop-copy="https://app.example.com/mcp">Copy URL</s-button>"#
            ),
            "{html}"
        );
        assert!(html.contains("No MCP tokens yet"), "{html}");
        assert_eq!(
            html.matches(r#"commandFor="create-mcp-token-modal" command="--show""#)
                .count(),
            2
        );
        // No secret, and no modal of a token.
        assert!(!html.contains("Copy this token now"), "{html}");
        assert!(!html.contains("edit-mcp-token-"), "{html}");
    }

    #[test]
    fn renders_a_row_and_its_forms_per_token() {
        let mut active = token("a1", "Claude <Desktop>", "active");
        active["lastUsedAt"] = json!("2026-11-09T08:00:00.000Z");
        let tokens = [active, token("d2", "CI", "disabled"), token("r3", "Old", "revoked")];
        let html = render(&tokens, None, &FormState::default());

        assert!(
            html.contains(r#"<s-text type="strong">Claude &lt;Desktop&gt;</s-text>"#),
            "{html}"
        );
        assert!(
            html.contains("Token ID <code>mtk_a1</code> · Fingerprint <code>ab12</code>"),
            "{html}"
        );
        assert!(html.contains("Stale: gone"), "{html}");
        assert!(html.contains(r#"<s-badge tone="success">active</s-badge>"#), "{html}");
        assert!(html.contains(r#"<s-badge tone="neutral">disabled</s-badge>"#), "{html}");
        assert!(html.contains(r#"<s-badge tone="critical">revoked</s-badge>"#), "{html}");
        assert!(
            html.contains(r#"<time datetime="2027-02-07T10:00:00.000Z">2027-02-07 10:00:00 UTC</time>"#),
            "{html}"
        );
        assert!(html.contains("<s-table-cell>Never</s-table-cell>"), "{html}");
        assert!(html.contains(r#"<time datetime="2026-11-09T08:00:00.000Z">"#), "{html}");

        // Disable the active one, enable the disabled one; nothing for the revoked one.
        let disable = form_of(
            &html,
            r#"value="updateToken"><input type="hidden" name="id" value="a1">"#,
        );
        assert!(disable.contains(r#"name="status" value="disabled""#), "{disable}");
        assert!(disable.contains(">Disable</s-button>"), "{disable}");
        let enable = form_of(
            &html,
            r#"value="updateToken"><input type="hidden" name="id" value="d2">"#,
        );
        assert!(enable.contains(r#"name="status" value="active""#), "{enable}");
        assert!(
            !html.contains(r#"value="updateToken"><input type="hidden" name="id" value="r3">"#),
            "{html}"
        );

        // Rotate and revoke, which a revoked token cannot.
        let rotate = form_of(
            &html,
            r#"value="rotateToken"><input type="hidden" name="id" value="a1">"#,
        );
        assert!(
            rotate.contains(r#"<s-button type="submit" variant="secondary">Rotate</s-button>"#),
            "{rotate}"
        );
        let rotate_revoked = form_of(
            &html,
            r#"value="rotateToken"><input type="hidden" name="id" value="r3">"#,
        );
        assert!(
            rotate_revoked.contains(r#"variant="secondary" disabled>Rotate"#),
            "{rotate_revoked}"
        );
        assert!(
            html.contains(r#"commandFor="revoke-mcp-token-a1" command="--show">Revoke"#),
            "{html}"
        );
        assert!(
            html.contains(r#"commandFor="revoke-mcp-token-r3" command="--show" disabled>Revoke"#),
            "{html}"
        );

        // The revocation is confirmed in a modal, its form posts the token.
        let revoke = form_of(
            &html,
            r#"value="revokeToken"><input type="hidden" name="id" value="a1">"#,
        );
        assert!(
            revoke.contains(r#"<s-modal id="revoke-mcp-token-a1" heading="Revoke Claude &lt;Desktop&gt;?""#),
            "{revoke}"
        );
        assert!(
            revoke.contains(r#"<s-button slot="primary-action" type="submit" variant="primary" tone="critical">Revoke token</s-button>"#),
            "{revoke}"
        );
        assert!(!html.contains(r#"id="revoke-mcp-token-r3""#), "{html}");

        // Every form posts to this page, at a path that is not the MCP endpoint: create,
        // disable and enable, three rotations, three edit modals, two revocations.
        assert_eq!(html.matches(r#"<form method="post">"#).count(), 11, "{html}");
        assert_eq!(html.matches("<form").count(), 11, "{html}");
    }

    #[test]
    fn the_edit_modal_of_a_token_saves_its_permissions() {
        let mut active = token("a1", "Claude", "active");
        active["recentAudits"] = json!([
            { "capabilityName": "openshop.logs.search", "targetShop": "a.myshopify.com", "status": "success",
              "createdAt": "2026-11-09T09:00:00.000Z" },
            { "capabilityName": null, "targetShop": null, "status": "denied", "createdAt": "2026-11-09T08:00:00.000Z" },
        ]);
        let tokens = [active, token("r3", "Old", "revoked")];
        let html = render(&tokens, None, &FormState::default());

        let edit = form_of(
            &html,
            r#"value="setPermissions"><input type="hidden" name="id" value="a1">"#,
        );
        assert!(
            edit.contains(r#"<s-modal id="edit-mcp-token-a1" heading="Edit Claude" accessibilityLabel="Edit MCP token" size="large">"#),
            "{edit}"
        );
        // What the token has and is still declared, checked; the rest not.
        assert!(
            edit.contains(r#"<s-checkbox name="permissions" value="read_logs" label="Read logs" labelAccessibilityVisibility="exclusive" checked>"#),
            "{edit}"
        );
        assert!(
            edit.contains(r#"value="run_flows" label="Run flows" labelAccessibilityVisibility="exclusive">"#),
            "{edit}"
        );
        assert!(!edit.contains(r#"value="gone""#), "{edit}");
        assert!(edit.contains("1 of 7 selected"), "{edit}");
        assert!(edit.contains(r#"label="Read &lt;demo&gt; status""#), "{edit}");
        assert!(edit.contains("Trigger OpenShop flow runs."), "{edit}");
        assert!(
            edit.contains(
                r#"<s-button slot="primary-action" type="submit" variant="primary">Save permissions</s-button>"#
            ),
            "{edit}"
        );
        // Its recent audit.
        assert!(edit.contains("Recent audit"), "{edit}");
        assert!(edit.contains("openshop.logs.search<div"), "{edit}");
        assert!(edit.contains("a.myshopify.com"), "{edit}");
        assert!(edit.contains(r#"<s-badge tone="success">success</s-badge>"#), "{edit}");
        assert!(edit.contains(r#"<s-table-cell>auth</s-table-cell>"#), "{edit}");
        assert!(edit.contains(r#"<s-badge tone="warning">denied</s-badge>"#), "{edit}");
        assert!(!edit.contains("data-openshop-open"), "{edit}");

        // A revoked token shows them and cannot change them.
        let revoked = form_of(
            &html,
            r#"value="setPermissions"><input type="hidden" name="id" value="r3">"#,
        );
        assert!(
            revoked.contains(r#"labelAccessibilityVisibility="exclusive" checked disabled>"#),
            "{revoked}"
        );
        assert!(
            revoked.contains(r#"variant="primary" disabled>Save permissions"#),
            "{revoked}"
        );
        assert!(!revoked.contains("Recent audit"), "{revoked}");
    }

    #[test]
    fn a_refused_form_opens_its_modal_again_with_its_errors() {
        let tokens = [token("a1", "Claude", "active"), token("b2", "CI", "active")];

        let create = FormState {
            action: Some(CREATE.to_string()),
            values: parse_fields(b"_action=createToken&name=&expiresInDays=365&permissions=run_flows"),
            errors: IndexMap::from([("name".to_string(), "Token name is required.".to_string())]),
            message: None,
        };
        let html = render(&tokens, None, &create);
        let modal = form_of(&html, r#"value="createToken""#);
        assert!(
            modal.contains(r#"<s-modal id="create-mcp-token-modal" heading="Create token" accessibilityLabel="Create MCP token" data-openshop-open>"#),
            "{modal}"
        );
        assert!(
            modal.contains(r#"<s-text-field name="name" label="Name" placeholder="Claude Desktop" value="" error="Token name is required."></s-text-field>"#),
            "{modal}"
        );
        assert!(
            modal.contains(r#"<s-option value="365" selected>1 year</s-option>"#),
            "{modal}"
        );
        assert!(
            modal.contains(r#"value="run_flows" label="Run flows" labelAccessibilityVisibility="exclusive" checked>"#),
            "{modal}"
        );
        assert!(
            !modal.contains(r#"value="read_logs" label="Read logs" labelAccessibilityVisibility="exclusive" checked>"#),
            "{modal}"
        );
        // The other modals stay closed.
        assert_eq!(html.matches("data-openshop-open").count(), 1, "{html}");

        // Not refused: 90 days, nothing checked.
        let fresh = render(&tokens, None, &FormState::default());
        let modal = form_of(&fresh, r#"value="createToken""#);
        assert!(
            modal.contains(r#"<s-select name="expiresInDays" label="Expiration" value="90">"#),
            "{modal}"
        );
        assert!(
            modal.contains(r#"<s-option value="none">No expiration</s-option>"#),
            "{modal}"
        );
        assert!(!modal.contains(" checked"), "{modal}");

        let edit = FormState {
            action: Some(SET_PERMISSIONS.to_string()),
            values: parse_fields(b"_action=setPermissions&id=b2&permissions=read_crons&permissions=nope"),
            errors: IndexMap::from([("permissions".to_string(), "Unknown permission \"nope\"".to_string())]),
            message: None,
        };
        let html = render(&tokens, None, &edit);
        let refused = form_of(
            &html,
            r#"value="setPermissions"><input type="hidden" name="id" value="b2">"#,
        );
        assert!(refused.contains(r#"size="large" data-openshop-open>"#), "{refused}");
        assert!(
            refused.contains(r#"<s-banner tone="critical">Unknown permission &quot;nope&quot;</s-banner>"#),
            "{refused}"
        );
        assert!(
            refused
                .contains(r#"value="read_crons" label="Read crons" labelAccessibilityVisibility="exclusive" checked>"#),
            "{refused}"
        );
        assert!(
            !refused
                .contains(r#"value="read_logs" label="Read logs" labelAccessibilityVisibility="exclusive" checked>"#),
            "{refused}"
        );
        let other = form_of(
            &html,
            r#"value="setPermissions"><input type="hidden" name="id" value="a1">"#,
        );
        assert!(!other.contains("data-openshop-open"), "{other}");
        assert!(
            other.contains(r#"value="read_logs" label="Read logs" labelAccessibilityVisibility="exclusive" checked>"#),
            "{other}"
        );
    }

    #[test]
    fn shows_a_secret_only_when_an_action_revealed_it() {
        let secret = Secret {
            token: "oshp_mcp_mtk_x.s3cr<e>t",
            message: "Token rotated. Copy the new value now.",
        };
        let html = render(&[token("a1", "Claude", "active")], Some(secret), &FormState::default());
        assert!(
            html.contains(r#"<s-banner tone="success">Token rotated. Copy the new value now.</s-banner>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<s-banner tone="warning" heading="Copy this token now">"#),
            "{html}"
        );
        assert!(
            html.contains("This value is shown once. Store it before leaving this page."),
            "{html}"
        );
        assert!(
            html.contains(r#"<code style="word-break:break-all">oshp_mcp_mtk_x.s3cr&lt;e&gt;t</code>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<s-button variant="secondary" data-openshop-copy="oshp_mcp_mtk_x.s3cr&lt;e&gt;t">Copy token</s-button>"#),
            "{html}"
        );
        // Before the URL of the server.
        assert!(html.find("Copy this token now").unwrap() < html.find("MCP endpoint").unwrap());
        let again = render(&[token("a1", "Claude", "active")], None, &FormState::default());
        assert!(!again.contains("s3cr"), "{again}");
    }
}
