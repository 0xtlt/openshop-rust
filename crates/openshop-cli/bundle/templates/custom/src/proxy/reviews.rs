use openshop::prelude::*;

/// Shopify renders the template with the objects of the storefront; what only
/// the server knows reaches it as a variable.
#[proxy(GET, "/reviews", liquid)]
pub async fn reviews(ctx: ProxyContext) -> Result<Liquid> {
    Ok(liquid!("reviews.liquid").assign("app_shop", &ctx.shop))
}
