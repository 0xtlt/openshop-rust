use openshop::{FieldDef, Provider, ProviderCall, validators};
use serde::{Deserialize, Serialize};

/// What a merchant saves for the warehouse, named after the fields below.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WarehouseConfig {
    pub api_url: String,
    pub api_key: String,
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

/// Sends records to the warehouse; `true` once they are accepted.
pub const PUSH: ProviderCall<Vec<WarehouseRecord>, bool> = ProviderCall::new("warehouse", "push");
/// Removes a record by its reference.
pub const DELETE: ProviderCall<String, ()> = ProviderCall::new("warehouse", "delete");

pub fn warehouse() -> Provider {
    Provider::new("warehouse")
        .field(
            "apiUrl",
            FieldDef::text("API URL")
                .placeholder("https://warehouse.example.com/api")
                .validate(validators::url()),
        )
        .field(
            "apiKey",
            FieldDef::password("API Key")
                .placeholder("wh_live_...")
                .validate(validators::non_empty()),
        )
        .field(
            "batchSize",
            FieldDef::number("Batch Size")
                .placeholder("100")
                .validate(validators::number_range(1.0, 1000.0)),
        )
        .typed_checker(|config: WarehouseConfig| async move {
            println!("[warehouse] Checking connection to {}...", config.api_url);
            Ok(true)
        })
        .handle(PUSH, |config: WarehouseConfig, records| async move {
            for batch in records.chunks(config.batch_size.max(1) as usize) {
                println!("[warehouse] Pushing {} records to {}", batch.len(), config.api_url);
            }
            Ok(true)
        })
        .handle(DELETE, |config: WarehouseConfig, reference| async move {
            println!("[warehouse] Deleting {reference} from {}", config.api_url);
            Ok(())
        })
}
