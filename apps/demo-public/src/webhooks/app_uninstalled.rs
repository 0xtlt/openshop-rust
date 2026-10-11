use openshop::prelude::*;

use crate::models::stock_settings::StockSettings;

/// A shop that leaves takes what it saved with it.
#[webhook("app/uninstalled")]
pub async fn app_uninstalled(ctx: WebhookContext) -> Result<()> {
    let removed = StockSettings::of(&ctx).query().delete().await?;
    println!(
        "[webhook] App uninstalled from {}: {removed} settings removed",
        ctx.shop
    );
    Ok(())
}
