use openshop::prelude::*;

#[flow(timeout = "ten minutes")]
async fn sync_orders(_ctx: FlowContext) -> Result<()> {
    Ok(())
}

fn main() {}
