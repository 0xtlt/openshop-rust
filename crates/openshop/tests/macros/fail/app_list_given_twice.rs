use openshop::prelude::*;

#[flow]
async fn sync_orders(_ctx: FlowContext) -> Result<()> {
    Ok(())
}

static APP: App = app! {
    flows: [sync_orders],
    flows: [sync_orders],
};

fn main() {
    let _ = &APP;
}
