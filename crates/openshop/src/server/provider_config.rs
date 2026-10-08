//! Validation and public projection of provider configuration.

use serde_json::Value;

use crate::types::{Field, FieldType, JsonMap, Provider, is_missing};

/// JavaScript's `Number(string)` for the inputs a form can send.
pub(crate) fn js_number(text: &str) -> Value {
    match text.trim().parse::<f64>() {
        Ok(number) if number.is_finite() => {
            if number.fract() == 0.0 && number.abs() < 9e15 {
                Value::from(number as i64)
            } else {
                Value::from(number)
            }
        }
        _ => Value::Null,
    }
}

/// JavaScript truthiness, used for checkbox values.
pub(crate) fn js_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|float| float != 0.0 && !float.is_nan()),
        Value::String(text) => !text.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// Form inputs arrive as strings: numbers and checkboxes are coerced to their types.
pub(crate) fn coerce_field_value(field: &Field, value: Option<Value>) -> Option<Value> {
    let value = value?;
    match (field.field_type, &value) {
        (FieldType::Number, Value::String(text)) if !text.trim().is_empty() => Some(js_number(text)),
        (FieldType::Checkbox, Value::String(text)) => Some(Value::Bool(text == "true")),
        (FieldType::Checkbox, other) => Some(Value::Bool(js_truthy(other))),
        _ => Some(value),
    }
}

pub fn provider_config_is_complete(provider: &Provider, stored_config: &JsonMap) -> bool {
    provider
        .fields
        .iter()
        .filter(|(_, field)| field.is_required())
        .all(|(name, _)| !is_missing(stored_config.get(name)))
}

/// Field metadata for the admin UI. Passwords only reveal whether a value is saved.
pub fn provider_fields_for_response(provider: &Provider, stored_config: &JsonMap) -> JsonMap {
    provider
        .fields
        .iter()
        .map(|(name, field)| {
            let mut metadata = field.metadata();
            if field.field_type == FieldType::Password {
                metadata.insert("hasValue".into(), Value::Bool(!is_missing(stored_config.get(name))));
            }
            (name.clone(), Value::Object(metadata))
        })
        .collect()
}

/// Stored values safe to send to the browser: everything except passwords.
pub fn public_provider_config(provider: &Provider, stored_config: &JsonMap) -> JsonMap {
    provider
        .fields
        .iter()
        .filter(|(_, field)| field.field_type != FieldType::Password)
        .filter_map(|(name, _)| Some((name.clone(), stored_config.get(name)?.clone())))
        .collect()
}

