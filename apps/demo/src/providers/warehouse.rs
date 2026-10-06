use openshop::{FieldDef, JsonMap, Provider, Value, json, validators};

fn api_url(config: &JsonMap) -> &str {
    config.get("apiUrl").and_then(Value::as_str).unwrap_or("unconfigured")
}

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
        .checker(|config| async move {
            let Some(url) = config.get("apiUrl").and_then(Value::as_str) else {
                return Ok(false);
            };
            println!("[warehouse] Checking connection to {url}...");
            Ok(true)
        })
        .typed_method("push", |config, data: Vec<JsonMap>| async move {
            println!("[warehouse] Pushing {} records to {}", data.len(), api_url(&config));
            Ok(true)
        })
        .method("delete", |config, id| async move {
            println!("[warehouse] Deleting {id} from {}", api_url(&config));
            Ok(json!(null))
        })
}
