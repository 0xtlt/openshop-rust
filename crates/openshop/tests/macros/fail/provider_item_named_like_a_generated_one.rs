use openshop::prelude::*;

#[derive(ProviderConfig)]
struct Warehouse {
    api_url: String,
}

#[provider]
impl Warehouse {
    pub fn on() -> Self {
        Self { api_url: String::new() }
    }
}

fn main() {}
