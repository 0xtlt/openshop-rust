use openshop::prelude::*;

#[webhook]
async fn uninstalled(_ctx: WebhookContext) -> Result<()> {
    Ok(())
}

fn main() {}
