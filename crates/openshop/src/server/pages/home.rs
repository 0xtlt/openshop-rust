//! `/`: the dashboard of the admin. Cards of figures (the flows, the crons
//! enabled and disabled, the providers, the last activity), a banner of the
//! failed runs among the latest with a link to each, the latest runs, and the
//! providers with the status of their connection; each part only when its
//! page of the framework is not disabled. Fetched again every 10 seconds.
//! An application with its own page at `/` shows that one instead.

use anyhow::Result;
use chrono::{DateTime, Utc};
use indexmap::IndexMap;
use maud::{Markup, html};
use serde_json::Value;

use super::BuiltinPage;
use super::providers::connection_status;
use super::ui::{status_tone, time_ago, to_fixed_1};
use crate::admin::{AdminPublicError, Outcome};
use crate::admin_paths::rpc::encode_uri_component as encode;
use crate::db::schema::FlowRun;
use crate::server::api::pages::resolved_admin_pages;
use crate::server::api::providers::provider_items;
use crate::server::api::runs::query_runs;
use crate::server::mcp::core::{cron_items, flow_items};
use crate::types::AdminPageMode;

/// How often the page is fetched again, in milliseconds.
const REFRESH_MS: u32 = 10_000;
/// The latest runs read; the table shows the first [`RECENT_SHOWN`].
const RECENT_LIMIT: &str = "10";
const RECENT_SHOWN: usize = 5;
/// The failed runs the banner names.
const FAILED_SHOWN: usize = 3;

/// A run as the dashboard shows it.
struct Run {
    id: String,
    flow: String,
    status: String,
    created_at: DateTime<Utc>,
    /// From its start to its end, when it has both.
    elapsed_ms: Option<i64>,
}

impl From<FlowRun> for Run {
    fn from(run: FlowRun) -> Self {
        let elapsed_ms = match (run.started_at, run.completed_at) {
            (Some(started), Some(completed)) => Some((completed - started).num_milliseconds()),
            _ => None,
        };
        Self {
            id: run.id.to_string(),
            flow: run.flow_name,
            status: run.status,
            created_at: run.created_at,
            elapsed_ms,
        }
    }
}

/// A provider and the status of its connection.
struct ProviderRow {
    name: String,
    configured: bool,
    last_check_ok: Option<bool>,
}

/// What the dashboard shows; a part is `None` when its page is disabled.
struct Dashboard {
    now: DateTime<Utc>,
    /// The number of flows, and the latest runs, newest first.
    flows: Option<(usize, Vec<Run>)>,
    /// The crons enabled, and those disabled.
    crons: Option<(usize, usize)>,
    providers: Option<Vec<ProviderRow>>,
}

pub(super) async fn render(page: &BuiltinPage<'_>) -> Result<Markup> {
    let (app, session) = (page.app, page.session);
    let pages = resolved_admin_pages(app.config());
    let shown = |mode: AdminPageMode| mode != AdminPageMode::Disabled;

    let flows = if shown(pages.flows) {
        let count = flow_items(app, &session.shop, |_| Value::Null).len();
        let query = IndexMap::from([("limit".to_string(), RECENT_LIMIT.to_string())]);
        let runs = query_runs(app, session, None, &query)
            .await?
            .map_err(|refused| AdminPublicError::new("INVALID_QUERY", refused).status(400))?;
        Some((count, runs.into_iter().map(Run::from).collect()))
    } else {
        None
    };
    let crons = if shown(pages.crons) {
        let items = cron_items(app, &session.shopify_app, &session.shop).await?;
        let enabled = items
            .iter()
            .filter(|cron| cron.get("enabled").and_then(Value::as_bool) == Some(true))
            .count();
        Some((enabled, items.len() - enabled))
    } else {
        None
    };
    let providers = if shown(pages.providers) {
        let items = provider_items(app, session).await?;
        Some(
            items
                .into_iter()
                .map(|provider| ProviderRow {
                    name: provider.name.to_string(),
                    configured: provider.configured,
                    last_check_ok: provider.last_check_ok,
                })
                .collect(),
        )
    } else {
        None
    };
    Ok(view(&Dashboard {
        now: Utc::now(),
        flows,
        crons,
        providers,
    }))
}

