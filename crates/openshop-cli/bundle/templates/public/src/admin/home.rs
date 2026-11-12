//! The home of the admin: what the last stock check of the shop found, and a
//! way to run one now.

use maud::html;
use openshop::prelude::*;
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

    /// Queues a check for the shop of the session, which a worker runs: the
    /// page is rendered again, with a toast.
    #[action]
    async fn check_now(ctx: AdminContext) -> Result<Outcome> {
        use crate::flows::check_stock::check_stock;

        check_stock::dispatch(&ctx).await?;
        Ok(Outcome::toast("Stock check started"))
    }

    /// Rendered by the server with what `summary` returns. While a check runs,
    /// the page is fetched again every 5 seconds.
    #[view]
    fn view(summary: Summary) -> Markup {
        let found = match &summary.checked_at {
            Some(at) => format!("{} variants run low. Last check: {at}.", summary.low_stock.len()),
            None => "No check yet.".to_string(),
        };
        html! {
            s-page heading="Stock watch" {
                @if summary.checking {
                    s-banner tone="info" data-openshop-refresh="5000" {
                        "A check is running. "
                        s-link href=(Self::PATH) { "Refresh" }
                    }
                }
                s-section heading=(format!("Under {} units", summary.threshold)) {
                    s-paragraph { (found) }
                    s-stack gap="small" {
                        @for variant in &summary.low_stock {
                            s-stack direction="inline" justifyContent="space-between" alignItems="center" {
                                s-stack direction="inline" gap="small" alignItems="center" {
                                    s-text type="strong" { (variant.name) }
                                    @if let Some(sku) = &variant.sku {
                                        s-text color="subdued" { (sku) }
                                    }
                                }
                                s-badge tone=(if variant.available > 0 { "warning" } else { "critical" }) {
                                    (variant.available) " left"
                                }
                            }
                        }
                    }
                }
                form method="post" {
                    input type="hidden" name="_action" value=(Self::CHECK_NOW);
                    s-button type="submit" variant="primary" { "Check now" }
                }
            }
        }
    }
}
