use openshop::prelude::*;

#[flow(concurrency = allow, concurrency = reject)]
async fn sync_orders(_ctx: FlowContext) -> Result<()> {
    Ok(())
}

fn main() {}
