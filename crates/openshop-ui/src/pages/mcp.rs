use crate::{
    components::page::Page,
    components::table_header::TableHeader,
    dom::{self, hide_overlay, locale_compare, locale_date_time},
    events::{event_checked, event_value},
    fetch::{ApiError, ApiResponse, api_json, api_request},
    http::Method,
    shopify,
    types::{BannerMessage, McpAuditLog, McpCapabilitySummary, McpPermissionSummary, McpTokenSummary},
};
use futures_util::future::try_join;
use indexmap::IndexMap;
use leptos::{prelude::*, task::spawn_local};
use serde::Deserialize;
use serde_json::{Value, json};
use std::cmp::Ordering;

const CREATE_MODAL_ID: &str = "create-mcp-token-modal";
const EDIT_MODAL_ID: &str = "edit-mcp-token-modal";
const DEFAULT_EXPIRATION: &str = "90";

type PermissionGroup = (String, Vec<McpPermissionSummary>);

fn format_date(value: Option<&str>) -> String {
    match value.filter(|value| !value.is_empty()) {
        Some(value) => locale_date_time(value),
        None => "Never".to_string(),
    }
}

/// Permissions by group, groups ordered with `compare`, permissions in declaration order.
fn permission_groups(
    permissions: &IndexMap<String, McpPermissionSummary>,
    compare: impl Fn(&str, &str) -> Ordering,
) -> Vec<PermissionGroup> {
    let mut groups: Vec<PermissionGroup> = Vec::new();
    for permission in permissions.values() {
        match groups.iter_mut().find(|(group, _)| *group == permission.group) {
            Some((_, members)) => members.push(permission.clone()),
            None => groups.push((permission.group.clone(), vec![permission.clone()])),
        }
    }
    groups.sort_by(|(left, _), (right, _)| compare(left, right));
    groups
}

fn expiration_payload(choice: &str) -> Value {
    choice.parse::<u32>().map_or(Value::Null, Value::from)
}

fn create_token_body(name: &str, expiration: &str, permissions: &[String]) -> Value {
    json!({
        "name": name,
        "expiresInDays": expiration_payload(expiration),
        "permissions": permissions,
    })
}

