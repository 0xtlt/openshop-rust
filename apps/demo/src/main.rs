#[tokio::main]
async fn main() -> openshop::Result<()> {
    openshop::cli::run(openshop_demo::config).await
}
