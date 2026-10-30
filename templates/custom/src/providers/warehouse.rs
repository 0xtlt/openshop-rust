use openshop::prelude::*;
use serde::{Deserialize, Serialize};

/// The warehouse of a shop: the fields are what a merchant saves in the
/// admin, the methods are what flows can do with it.
#[derive(Debug, ProviderConfig)]
pub struct Warehouse {
    #[field(label = "API URL", placeholder = "https://warehouse.example.com/api", url)]
    pub api_url: String,
    #[field(
        label = "API Key",
        password,
        placeholder = "wh_live_...",
        description = "Found in your warehouse dashboard, under API settings.",
        non_empty
    )]
    pub api_key: String,
    #[field(
        placeholder = "100",
        description = "Number of records sent per request, between 1 and 1000.",
        range(min = 1, max = 1000)
    )]
    pub batch_size: u32,
}

/// One order as the warehouse expects it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WarehouseRecord {
    #[serde(rename = "ref")]
    pub reference: String,
    pub amount: String,
    pub line_count: usize,
    pub exported_at: String,
}

/// Named `warehouse`, after the struct.
#[provider]
impl Warehouse {
    #[checker]
    async fn check(&self) -> Result<bool> {
        println!("[warehouse] Checking connection to {}...", self.api_url);
        Ok(true)
    }

    /// Sends records to the warehouse; `true` once they are accepted.
    pub async fn push(&self, records: Vec<WarehouseRecord>) -> Result<bool> {
        for batch in records.chunks(self.batch_size.max(1) as usize) {
            println!("[warehouse] Pushing {} records to {}", batch.len(), self.api_url);
        }
        Ok(true)
    }

    /// Removes a record by its reference.
    pub async fn delete(&self, reference: String) -> Result<()> {
        println!("[warehouse] Deleting {reference} from {}", self.api_url);
        Ok(())
    }
}