fn toggle_set(current: &mut Vec<String>, key: &str, enabled: bool) {
    let position = current.iter().position(|entry| entry == key);
    match (position, enabled) {
        (None, true) => current.push(key.to_string()),
        (Some(index), false) => {
            current.remove(index);
        }
        _ => {}
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

#[derive(Deserialize)]
struct CreatedItem {
    id: String,
}

#[derive(Deserialize)]
struct IssuedToken {
    token: String,
    #[serde(default)]
    item: Option<CreatedItem>,
}

async fn mutate(method: Method, url: &str, body: Option<&Value>, fallback: &str) -> Result<ApiResponse, ApiError> {
    let response = api_request(method, url, body).await?;
    if response.ok {
        Ok(response)
    } else {
        Err(ApiError(response.error().unwrap_or_else(|| fallback.to_string())))
    }
}

#[derive(Clone, Copy)]
struct McpState {
    capabilities: RwSignal<Option<McpCapabilitySummary>>,
    tokens: RwSignal<Vec<McpTokenSummary>>,
    selected_id: RwSignal<Option<String>>,
    selected_detail: RwSignal<Option<McpTokenSummary>>,
    selected_permissions: RwSignal<Vec<String>>,
    new_name: RwSignal<String>,
    new_expiration: RwSignal<String>,
    new_permissions: RwSignal<Vec<String>>,
    one_time_token: RwSignal<Option<String>>,
    busy: RwSignal<Option<String>>,
    message: RwSignal<Option<BannerMessage>>,
    create_error: RwSignal<Option<String>>,
    edit_error: RwSignal<Option<String>>,
}

impl McpState {
    fn new() -> Self {
        Self {
            capabilities: RwSignal::new(None),
            tokens: RwSignal::new(Vec::new()),
            selected_id: RwSignal::new(None),
            selected_detail: RwSignal::new(None),
            selected_permissions: RwSignal::new(Vec::new()),
            new_name: RwSignal::new(String::new()),
            new_expiration: RwSignal::new(DEFAULT_EXPIRATION.to_string()),
            new_permissions: RwSignal::new(Vec::new()),
            one_time_token: RwSignal::new(None),
            busy: RwSignal::new(None),
            message: RwSignal::new(None),
            create_error: RwSignal::new(None),
            edit_error: RwSignal::new(None),
        }
    }

    fn fail(self, error: ApiError) {
        self.message.set(Some(BannerMessage::critical(error.to_string())));
    }

    /// Token permissions that are still declared; with `keep_all_when_unloaded`, everything is
    /// kept until the capabilities are known.
    fn known_permissions(self, permissions: &[String], keep_all_when_unloaded: bool) -> Vec<String> {
        self.capabilities
            .try_with_untracked(|capabilities| match capabilities {
                Some(capabilities) => permissions
                    .iter()
                    .filter(|permission| capabilities.permissions.contains_key(*permission))
                    .cloned()
                    .collect(),
                None if keep_all_when_unloaded => permissions.to_vec(),
                None => Vec::new(),
            })
            .unwrap_or_default()
    }

    async fn load_capabilities(self) -> Result<(), ApiError> {
        let data = api_json::<McpCapabilitySummary>("/api/mcp/capabilities").await?;
        self.capabilities.set(Some(data));
        Ok(())
    }

    async fn load_tokens(self) -> Result<(), ApiError> {
        let data = api_json::<Vec<McpTokenSummary>>("/api/mcp/tokens").await?;
        if self.selected_id.try_with_untracked(Option::is_none) == Some(true)
            && let Some(first) = data.first()
        {
            self.selected_id.set(Some(first.id.clone()));
        }
        self.tokens.set(data);
        Ok(())
    }

    async fn load_selected_detail(self, id: Option<String>) -> Result<(), ApiError> {
        let Some(id) = id else {
            self.selected_detail.set(None);
            self.selected_permissions.set(Vec::new());
            return Ok(());
        };
        let data = api_json::<McpTokenSummary>(&format!("/api/mcp/tokens/{id}")).await?;
        self.selected_permissions
            .set(self.known_permissions(&data.permissions, false));
        self.selected_detail.set(Some(data));
        Ok(())
    }

    async fn copy_text(self, value: String, label: &'static str) {
        if dom::copy_text(&value).await.is_ok() {
            let text = format!("{label} copied.");
            if !shopify::toast(&text) {
                self.message.set(Some(BannerMessage::success(text)));
            }
        } else {
            let text = format!("Unable to copy {}.", label.to_lowercase());
            self.message.set(Some(BannerMessage::critical(text.clone())));
            shopify::toast_error(&text);
        }
    }

    fn open_edit_token(self, token: &McpTokenSummary) {
        self.selected_id.set(Some(token.id.clone()));
        self.selected_detail.set(None);
        self.selected_permissions
            .set(self.known_permissions(&token.permissions, true));
        self.edit_error.set(None);
    }

    async fn create_token(self) {
        let name = self.new_name.get_untracked();
        if name.trim().is_empty() {
            self.create_error.set(Some("Token name is required.".to_string()));
            return;
        }
        let body = create_token_body(
            &name,
            &self.new_expiration.get_untracked(),
            &self.new_permissions.get_untracked(),
        );
        self.busy.set(Some("create".to_string()));
        self.message.set(None);
        self.create_error.set(None);
        let created = async {
            let response = mutate(Method::Post, "/api/mcp/tokens", Some(&body), "Unable to create token").await?;
            let issued = response.json::<IssuedToken>()?;
            self.one_time_token.set(Some(issued.token));
            self.new_name.set(String::new());
            self.new_expiration.set(DEFAULT_EXPIRATION.to_string());
            self.new_permissions.set(Vec::new());
            self.selected_id.set(issued.item.map(|item| item.id));
            self.load_tokens().await?;
            hide_overlay(CREATE_MODAL_ID);
            shopify::toast("Token created");
            let text = "Token created. Copy it now; it will not be shown again.";
            self.message.set(Some(BannerMessage::success(text)));
            Ok::<(), ApiError>(())
        };
        if let Err(error) = created.await {
            self.create_error.set(Some(error.to_string()));
        }
        self.busy.set(None);
    }

    async fn save_permissions(self) {
        let Some(id) = self.selected_id.get_untracked() else {
            return;
        };
        if !self
            .tokens
            .with_untracked(|tokens| tokens.iter().any(|token| token.id == id))
        {
            return;
        }
        let body = json!({ "permissions": self.selected_permissions.get_untracked() });
        self.busy.set(Some("permissions".to_string()));
        self.message.set(None);
        self.edit_error.set(None);
        let saved = async {
            let url = format!("/api/mcp/tokens/{id}/permissions");
            mutate(Method::Put, &url, Some(&body), "Unable to save permissions").await?;
            self.load_tokens().await?;
            self.load_selected_detail(Some(id.clone())).await?;
            hide_overlay(EDIT_MODAL_ID);
            shopify::toast("Permissions saved");
            self.message.set(Some(BannerMessage::success("Permissions saved.")));
            Ok::<(), ApiError>(())
        };
        if let Err(error) = saved.await {
            let text = error.to_string();
            self.edit_error.set(Some(text.clone()));
            shopify::toast_error(&text);
        }
        self.busy.set(None);
    }

    async fn patch_token(self, id: String, status: &'static str) {
        self.busy.set(Some(id.clone()));
        let patched = async {
            let body = json!({ "status": status });
            mutate(
                Method::Patch,
                &format!("/api/mcp/tokens/{id}"),
                Some(&body),
                "Unable to update token",
            )
            .await?;
            self.load_tokens().await
        };
        if let Err(error) = patched.await {
            self.fail(error);
        }
        self.busy.set(None);
    }

    async fn revoke_token(self, id: String) {
        self.busy.set(Some(id.clone()));
        let revoked = async {
            mutate(
                Method::Post,
                &format!("/api/mcp/tokens/{id}/revoke"),
                None,
                "Unable to revoke token",
            )
            .await?;
            self.load_tokens().await?;
            self.load_selected_detail(Some(id.clone())).await
        };
        if let Err(error) = revoked.await {
            self.fail(error);
        }
        self.busy.set(None);
    }

    async fn rotate_token(self, id: String) {
        self.busy.set(Some(id.clone()));
        self.message.set(None);
        let rotated = async {
            let url = format!("/api/mcp/tokens/{id}/rotate");
            let response = mutate(Method::Post, &url, None, "Unable to rotate token").await?;
            self.one_time_token.set(Some(response.json::<IssuedToken>()?.token));
            self.load_tokens().await?;
            self.load_selected_detail(Some(id.clone())).await?;
            shopify::toast("Token rotated");
            self.message
                .set(Some(BannerMessage::success("Token rotated. Copy the new value now.")));
            Ok::<(), ApiError>(())
        };
        if let Err(error) = rotated.await {
            self.fail(error);
        }
        self.busy.set(None);
    }
}

fn token_metadata(token_id: String, fingerprint: String, font_size: &'static str) -> impl IntoView {
    view! {
        <span style=format!("color:#6d7175;font-size:{font_size}")>
            "Token ID " <code>{token_id}</code> " · Fingerprint " <code>{fingerprint}</code>
        </span>
    }
}

#[component]
fn PermissionList(
    groups: Memo<Vec<PermissionGroup>>,
    selection: RwSignal<Vec<String>>,
    #[prop(optional, into)] disabled: Signal<bool>,
    max_height: &'static str,
) -> impl IntoView {
    let permission_row = move |permission: McpPermissionSummary| {
        let (checked_key, toggled_key) = (permission.key.clone(), permission.key.clone());
        view! {
            <div style="display:grid;grid-template-columns:auto 1fr;gap:8px;align-items:start">
                <s-checkbox
                    label=permission.label.clone()
                    labelAccessibilityVisibility="exclusive"
                    prop:checked=move || selection.with(|selection| selection.contains(&checked_key))
                    on:change=move |event| {
                        selection.update(|selection| toggle_set(selection, &toggled_key, event_checked(&event)))
                    }
                    disabled=move || disabled.get()
                />
                <span>
                    <strong>{permission.label}</strong>
                    <span style="color:#6d7175">" — " {permission.key}</span>
                    {permission
                        .description
                        .filter(|description| !description.is_empty())
                        .map(|description| view! { <div style="color:#6d7175;font-size:12px">{description}</div> })}
                </span>
            </div>
        }
    };

    view! {
        <div style=format!("max-height:{max_height};overflow-y:auto;padding-right:4px")>
            {move || {
                groups
                    .get()
                    .into_iter()
                    .map(|(group, permissions)| {
                        view! {
                            <div style="margin-top:12px">
                                <div style="font-weight:600;margin-bottom:6px">{group}</div>
                                <div style="display:grid;gap:6px">
                                    {permissions.into_iter().map(permission_row).collect_view()}
                                </div>
                            </div>
                        }
                    })
                    .collect_view()
            }}
        </div>
    }
}

fn audit_table(audits: Vec<McpAuditLog>) -> impl IntoView {
    view! {
        <s-box paddingBlockStart="base">
            <s-text type="strong">"Recent audit"</s-text>
            <s-table>
                <s-table-header-row>
                    <TableHeader list_slot="primary">"Capability"</TableHeader>
                    <TableHeader list_slot="inline">"Status"</TableHeader>
                    <TableHeader list_slot="labeled">"When"</TableHeader>
                </s-table-header-row>
                <s-table-body>
                    {audits
                        .into_iter()
                        .map(|audit| {
                            view! {
                                <s-table-row>
                                    <s-table-cell>
                                        {audit.capability_name.unwrap_or_else(|| "auth".to_string())}
                                        {audit
                                            .target_shop
                                            .filter(|shop| !shop.is_empty())
                                            .map(|shop| view! { <div style="color:#6d7175;font-size:12px">{shop}</div> })}
                                    </s-table-cell>
                                    <s-table-cell>
                                        <s-badge tone=audit_tone(&audit.status)>{audit.status.clone()}</s-badge>
                                    </s-table-cell>
                                    <s-table-cell>{format_date(audit.created_at.as_deref())}</s-table-cell>
                                </s-table-row>
                            }
                        })
                        .collect_view()}
                </s-table-body>
            </s-table>
        </s-box>
    }
}

#[component]
pub fn Mcp() -> impl IntoView {
    let state = McpState::new();
    let McpState {
        capabilities,
        tokens,
        selected_id,
        selected_detail,
        selected_permissions,
        new_name,
        new_expiration,
        new_permissions,
        one_time_token,
        busy,
        message,
        create_error,
        edit_error,
    } = state;
    let mcp_url = StoredValue::new(format!("{}/mcp", dom::location_origin()));

    let groups = Memo::new(move |_| {
        capabilities.with(|capabilities| {
            capabilities
                .as_ref()
                .map(|capabilities| permission_groups(&capabilities.permissions, locale_compare))
                .unwrap_or_default()
        })
    });
    let permission_keys = Memo::new(move |_| {
        groups.with(|groups| {
            groups
                .iter()
                .flat_map(|(_, permissions)| permissions.iter().map(|permission| permission.key.clone()))
                .collect::<Vec<_>>()
        })
    });
    let selected = Memo::new(move |_| {
        let id = selected_id.get()?;
        tokens.with(|tokens| tokens.iter().find(|token| token.id == id).cloned())
    });
    let all_selected = move |selection: RwSignal<Vec<String>>| {
        permission_keys
            .with(|keys| !keys.is_empty() && selection.with(|selection| keys.iter().all(|key| selection.contains(key))))
    };
    let no_permissions = move || permission_keys.with(Vec::is_empty);
    let is_busy = move |key: &str| busy.with(|busy| busy.as_deref() == Some(key));
    let selected_revoked =
        Signal::derive(move || selected.with(|token| token.as_ref().is_some_and(|token| token.status == "revoked")));

    spawn_local(async move {
        if let Err(error) = try_join(state.load_capabilities(), state.load_tokens()).await {
            state.fail(error);
        }
    });

    Effect::new(move |_| {
        let id = selected_id.get();
        capabilities.track();
        spawn_local(async move {
            if let Err(error) = state.load_selected_detail(id).await {
                state.fail(error);
            }
        });
    });

    let copy = move |value: String, label: &'static str| spawn_local(state.copy_text(value, label));

    let create_modal = view! {
        <s-modal id=CREATE_MODAL_ID heading="Create token" accessibility-label="Create MCP token">
            <s-stack direction="block" gap="base">
                {move || create_error.get().map(|error| view! { <s-banner tone="critical">{error}</s-banner> })}
                <s-grid gridTemplateColumns="@container (inline-size > 700px) 2fr 1fr, 1fr" gap="base">
                    <s-text-field
                        label="Name"
                        prop:value=move || new_name.get()
                        on:input=move |event| new_name.set(event_value(&event))
                        placeholder="Claude Desktop"
                    />
                    <s-select
                        label="Expiration"
                        prop:value=move || new_expiration.get()
                        on:change=move |event| new_expiration.set(event_value(&event))
                    >
                        <s-option value="30">"30 days"</s-option>
                        <s-option value="90">"90 days"</s-option>
                        <s-option value="365">"1 year"</s-option>
                        <s-option value="none">"No expiration"</s-option>
                    </s-select>
                </s-grid>

                <s-box>
                    <s-stack direction="inline" justifyContent="space-between" alignItems="center">
                        <s-text type="strong">"Permissions"</s-text>
                        <span style="display:flex;gap:6px;flex-wrap:wrap">
                            <s-button
                                variant="secondary"
                                on:click=move |_| new_permissions.set(permission_keys.get_untracked())
                                disabled=move || no_permissions() || all_selected(new_permissions)
                            >
                                "Select all"
                            </s-button>
                            <s-button
                                variant="secondary"
                                on:click=move |_| new_permissions.set(Vec::new())
                                disabled=move || new_permissions.with(Vec::is_empty)
                            >
                                "Clear all"
                            </s-button>
                        </span>
                    </s-stack>
                    <PermissionList groups selection=new_permissions max_height="55vh" />
                </s-box>
            </s-stack>
            <s-button
                slot="primary-action"
                variant="primary"
                on:click=move |_| spawn_local(state.create_token())
                disabled=move || is_busy("create")
            >
                {move || if is_busy("create") { "Creating..." } else { "Create token" }}
            </s-button>
            <s-button slot="secondary-actions" variant="secondary" commandFor=CREATE_MODAL_ID command="--hide">
                "Cancel"
            </s-button>
        </s-modal>
    };

    let edit_body = move || {
        let Some(token) = selected.get() else {
            return view! { <s-text color="subdued">"Select a token to edit."</s-text> }.into_any();
        };
        view! {
            <s-box paddingBlockEnd="base">
                <s-stack gap="small">
                    <s-stack direction="inline" gap="small" alignItems="center">
                        <s-badge tone=token_tone(&token.status)>{token.status.clone()}</s-badge>
                        {token_metadata(token.token_id.clone(), token.token_fingerprint.clone(), "14px")}
                    </s-stack>
                    <s-text color="subdued">
                        "Expires " {format_date(token.expires_at.as_deref())} " · Last used "
                        {format_date(token.last_used_at.as_deref())}
                    </s-text>
                </s-stack>
            </s-box>

            <s-box>
                <s-stack direction="inline" justifyContent="space-between" alignItems="center">
                    <s-text type="strong">"Permissions"</s-text>
                    <s-text color="subdued">
                        {move || selected_permissions.with(Vec::len)} " of "
                        {move || permission_keys.with(Vec::len)} " selected"
                    </s-text>
                </s-stack>
                <s-box paddingBlockStart="small">
                    <span style="display:flex;gap:6px;flex-wrap:wrap">
                        <s-button
                            variant="secondary"
                            on:click=move |_| selected_permissions.set(permission_keys.get_untracked())
                            disabled=move || {
                                no_permissions() || all_selected(selected_permissions) || selected_revoked.get()
                            }
                        >
                            "Select all"
                        </s-button>
                        <s-button
                            variant="secondary"
                            on:click=move |_| selected_permissions.set(Vec::new())
                            disabled=move || selected_permissions.with(Vec::is_empty) || selected_revoked.get()
                        >
                            "Clear all"
                        </s-button>
                    </span>
                </s-box>
                <PermissionList
                    groups
                    selection=selected_permissions
                    disabled=selected_revoked
                    max_height="45vh"
                />
            </s-box>

            {move || {
                selected_detail
                    .with(|detail| detail.as_ref().and_then(|detail| detail.recent_audits.clone()))
                    .filter(|audits| !audits.is_empty())
                    .map(audit_table)
            }}
        }
        .into_any()
    };

    let edit_modal = view! {
        <s-modal
            id=EDIT_MODAL_ID
            heading=move || {
                selected
                    .with(|token| token.as_ref().map(|token| format!("Edit {}", token.name)))
                    .unwrap_or_else(|| "Edit token".to_string())
            }
            accessibility-label="Edit MCP token"
            size="large"
        >
            <s-stack direction="block" gap="base">
                {move || edit_error.get().map(|error| view! { <s-banner tone="critical">{error}</s-banner> })}
                {edit_body}
            </s-stack>
            <s-button
                slot="primary-action"
                variant="primary"
                on:click=move |_| spawn_local(state.save_permissions())
                disabled=move || selected.with(Option::is_none) || is_busy("permissions") || selected_revoked.get()
            >
                {move || if is_busy("permissions") { "Saving..." } else { "Save permissions" }}
            </s-button>
            <s-button slot="secondary-actions" variant="secondary" commandFor=EDIT_MODAL_ID command="--hide">
                "Cancel"
            </s-button>
        </s-modal>
    };

    let token_row = move |token: McpTokenSummary| {
        let id = StoredValue::new(token.id.clone());
        let status = token.status.clone();
        let is_revoked = status == "revoked";
        let row_busy = move || busy.with(|busy| busy.as_ref() == Some(&id.get_value()));
        let toggle_status = match status.as_str() {
            "active" => Some(("Disable", "disabled")),
            "disabled" => Some(("Enable", "active")),
            _ => None,
        };
        let stale = (!token.stale_permissions.is_empty()).then(|| token.stale_permissions.join(", "));
        let edited = token.clone();
        view! {
            <s-table-row>
                <s-table-cell>
                    <s-text type="strong">{token.name.clone()}</s-text>
                    <div>{token_metadata(token.token_id.clone(), token.token_fingerprint.clone(), "12px")}</div>
                    {stale.map(|stale| view! { <div style="color:#8a6116;font-size:12px">"Stale: " {stale}</div> })}
                </s-table-cell>
                <s-table-cell>
                    <s-badge tone=token_tone(&status)>{status.clone()}</s-badge>
                </s-table-cell>
                <s-table-cell>{format_date(token.expires_at.as_deref())}</s-table-cell>
                <s-table-cell>{format_date(token.last_used_at.as_deref())}</s-table-cell>
                <s-table-cell>
                    <span style="display:flex;gap:6px;flex-wrap:wrap">
                        <s-button
                            variant="secondary"
                            commandFor=EDIT_MODAL_ID
                            command="--show"
                            on:click=move |_| state.open_edit_token(&edited)
                        >
                            "Edit"
                        </s-button>
                        {toggle_status
                            .map(|(label, next_status)| {
                                view! {
                                    <s-button
                                        variant="secondary"
                                        on:click=move |_| spawn_local(state.patch_token(id.get_value(), next_status))
                                        disabled=row_busy
                                    >
                                        {label}
                                    </s-button>
                                }
                            })}
                        <s-button
                            variant="secondary"
                            on:click=move |_| spawn_local(state.rotate_token(id.get_value()))
                            disabled=move || row_busy() || is_revoked
                        >
                            "Rotate"
                        </s-button>
                        <s-button
                            variant="secondary"
                            tone="critical"
                            on:click=move |_| spawn_local(state.revoke_token(id.get_value()))
                            disabled=move || row_busy() || is_revoked
                        >
                            "Revoke"
                        </s-button>
                    </span>
                </s-table-cell>
            </s-table-row>
        }
    };

    let token_list = move || {
        let tokens = tokens.get();
        if tokens.is_empty() {
            return view! {
                <s-section accessibilityLabel="MCP tokens empty state">
                    <s-grid justifyItems="center" paddingBlock="large-300">
                        <s-box maxInlineSize="420px">
                            <s-stack gap="base" alignItems="center">
                                <s-text type="strong">"No MCP tokens yet"</s-text>
                                <s-text color="subdued">
                                    "Create a scoped token to let an MCP client call OpenShop tools and resources."
                                </s-text>
                                <s-button
                                    variant="primary"
                                    commandFor=CREATE_MODAL_ID
                                    command="--show"
                                    disabled=move || capabilities.with(Option::is_none)
                                    on:click=move |_| create_error.set(None)
                                >
                                    "Create token"
                                </s-button>
                            </s-stack>
                        </s-box>
                    </s-grid>
                </s-section>
            }
            .into_any();
        }
        view! {
            <s-section padding="none" accessibilityLabel="MCP tokens">
                <s-table>
                    <s-table-header-row>
                        <TableHeader list_slot="primary">"Name"</TableHeader>
                        <TableHeader list_slot="inline">"Status"</TableHeader>
                        <TableHeader list_slot="labeled">"Expires"</TableHeader>
                        <TableHeader list_slot="labeled">"Last used"</TableHeader>
                        <TableHeader />
                    </s-table-header-row>
                    <s-table-body>{tokens.into_iter().map(token_row).collect_view()}</s-table-body>
                </s-table>
            </s-section>
        }
        .into_any()
    };

    view! {
        {create_modal}
        {edit_modal}

        <Page heading="MCP">
            <s-button
                slot="primary-action"
                variant="primary"
                commandFor=CREATE_MODAL_ID
                command="--show"
                disabled=move || capabilities.with(Option::is_none)
                on:click=move |_| create_error.set(None)
            >
                "Create token"
            </s-button>

            {move || message.get().map(|message| view! { <s-banner tone=message.tone>{message.text}</s-banner> })}
            {move || {
                one_time_token
                    .get()
                    .map(|token| {
                        let copied = token.clone();
                        view! {
                            <s-banner tone="warning" heading="Copy this token now">
                                <s-stack gap="small">
                                    <s-text>"This value is shown once. Store it before leaving this page."</s-text>
                                    <s-box
                                        padding="base"
                                        background="subdued"
                                        borderRadius="base"
                                        borderWidth="base"
                                        borderColor="base"
                                    >
                                        <code style="word-break:break-all">{token}</code>
                                    </s-box>
                                    <s-button variant="secondary" on:click=move |_| copy(copied.clone(), "Token")>
                                        "Copy token"
                                    </s-button>
                                </s-stack>
                            </s-banner>
                        }
                    })
            }}

            <s-section heading="MCP endpoint" accessibilityLabel="MCP endpoint">
                <s-grid
                    gridTemplateColumns="@container (inline-size > 700px) 1fr auto, 1fr"
                    gap="base"
                    alignItems="center"
                >
                    <s-box padding="base" background="subdued" borderRadius="base" borderWidth="base" borderColor="base">
                        <code style="word-break:break-all">{mcp_url.get_value()}</code>
                    </s-box>
                    <s-button variant="secondary" on:click=move |_| copy(mcp_url.get_value(), "MCP URL")>
                        "Copy URL"
                    </s-button>
                </s-grid>
            </s-section>

            {token_list}
        </Page>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn permission(key: &str, group: &str) -> (String, McpPermissionSummary) {
        let summary = McpPermissionSummary {
            key: key.to_string(),
            label: key.to_uppercase(),
            description: None,
            group: group.to_string(),
        };
        (key.to_string(), summary)
    }

    #[test]
    fn groups_permissions_and_sorts_groups() {
        let permissions = IndexMap::from([
            permission("runs.read", "Runs"),
            permission("flows.run", "Flows"),
            permission("runs.cancel", "Runs"),
            permission("flows.read", "Flows"),
        ]);
        let groups = permission_groups(&permissions, |left, right| left.cmp(right));
        let summary: Vec<(&str, Vec<&str>)> = groups
            .iter()
            .map(|(group, members)| {
                (
                    group.as_str(),
                    members.iter().map(|member| member.key.as_str()).collect(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                ("Flows", vec!["flows.run", "flows.read"]),
                ("Runs", vec!["runs.read", "runs.cancel"])
            ]
        );
    }

    #[test]
    fn builds_the_create_token_payload() {
        let permissions = ["flows.read".to_string()];
        assert_eq!(
            create_token_body("Claude Desktop", "90", &permissions).to_string(),
            r#"{"name":"Claude Desktop","expiresInDays":90,"permissions":["flows.read"]}"#
        );
        assert_eq!(
            create_token_body("CI", "none", &[]).to_string(),
            r#"{"name":"CI","expiresInDays":null,"permissions":[]}"#
        );
    }

    #[test]
    fn toggles_permissions_in_selection_order() {
        let mut selection = vec!["a".to_string()];
        toggle_set(&mut selection, "b", true);
        toggle_set(&mut selection, "b", true);
        toggle_set(&mut selection, "a", false);
        toggle_set(&mut selection, "missing", false);
        assert_eq!(selection, ["b"]);
    }

    #[test]
    fn maps_statuses_to_tones() {
        assert_eq!(token_tone("active"), "success");
        assert_eq!(token_tone("revoked"), "critical");
        assert_eq!(token_tone("disabled"), "neutral");
        assert_eq!(audit_tone("success"), "success");
        assert_eq!(audit_tone("denied"), "warning");
        assert_eq!(audit_tone("error"), "critical");
    }
}