/// How long a run took: `1.3s`, `250ms`; `…` while it runs, `—` otherwise.
fn duration(elapsed_ms: Option<i64>, status: &str) -> String {
    match elapsed_ms.map(|ms| ms.max(0)) {
        Some(ms) if ms >= 1_000 => format!("{}s", to_fixed_1(ms as f64 / 1_000.0)),
        Some(ms) => format!("{ms}ms"),
        None if status == "running" => "…".to_string(),
        None => "—".to_string(),
    }
}

fn view(dashboard: &Dashboard) -> Markup {
    let now = dashboard.now;
    let runs = dashboard
        .flows
        .as_ref()
        .map(|(_, runs)| runs.as_slice())
        .unwrap_or_default();
    let failed: Vec<&Run> = runs.iter().filter(|run| run.status == "failed").collect();
    html! {
        s-page heading="Dashboard" data-openshop-refresh=(REFRESH_MS) {
            @if !failed.is_empty() {
                s-banner tone="critical" heading=(format!("{} failed run{}", failed.len(), if failed.len() > 1 { "s" } else { "" })) {
                    @for run in failed.iter().take(FAILED_SHOWN) {
                        s-paragraph {
                            s-text type="strong" { (run.flow) }
                            " — " (time_ago(now, run.created_at)) " "
                            s-link href=(format!("/runs/{}", run.id)) { "View" }
                        }
                    }
                }
            }
            (stats(dashboard))
            @if dashboard.flows.is_some() {
                (recent_runs(now, runs))
            }
            @if let Some(providers) = &dashboard.providers {
                (providers_section(providers))
            }
        }
    }
}

fn stat_card(label: &str, figure: Markup) -> Markup {
    html! {
        s-grid-item {
            s-box padding="base" background="subdued" borderRadius="base" borderWidth="base" borderColor="base" {
                s-stack gap="small-200" {
                    s-text color="subdued" { (label) }
                    (figure)
                }
            }
        }
    }
}

fn stats(dashboard: &Dashboard) -> Markup {
    if dashboard.flows.is_none() && dashboard.crons.is_none() && dashboard.providers.is_none() {
        return html! {};
    }
    html! {
        s-section {
            s-query-container {
                s-grid gridTemplateColumns="@container (inline-size > 500px) 1fr 1fr 1fr 1fr, 1fr 1fr" gap="base" {
                    @if let Some((count, _)) = &dashboard.flows {
                        (stat_card("Flows", html! { s-heading { (count) } }))
                    }
                    @if let Some((enabled, disabled)) = dashboard.crons {
                        (stat_card("Crons", html! {
                            s-stack direction="inline" gap="small-200" alignItems="baseline" {
                                s-heading { (enabled) }
                                @if disabled > 0 {
                                    s-text color="subdued" { "(" (disabled) " disabled)" }
                                }
                            }
                        }))
                    }
                    @if let Some(providers) = &dashboard.providers {
                        (stat_card("Providers", html! { s-heading { (providers.len()) } }))
                    }
                    @if let Some((_, runs)) = &dashboard.flows {
                        @let last = runs.first().map_or_else(|| "—".to_string(), |run| time_ago(dashboard.now, run.created_at));
                        (stat_card("Last activity", html! { s-heading { (last) } }))
                    }
                }
            }
        }
    }
}

fn recent_runs(now: DateTime<Utc>, runs: &[Run]) -> Markup {
    html! {
        s-section heading="Recent runs" {
            s-table {
                s-table-header-row {
                    s-table-header listSlot="primary" { "Flow" }
                    s-table-header listSlot="inline" { "Status" }
                    s-table-header listSlot="labeled" { "When" }
                    s-table-header listSlot="labeled" { "Duration" }
                }
                s-table-body {
                    @if runs.is_empty() {
                        s-table-row {
                            s-table-cell { s-text color="subdued" { "No runs yet" } }
                            s-table-cell {} s-table-cell {} s-table-cell {}
                        }
                    }
                    @for run in runs.iter().take(RECENT_SHOWN) {
                        s-table-row {
                            s-table-cell {
                                s-link href=(format!("/runs/{}", run.id)) tone="neutral" {
                                    s-text type="strong" { (run.flow) }
                                }
                            }
                            s-table-cell { s-badge tone=(status_tone(&run.status)) { (run.status) } }
                            s-table-cell { s-text color="subdued" { (time_ago(now, run.created_at)) } }
                            s-table-cell { s-text color="subdued" { (duration(run.elapsed_ms, &run.status)) } }
                        }
                    }
                }
            }
            s-box paddingBlockStart="base" {
                s-button variant="tertiary" href="/flows" { "View all runs" }
            }
        }
    }
}

