use crate::{
    admin_pages::use_admin_pages,
    components::page::Page,
    components::table_header::TableHeader,
    dom::{every, now_ms},
    fetch::{ApiError, api_json},
    format::{dashboard_duration, elapsed_ms, time_ago},
    provider_status::provider_connection_status,
    reactive::set_if_changed,
    router::route,
    types::{CronItem, FlowRun, FlowSummary, ProviderSummary, status_tone},
};
use futures_util::future::try_join4;
use leptos::{prelude::*, task::spawn_local};
use openshop_shared::AdminPageMode;
use std::{cell::Cell, rc::Rc};

async fn load_if<T>(enabled: bool, path: &str) -> Result<Vec<T>, ApiError>
where
    T: serde::de::DeserializeOwned,
{
    if enabled { api_json(path).await } else { Ok(Vec::new()) }
}

#[component]
fn StatCard(label: &'static str, children: Children) -> impl IntoView {
    view! {
        <s-grid-item>
            <s-box padding="base" background="subdued" borderRadius="base" borderWidth="base" borderColor="base">
                <s-stack gap="small-200">
                    <s-text color="subdued">{label}</s-text>
                    {children()}
                </s-stack>
            </s-box>
        </s-grid-item>
    }
}

#[component]
pub fn Home() -> impl IntoView {
    let pages = use_admin_pages();
    let enabled = |mode: fn(&openshop_shared::ResolvedAdminPages) -> AdminPageMode| {
        Memo::new(move |_| mode(&pages.get()) != AdminPageMode::Disabled)
    };
    let flows_enabled = enabled(|pages| pages.flows);
    let crons_enabled = enabled(|pages| pages.crons);
    let providers_enabled = enabled(|pages| pages.providers);

    let flow_count = RwSignal::new(0_usize);
    let recent_runs = RwSignal::new(Vec::<FlowRun>::new());
    let crons = RwSignal::new(Vec::<CronItem>::new());
    let providers = RwSignal::new(Vec::<ProviderSummary>::new());
    let load_error = RwSignal::new(None::<String>);
    // Relative times are refreshed with every poll.
    let now = RwSignal::new(now_ms());

    Effect::new(move |_| {
        let (flows_on, crons_on, providers_on) = (flows_enabled.get(), crons_enabled.get(), providers_enabled.get());
        let in_flight = Rc::new(Cell::new(false));
        let load = move || {
            if in_flight.replace(true) {
                return;
            }
            let in_flight = Rc::clone(&in_flight);
            spawn_local(async move {
                let loaded = try_join4(
                    load_if::<FlowSummary>(flows_on, "/api/flows"),
                    load_if::<FlowRun>(flows_on, "/api/runs?limit=10"),
                    load_if::<CronItem>(crons_on, "/api/crons"),
                    load_if::<ProviderSummary>(providers_on, "/api/providers"),
                )
                .await;
                match loaded {
                    Ok((flows, runs, cron_items, provider_items)) => {
                        set_if_changed(flow_count, flows.len());
                        set_if_changed(recent_runs, runs);
                        set_if_changed(crons, cron_items);
                        set_if_changed(providers, provider_items);
                        set_if_changed(load_error, None);
                    }
                    Err(error) => set_if_changed(load_error, Some(error.to_string())),
                }
                now.set(now_ms());
                in_flight.set(false);
            });
        };
        load();
        every(10_000, load);
    });

    let failed_banner = move || {
        let failed: Vec<FlowRun> = recent_runs
            .get()
            .into_iter()
            .filter(|run| run.status == "failed")
            .collect();
        (!failed.is_empty()).then(|| {
            let heading = format!("{} failed run{}", failed.len(), if failed.len() > 1 { "s" } else { "" });
            view! {
                <s-banner tone="critical" heading=heading>
                    {failed
                        .into_iter()
                        .take(3)
                        .map(|run| {
                            view! {
                                <s-paragraph>
                                    <s-text type="strong">{run.flow_name}</s-text>
                                    " — "
                                    {move || time_ago(now.get(), Some(&run.created_at))}
                                    " "
                                    <s-link href=format!("/runs/{}", run.id)>"View"</s-link>
                                </s-paragraph>
                            }
                        })
                        .collect_view()}
                </s-banner>
            }
        })
    };

    let stats = move || {
        (flows_enabled.get() || crons_enabled.get() || providers_enabled.get()).then(|| {
            view! {
                <s-section>
                    <s-query-container>
                        <s-grid
                            gridTemplateColumns="@container (inline-size > 500px) 1fr 1fr 1fr 1fr, 1fr 1fr"
                            gap="base"
                        >
                            {flows_enabled
                                .get()
                                .then(|| {
                                    view! {
                                        <StatCard label="Flows">
                                            <s-heading>{move || flow_count.get()}</s-heading>
                                        </StatCard>
                                    }
                                })}
                            {crons_enabled
                                .get()
                                .then(|| {
                                    let count = move |enabled: bool| {
                                        crons.with(|crons| crons.iter().filter(|cron| cron.enabled == enabled).count())
                                    };
                                    view! {
                                        <StatCard label="Crons">
                                            <s-stack direction="inline" gap="small-200" alignItems="baseline">
                                                <s-heading>{move || count(true)}</s-heading>
                                                {move || {
                                                    let disabled = count(false);
                                                    (disabled > 0)
                                                        .then(|| {
                                                            view! {
                                                                <s-text color="subdued">
                                                                    {format!("({disabled} disabled)")}
                                                                </s-text>
                                                            }
                                                        })
                                                }}
                                            </s-stack>
                                        </StatCard>
                                    }
                                })}
                            {providers_enabled
                                .get()
                                .then(|| {
                                    view! {
                                        <StatCard label="Providers">
                                            <s-heading>{move || providers.with(Vec::len)}</s-heading>
                                        </StatCard>
                                    }
                                })}
                            {flows_enabled
                                .get()
                                .then(|| {
                                    view! {
                                        <StatCard label="Last activity">
                                            <s-heading>
                                                {move || {
                                                    recent_runs
                                                        .with(|runs| {
                                                            runs.first().map(|run| time_ago(now.get(), Some(&run.created_at)))
                                                        })
                                                        .unwrap_or_else(|| "—".to_string())
                                                }}
                                            </s-heading>
                                        </StatCard>
                                    }
                                })}
                        </s-grid>
                    </s-query-container>
                </s-section>
            }
        })
    };

    let recent_runs_section = move || {
        flows_enabled.get().then(|| {
            view! {
                <s-section heading="Recent runs">
                    <s-table>
                        <s-table-header-row>
                            <TableHeader list_slot="primary">"Flow"</TableHeader>
                            <TableHeader list_slot="inline">"Status"</TableHeader>
                            <TableHeader list_slot="labeled">"When"</TableHeader>
                            <TableHeader list_slot="labeled">"Duration"</TableHeader>
                        </s-table-header-row>
                        <s-table-body>
                            {move || {
                                let runs = recent_runs.get();
                                if runs.is_empty() {
                                    return view! {
                                        <s-table-row>
                                            <s-table-cell>
                                                <s-text color="subdued">"No runs yet"</s-text>
                                            </s-table-cell>
                                            <s-table-cell />
                                            <s-table-cell />
                                            <s-table-cell />
                                        </s-table-row>
                                    }
                                        .into_any();
                                }
                                runs.into_iter()
                                    .take(5)
                                    .map(|run| {
                                        let elapsed = elapsed_ms(run.started_at.as_deref(), run.completed_at.as_deref());
                                        view! {
                                            <s-table-row>
                                                <s-table-cell>
                                                    <s-link href=format!("/runs/{}", run.id) tone="neutral">
                                                        <s-text type="strong">{run.flow_name}</s-text>
                                                    </s-link>
                                                </s-table-cell>
                                                <s-table-cell>
                                                    <s-badge tone=status_tone(&run.status)>{run.status.clone()}</s-badge>
                                                </s-table-cell>
                                                <s-table-cell>
                                                    <s-text color="subdued">
                                                        {move || time_ago(now.get(), Some(&run.created_at))}
                                                    </s-text>
                                                </s-table-cell>
                                                <s-table-cell>
                                                    <s-text color="subdued">{dashboard_duration(elapsed, &run.status)}</s-text>
                                                </s-table-cell>
                                            </s-table-row>
                                        }
                                    })
                                    .collect_view()
                                    .into_any()
                            }}
                        </s-table-body>
                    </s-table>
                    <s-box paddingBlockStart="base">
                        <s-button variant="tertiary" on:click=move |_| route("/flows")>
                            "View all runs"
                        </s-button>
                    </s-box>
                </s-section>
            }
        })
    };

    let providers_section = move || {
        providers_enabled.get().then(|| {
            view! {
                <s-section heading="Providers">
                    <s-table>
                        <s-table-header-row>
                            <TableHeader list_slot="primary">"Name"</TableHeader>
                            <TableHeader list_slot="inline">"Status"</TableHeader>
                            <TableHeader />
                        </s-table-header-row>
                        <s-table-body>
                            {move || {
                                providers
                                    .get()
                                    .into_iter()
                                    .map(|provider| {
                                        let connection = provider_connection_status(
                                            provider.configured,
                                            provider.last_check_ok,
                                        );
                                        view! {
                                            <s-table-row>
                                                <s-table-cell>
                                                    <s-text type="strong">{provider.name}</s-text>
                                                </s-table-cell>
                                                <s-table-cell>
                                                    <s-badge tone=connection.tone>{connection.label}</s-badge>
                                                </s-table-cell>
                                                <s-table-cell>
                                                    <s-button variant="secondary" on:click=move |_| route("/providers")>
                                                        "Configure"
                                                    </s-button>
                                                </s-table-cell>
                                            </s-table-row>
                                        }
                                    })
                                    .collect_view()
                            }}
                        </s-table-body>
                    </s-table>
                </s-section>
            }
        })
    };

    view! {
        <Page heading="Dashboard">
            {move || load_error.get().map(|error| view! { <s-banner tone="critical">{error}</s-banner> })}
            {failed_banner}
            {stats}
            {recent_runs_section}
            {providers_section}
        </Page>
    }
}
