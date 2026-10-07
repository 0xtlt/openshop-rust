#[tokio::main]
async fn main() -> openshop::Result<()> {
    openshop_demo::APP.run().await
}
