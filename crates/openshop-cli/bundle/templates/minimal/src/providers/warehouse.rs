use openshop::prelude::*;
use serde::{Deserialize, Serialize};

/// One order as the warehouse receives it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Order {
    pub id: String,
    pub name: String,
}

/// The warehouse of a shop: the fields are the form a merchant fills in the
/// admin, the methods are what flows can do with it.
#[derive(Debug, ProviderConfig)]
pub struct Warehouse {
    #[field(label = "API URL", placeholder = "https://warehouse.example.com/api", url)]
    pub api_url: String,
    #[field(label = "API Key", password, non_empty)]
    pub api_key: String,
}

/// Named `warehouse`, after the struct.
#[provider]
impl Warehouse {
    /// The connection check of the admin.
    #[checker]
    async fn check(&self) -> Result<bool> {
        Ok(!self.api_url.is_empty() && !self.api_key.is_empty())
    }

    /// Sends orders to the warehouse and returns how many it accepted.
    pub async fn push(&self, orders: Vec<Order>) -> Result<usize> {
        // Call your service here, at `self.api_url` with `self.api_key`.
        println!("[warehouse] Pushing {} orders to {}", orders.len(), self.api_url);
        Ok(orders.len())
    }
}
