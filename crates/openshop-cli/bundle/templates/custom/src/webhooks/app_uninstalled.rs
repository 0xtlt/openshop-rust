use openshop::prelude::*;

#[webhook("app/uninstalled")]
pub async fn app_uninstalled(ctx: WebhookContext) -> Result<()> {
    println!("[webhook] App uninstalled from {}", ctx.shop);
    Ok(())
}