fn providers_section(providers: &[ProviderRow]) -> Markup {
    html! {
        s-section heading="Providers" {
            s-table {
                s-table-header-row {
                    s-table-header listSlot="primary" { "Name" }
                    s-table-header listSlot="inline" { "Status" }
                    s-table-header {}
                }
                s-table-body {
                    @if providers.is_empty() {
                        s-table-row {
                            s-table-cell { s-text color="subdued" { "No providers registered." } }
                            s-table-cell {} s-table-cell {}
                        }
                    }
                    @for provider in providers {
                        @let (tone, label) = connection_status(provider.configured, provider.last_check_ok);
                        s-table-row {
                            s-table-cell { s-text type="strong" { (provider.name) } }
                            s-table-cell { s-badge tone=(tone) { (label) } }
                            s-table-cell {
                                s-button variant="secondary" href=(format!("/providers?provider={}", encode(&provider.name))) {
                                    "Configure"
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The dashboard has no form.
pub(super) async fn submit(_page: &BuiltinPage<'_>, _action: &str, _fields: &[(String, String)]) -> Result<Outcome> {
    Err(super::no_action())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-09T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn run(id: &str, flow: &str, status: &str, minutes_ago: i64, elapsed_ms: Option<i64>) -> Run {
        Run {
            id: id.to_string(),
            flow: flow.to_string(),
            status: status.to_string(),
            created_at: now() - Duration::minutes(minutes_ago),
            elapsed_ms,
        }
    }

    fn provider(name: &str, configured: bool, last_check_ok: Option<bool>) -> ProviderRow {
        ProviderRow {
            name: name.to_string(),
            configured,
            last_check_ok,
        }
    }

    fn dashboard() -> Dashboard {
        Dashboard {
            now: now(),
            flows: Some((
                2,
                vec![
                    run("r1", "syncOrders", "failed", 5, Some(1_250)),
                    run("r2", "<b>import</b>", "completed", 90, Some(999)),
                    run("r3", "syncOrders", "running", 60 * 30, None),
                    run("r4", "syncOrders", "failed", 60 * 49, None),
                ],
            )),
            crons: Some((3, 1)),
            providers: Some(vec![
                provider("warehouse", true, Some(true)),
                provider("erp & co", false, None),
            ]),
        }
    }

    #[test]
    fn renders_the_cards_the_failed_runs_the_runs_and_the_providers() {
        let html = view(&dashboard()).into_string();
        // Fetched again every 10 seconds.
        assert!(
            html.starts_with(r#"<s-page heading="Dashboard" data-openshop-refresh="10000">"#),
            "{html}"
        );
        // The failed runs, each with a link.
        assert!(
            html.contains(r#"<s-banner tone="critical" heading="2 failed runs">"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<s-paragraph><s-text type="strong">syncOrders</s-text> — 5m ago <s-link href="/runs/r1">View</s-link></s-paragraph>"#),
            "{html}"
        );
        assert!(html.contains(r#"— 2d ago <s-link href="/runs/r4">"#), "{html}");
        // The cards.
        assert!(
            html.contains(r#"<s-text color="subdued">Flows</s-text><s-heading>2</s-heading>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<s-heading>3</s-heading><s-text color="subdued">(1 disabled)</s-text>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<s-text color="subdued">Providers</s-text><s-heading>2</s-heading>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<s-text color="subdued">Last activity</s-text><s-heading>5m ago</s-heading>"#),
            "{html}"
        );
        // The runs: a link to each, its status, when, how long.
        assert!(
            html.contains(r#"<s-link href="/runs/r2" tone="neutral"><s-text type="strong">&lt;b&gt;import&lt;/b&gt;</s-text></s-link>"#),
            "{html}"
        );
        assert!(html.contains(r#"<s-badge tone="critical">failed</s-badge>"#), "{html}");
        assert!(html.contains(r#"<s-badge tone="warning">running</s-badge>"#), "{html}");
        assert!(html.contains(r#"<s-text color="subdued">1h ago</s-text><"#), "{html}");
        assert!(html.contains(r#"<s-text color="subdued">1.3s</s-text>"#), "{html}");
        assert!(html.contains(r#"<s-text color="subdued">999ms</s-text>"#), "{html}");
        assert!(html.contains(r#"<s-text color="subdued">…</s-text>"#), "{html}");
        assert!(
            html.contains(r#"<s-button variant="tertiary" href="/flows">View all runs</s-button>"#),
            "{html}"
        );
        // The providers, each with its status and a link to its form.
        assert!(
            html.contains(r#"<s-badge tone="success">Connected</s-badge>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<s-badge tone="warning">Not configured</s-badge>"#),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<s-button variant="secondary" href="/providers?provider=erp%20%26%20co">Configure</s-button>"#
            ),
            "{html}"
        );
        assert!(
            html.contains(r#"<s-text type="strong">erp &amp; co</s-text>"#),
            "{html}"
        );
    }

    #[test]
    fn shows_five_runs_and_three_failed_ones() {
        let runs = (0..8)
            .map(|index| run(&format!("r{index}"), "syncOrders", "failed", index, None))
            .collect();
        let html = view(&Dashboard {
            flows: Some((1, runs)),
            ..dashboard()
        })
        .into_string();
        assert!(html.contains(r#"heading="8 failed runs""#), "{html}");
        assert_eq!(html.matches(">View</s-link>").count(), 3, "{html}");
        assert_eq!(html.matches(r#"tone="neutral">"#).count(), 5, "{html}");
        // The newest run is the last activity.
        assert!(html.contains("<s-heading>just now</s-heading>"), "{html}");
    }

    #[test]
    fn says_when_there_is_nothing_yet() {
        let html = view(&Dashboard {
            now: now(),
            flows: Some((0, Vec::new())),
            crons: Some((0, 0)),
            providers: Some(Vec::new()),
        })
        .into_string();
        assert!(!html.contains("s-banner"), "{html}");
        assert!(html.contains("No runs yet"), "{html}");
        assert!(html.contains("No providers registered."), "{html}");
        assert!(html.contains("<s-heading>—</s-heading>"), "{html}");
        assert!(!html.contains("disabled)"), "{html}");
        let one = view(&Dashboard {
            flows: Some((1, vec![run("r1", "syncOrders", "failed", 0, None)])),
            ..dashboard()
        })
        .into_string();
        assert!(one.contains(r#"heading="1 failed run""#), "{one}");
    }

    #[test]
    fn leaves_out_each_part_whose_page_is_disabled() {
        let html = view(&Dashboard {
            flows: None,
            ..dashboard()
        })
        .into_string();
        assert!(!html.contains("Recent runs"), "{html}");
        assert!(!html.contains("Last activity"), "{html}");
        assert!(!html.contains(">Flows<"), "{html}");
        assert!(!html.contains("s-banner"), "{html}");
        assert!(
            html.contains(">Crons<") && html.contains(r#"heading="Providers""#),
            "{html}"
        );

        let html = view(&Dashboard {
            providers: None,
            crons: None,
            ..dashboard()
        })
        .into_string();
        assert!(!html.contains("Providers"), "{html}");
        assert!(!html.contains(">Crons<"), "{html}");

        let html = view(&Dashboard {
            now: now(),
            flows: None,
            crons: None,
            providers: None,
        })
        .into_string();
        assert!(!html.contains("s-section"), "{html}");
        assert!(html.contains("data-openshop-refresh"), "{html}");
    }

    #[test]
    fn writes_ages_and_durations_as_the_dashboard_did() {
        let now = now();
        assert_eq!(time_ago(now, now - Duration::seconds(30)), "just now");
        assert_eq!(time_ago(now, now + Duration::minutes(5)), "just now");
        assert_eq!(time_ago(now, now - Duration::minutes(45)), "45m ago");
        assert_eq!(time_ago(now, now - Duration::seconds(4 * 3_600 + 3_599)), "4h ago");
        assert_eq!(time_ago(now, now - Duration::days(3)), "3d ago");

        assert_eq!(duration(Some(1_250), "completed"), "1.3s");
        assert_eq!(duration(Some(1_150), "completed"), "1.1s");
        assert_eq!(duration(Some(999), "completed"), "999ms");
        assert_eq!(duration(Some(-5), "completed"), "0ms");
        assert_eq!(duration(None, "running"), "…");
        assert_eq!(duration(None, "pending"), "—");

        assert_eq!(status_tone("completed"), "success");
        assert_eq!(status_tone("sleeping"), "info");
        assert_eq!(status_tone("canceled"), "neutral");
        assert_eq!(status_tone("skipped"), "auto");
    }
}
