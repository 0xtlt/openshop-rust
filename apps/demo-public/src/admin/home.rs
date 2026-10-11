//! The home of the admin: what the last stock check of the shop found, and a
//! way to run one now.

use leptos::prelude::*;
use leptos::task::spawn_local;
use openshop::prelude::*;
use openshop::ui::{AdminActionOptions, shopify, use_action, use_loader};
use serde::{Deserialize, Serialize};

/// A variant that runs low, as the page shows it.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct LowStock {
    pub name: String,
    pub sku: Option<String>,
    pub available: i64,
}

/// What a merchant sees of the stock checks of their shop.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    /// Under how many units a variant runs low.
    pub threshold: i32,
    /// Whether a check waits or runs.
    pub checking: bool,
    /// When the last check ended.
    pub checked_at: Option<String>,
    /// What it found.
    pub low_stock: Vec<LowStock>,
}

pub struct HomePage;

#[admin_page("/")]
impl HomePage {
    #[loader]
    async fn summary(ctx: AdminContext) -> Result<Summary> {
        use openshop::{FlowRun, StepResult};

        use crate::flows::check_stock::{FIND_LOW_STOCK, LowStockVariant, check_stock};
        use crate::models::stock_settings::StockSettings;

        // The runs of the shop of the session: a merchant never reads those of another shop.
        let checks = || FlowRun::of(&ctx).filter(FlowRun::FLOW_NAME.eq(check_stock::NAME));
        let waiting = checks()
            .filter(FlowRun::STATUS.is_in(["pending", "sleeping", "running"]))
            .count()
            .await?;
        let last = checks()
            .filter(FlowRun::STATUS.eq("completed"))
            .order_by(FlowRun::COMPLETED_AT.desc())
            .first()
            .await?;

        // What the check found is the output of its step.
        let mut low_stock = Vec::new();
        if let Some(run) = &last {
            let found = StepResult::of_run(&ctx.db, run.id)
                .filter(StepResult::STEP_NAME.eq(FIND_LOW_STOCK))
                .filter(StepResult::STATUS.eq("completed"))
                .first()
                .await?;
            if let Some(output) = found.and_then(|step| step.output) {
                let variants: Vec<LowStockVariant> = openshop::serde_json::from_value(output)?;
                low_stock = variants
                    .into_iter()
                    .map(|variant| LowStock {
                        name: variant.name,
                        sku: variant.sku,
                        available: variant.available,
                    })
                    .collect();
            }
        }

        Ok(Summary {
            threshold: StockSettings::threshold_of(&ctx).await?,
            checking: waiting > 0,
            checked_at: last
                .and_then(|run| run.completed_at)
                .map(|at| at.format("%-d %B %Y, %H:%M UTC").to_string()),
            low_stock,
        })
    }

    #[action]
    async fn check_now(ctx: AdminContext) -> Result<()> {
        use crate::flows::check_stock::check_stock;

        // Queued for the shop of the session, and run by a worker.
        check_stock::dispatch(&ctx).await?;
        Ok(())
    }

    #[view]
    fn view() -> impl IntoView {
        let summary = use_loader(Self::SUMMARY, || ());
        let check = use_action(Self::CHECK_NOW);
        let check_now = move |_| {
            spawn_local(async move {
                let options = AdminActionOptions::new().revalidate(Self::SUMMARY);
                if let Ok(Some(())) = check.invoke((), options).await {
                    shopify::toast("Stock check started");
                }
            });
        };

        view! {
            <s-page heading="Stock watch">
                {move || summary.loading.get().then(|| view! { <s-spinner accessibilityLabel="Loading the last check" /> })}
                {move || summary.error.get().map(|error| view! { <s-banner tone="critical">{error.message}</s-banner> })}
                {move || check.error.get().map(|error| view! { <s-banner tone="critical">{error.message}</s-banner> })}
                {move || {
                    summary.data.get().map(|last| {
                        let found = match &last.checked_at {
                            Some(at) => format!("{} variants run low. Last check: {at}.", last.low_stock.len()),
                            None => "No check yet.".to_string(),
                        };
                        view! {
                            {last.checking.then(|| view! {
                                <s-banner tone="info">
                                    "A check is running. "
                                    <s-link on:click=move |_| summary.revalidate()>"Refresh"</s-link>
                                </s-banner>
                            })}
                            <s-section heading=format!("Under {} units", last.threshold)>
                                <s-paragraph>{found}</s-paragraph>
                                <s-stack gap="small">
                                    {last
                                        .low_stock
                                        .into_iter()
                                        .map(|variant| {
                                            let tone = if variant.available > 0 { "warning" } else { "critical" };
                                            view! {
                                                <s-stack direction="inline" justifyContent="space-between" alignItems="center">
                                                    <s-stack direction="inline" gap="small" alignItems="center">
                                                        <s-text type="strong">{variant.name}</s-text>
                                                        <s-text color="subdued">{variant.sku}</s-text>
                                                    </s-stack>
                                                    <s-badge tone=tone>{format!("{} left", variant.available)}</s-badge>
                                                </s-stack>
                                            }
                                        })
                                        .collect_view()}
                                </s-stack>
                            </s-section>
                        }
                    })
                }}
                <s-button variant="primary" loading=move || check.pending.get() on:click=check_now>
                    "Check now"
                </s-button>
            </s-page>
        }
    }
}
