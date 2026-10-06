use openshop::{FieldDef, JsonMap, Provider, Value, validators};

fn is_set(config: &JsonMap, key: &str) -> bool {
    config.get(key).and_then(Value::as_str).is_some_and(|value| !value.is_empty())
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
            FieldDef::password("API Key").validate(validators::non_empty()),
        )
        .checker(|config| async move { Ok(is_set(&config, "apiUrl") && is_set(&config, "apiKey")) })
        .typed_method("push", |config, data: Vec<JsonMap>| async move {
            let api_url = config.get("apiUrl").and_then(Value::as_str).unwrap_or_default();
            println!("[warehouse] Pushing {} records to {api_url}", data.len());
            Ok(true)
        })
}
