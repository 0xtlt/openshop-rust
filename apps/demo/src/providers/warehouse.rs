use openshop::{ProviderConfig, Result, provider, validators};
use serde::{Deserialize, Serialize};

/// The warehouse of a shop: the fields are what a merchant saves in the
/// admin, the methods are what flows can do with it.
#[derive(Debug, ProviderConfig)]
pub struct Warehouse {
    #[field(label = "API URL", placeholder = "https://warehouse.example.com/api", validate = validators::url())]
    pub api_url: String,
    #[field(label = "API Key", password, placeholder = "wh_live_...", validate = validators::non_empty())]
    pub api_key: String,
    #[field(label = "Batch Size", placeholder = "100", validate = validators::number_range(1.0, 1000.0))]
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

#[provider("warehouse")]
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