/// Validates submitted configuration. A blank password keeps the stored one.
pub fn parse_provider_config(
    provider: &Provider,
    raw_config: Option<&Value>,
    existing_config: &JsonMap,
) -> Result<JsonMap, String> {
    let empty = JsonMap::new();
    let input = raw_config.and_then(Value::as_object).unwrap_or(&empty);
    let mut data = JsonMap::new();

    for (name, field) in &provider.fields {
        let mut value = input.get(name).cloned();

        if field.field_type == FieldType::Password
            && is_missing(value.as_ref())
            && let Some(existing) = existing_config.get(name)
        {
            value = Some(existing.clone());
        }

        let value = coerce_field_value(field, value);
        if field.is_required() && is_missing(value.as_ref()) {
            return Err(format!("Field \"{name}\" is required"));
        }
        if let Some(value) = value.filter(|value| !is_missing(Some(value))) {
            data.insert(name.clone(), value);
        }
    }

    let mut transformed = match &provider.transformer {
        Some(transformer) => match transformer(data) {
            Value::Object(transformed) => transformed,
            _ => return Err("Provider transformer must return an object".to_string()),
        },
        None => data,
    };

    for (name, field) in &provider.fields {
        let value = transformed.get(name).cloned();
        if field.is_required() && is_missing(value.as_ref()) {
            return Err(format!("Field \"{name}\" is required"));
        }

        if let (Some(validate), Some(value)) = (&field.validate, value.filter(|value| !is_missing(Some(value)))) {
            let validated = validate(&value).map_err(|summary| format!("Field \"{name}\": {summary}"))?;
            transformed.insert(name.clone(), validated);
        }
    }

    Ok(transformed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validators;
    use serde_json::json;

    fn object(value: Value) -> JsonMap {
        value.as_object().cloned().unwrap()
    }

    fn warehouse() -> Provider {
        Provider::new("warehouse")
            .field("endpoint", Field::text("Endpoint").validate(validators::url()))
            .field("apiKey", Field::password("API key"))
            .field("note", Field::text("Note").optional())
            .field("enabled", Field::checkbox("Enabled"))
            .field(
                "retries",
                Field::number("Retries").validate(validators::number_range(0.0, 10.0)),
            )
    }

    #[test]
    fn is_incomplete_when_a_required_field_is_missing() {
        let provider = Provider::new("warehouse")
            .field("endpoint", Field::text("Endpoint"))
            .field("apiKey", Field::password("API key"));

        assert!(!provider_config_is_complete(&provider, &JsonMap::new()));
        assert!(!provider_config_is_complete(
            &provider,
            &object(json!({ "endpoint": "https://warehouse.test" }))
        ));
        assert!(!provider_config_is_complete(
            &provider,
            &object(json!({ "endpoint": "https://warehouse.test", "apiKey": "" }))
        ));
    }

    #[test]
    fn is_complete_when_required_values_are_saved_including_secrets() {
        assert!(provider_config_is_complete(
            &warehouse(),
            &object(json!({
                "endpoint": "https://warehouse.test",
                "apiKey": "secret-1",
                "enabled": false,
                "retries": 0,
            }))
        ));
    }

    #[test]
    fn treats_a_provider_with_no_required_fields_as_configured() {
        assert!(provider_config_is_complete(&Provider::new("empty"), &JsonMap::new()));
        let provider = Provider::new("notes").field("note", Field::text("Note").optional());
        assert!(provider_config_is_complete(&provider, &JsonMap::new()));
    }

    #[test]
    fn hides_passwords_from_public_config_and_reports_presence() {
        let stored = object(json!({ "endpoint": "https://warehouse.test", "apiKey": "secret-1" }));
        let public = public_provider_config(&warehouse(), &stored);
        assert_eq!(public, object(json!({ "endpoint": "https://warehouse.test" })));

        let fields = provider_fields_for_response(&warehouse(), &stored);
        assert_eq!(
            fields["apiKey"],
            json!({ "type": "password", "label": "API key", "hasValue": true })
        );
        assert_eq!(fields["endpoint"], json!({ "type": "text", "label": "Endpoint" }));
        assert_eq!(
            fields["note"],
            json!({ "type": "text", "label": "Note", "required": false })
        );

        let fields = provider_fields_for_response(&warehouse(), &JsonMap::new());
        assert_eq!(fields["apiKey"]["hasValue"], false);
    }

    #[test]
    fn coerces_form_strings_and_validates() {
        let parsed = parse_provider_config(
            &warehouse(),
            Some(&json!({
                "endpoint": "https://warehouse.test",
                "apiKey": "secret-1",
                "enabled": "true",
                "retries": "3",
                "note": "",
            })),
            &JsonMap::new(),
        )
        .unwrap();
        assert_eq!(
            parsed,
            object(json!({
                "endpoint": "https://warehouse.test",
                "apiKey": "secret-1",
                "enabled": true,
                "retries": 3,
            }))
        );
    }

    #[test]
    fn keeps_the_stored_password_when_left_blank() {
        let existing = object(json!({ "apiKey": "stored-secret" }));
        let parsed = parse_provider_config(
            &warehouse(),
            Some(&json!({ "endpoint": "https://warehouse.test", "apiKey": "", "enabled": false, "retries": 1 })),
            &existing,
        )
        .unwrap();
        assert_eq!(parsed["apiKey"], "stored-secret");
        assert_eq!(parsed["enabled"], false);
    }

    #[test]
    fn reports_missing_and_invalid_fields() {
        let error = parse_provider_config(&warehouse(), Some(&json!({})), &JsonMap::new()).unwrap_err();
        assert_eq!(error, "Field \"endpoint\" is required");

        let error = parse_provider_config(&warehouse(), None, &JsonMap::new()).unwrap_err();
        assert_eq!(error, "Field \"endpoint\" is required");

        let error = parse_provider_config(
            &warehouse(),
            Some(&json!({ "endpoint": "not a url", "apiKey": "k", "enabled": true, "retries": 1 })),
            &JsonMap::new(),
        )
        .unwrap_err();
        assert!(error.starts_with("Field \"endpoint\": "), "{error}");

        let error = parse_provider_config(
            &warehouse(),
            Some(&json!({ "endpoint": "https://w.test", "apiKey": "k", "enabled": true, "retries": 99 })),
            &JsonMap::new(),
        )
        .unwrap_err();
        assert!(error.starts_with("Field \"retries\": "), "{error}");
    }

    #[test]
    fn applies_the_transformer_before_validation() {
        let provider = Provider::new("warehouse")
            .field("endpoint", Field::text("Endpoint").validate(validators::url()))
            .transformer(|mut data| {
                if let Some(Value::String(endpoint)) = data.get("endpoint").cloned() {
                    data.insert(
                        "endpoint".into(),
                        Value::from(endpoint.trim_end_matches('/').to_string()),
                    );
                }
                Value::Object(data)
            });
        let parsed = parse_provider_config(
            &provider,
            Some(&json!({ "endpoint": "https://warehouse.test/" })),
            &JsonMap::new(),
        )
        .unwrap();
        assert_eq!(parsed["endpoint"], "https://warehouse.test");

        let broken = Provider::new("warehouse")
            .field("endpoint", Field::text("Endpoint"))
            .transformer(|_| Value::Null);
        let error = parse_provider_config(&broken, Some(&json!({ "endpoint": "x" })), &JsonMap::new()).unwrap_err();
        assert_eq!(error, "Provider transformer must return an object");
    }
}
