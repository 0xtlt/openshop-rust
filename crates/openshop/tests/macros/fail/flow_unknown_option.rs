use openshop::prelude::*;

#[flow(retries = 3)]
async fn sync_orders(_ctx: FlowContext) -> Result<()> {
    Ok(())
}

fn main() {}
