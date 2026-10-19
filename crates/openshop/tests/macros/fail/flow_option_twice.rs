use openshop::prelude::*;

#[flow(timeout = "10s", timeout = "20s")]
async fn sync_orders(_ctx: FlowContext) -> Result<()> {
    Ok(())
}

fn main() {}
