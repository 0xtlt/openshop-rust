use openshop::prelude::*;

#[derive(ProviderConfig)]
struct Warehouse {
    api_url: String,
}

#[provider]
impl Warehouse {
    pub fn ping(&self) -> Result<String> {
        Ok(self.api_url.clone())
    }
}

fn main() {}
