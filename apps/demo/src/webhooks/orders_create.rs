use openshop::{Result, WebhookContext, webhook};
use serde::Deserialize;

/// What the handler reads from the order Shopify sends.
#[derive(Debug, Deserialize)]
pub struct CreatedOrder {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub total_price: String,
}

#[webhook("orders/create")]
pub async fn orders_create(ctx: WebhookContext, order: CreatedOrder) -> Result<()> {
    println!(
        "[webhook] New order on {}: {} — {}",
        ctx.shop, order.name, order.total_price
    );
    Ok(())
}
