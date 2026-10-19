use openshop::prelude::*;

#[derive(ProviderConfig)]
struct Warehouse {
    api_url: String,
    #[field(name = "apiUrl")]
    url: String,
}

fn main() {}
