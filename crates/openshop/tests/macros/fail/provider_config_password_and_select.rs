use openshop::prelude::*;

#[derive(ProviderConfig)]
struct Warehouse {
    #[field(password, select("Europe" = "eu"))]
    region: String,
}

fn main() {}
