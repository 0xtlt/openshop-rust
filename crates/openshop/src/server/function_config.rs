//! Validation of Shopify Function instance configuration.

use serde_json::Value;

use super::provider_config::{js_number, js_truthy};
use crate::types::{FieldDef, FieldDefinitions, FieldType, JsonMap, is_missing};

fn coerce_field_value(field: &FieldDef, value: Option<Value>) -> Option<Value> {
    match (field.field_type, value) {
        (FieldType::Number, Some(Value::String(text))) if !text.trim().is_empty() => Some(js_number(&text)),
        (FieldType::Checkbox, Some(Value::String(text))) => Some(Value::Bool(text == "true")),
        (FieldType::Checkbox, Some(other)) => Some(Value::Bool(js_truthy(&other))),
        (_, value) => value,
    }
}

pub fn validate_function_config(fields: &FieldDefinitions, raw_config: Option<&Value>) -> Result<JsonMap, String> {
    let empty = JsonMap::new();
    let input = raw_config.and_then(Value::as_object).unwrap_or(&empty);
    let mut config = JsonMap::new();

    for (name, field) in fields {
        let value = coerce_field_value(field, input.get(name).cloned());
        if field.is_required() && is_missing(value.as_ref()) {
            return Err(format!("Field \"{name}\" is required"));
        }

        let Some(value) = value.filter(|value| !is_missing(Some(value))) else {
            continue;
        };
        let value = match &field.validate {
            Some(validate) => validate(&value).map_err(|summary| format!("Field \"{name}\": {summary}"))?,
            None => value,
        };
        config.insert(name.clone(), value);
    }

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validators;
    use indexmap::IndexMap;
    use serde_json::json;

    fn fields() -> FieldDefinitions {
        IndexMap::from([
            (
                "minQuantity".to_string(),
                FieldDef::number("Minimum quantity").validate(validators::integer_min(1)),
            ),
            (
                "percentage".to_string(),
                FieldDef::number("Discount percentage").validate(validators::number_range(0.0, 100.0)),
            ),
            (
                "excludedTags".to_string(),
                FieldDef::text("Excluded product tags").optional(),
            ),
            ("stackable".to_string(), FieldDef::checkbox("Stackable").optional()),
        ])
    }

    #[test]
    fn coerces_and_validates_fields() {
        let config = validate_function_config(
            &fields(),
            Some(&json!({ "minQuantity": "3", "percentage": 12.5, "excludedTags": "", "stackable": "true" })),
        )
        .unwrap();
        assert_eq!(
            Value::Object(config),
            json!({ "minQuantity": 3, "percentage": 12.5, "stackable": true })
        );
    }

    #[test]
    fn reports_missing_required_fields() {
        assert_eq!(
            validate_function_config(&fields(), Some(&json!({ "percentage": 10 }))).unwrap_err(),
            "Field \"minQuantity\" is required"
        );
        assert_eq!(
            validate_function_config(&fields(), None).unwrap_err(),
            "Field \"minQuantity\" is required"
        );
    }

    #[test]
    fn reports_validator_failures_with_the_field_name() {
        let error =
            validate_function_config(&fields(), Some(&json!({ "minQuantity": 0, "percentage": 10 }))).unwrap_err();
        assert_eq!(error, "Field \"minQuantity\": must be at least 1 (was 0)");
        let error =
            validate_function_config(&fields(), Some(&json!({ "minQuantity": 2, "percentage": 101 }))).unwrap_err();
        assert_eq!(error, "Field \"percentage\": must be at most 100 (was 101)");
    }

    #[test]
    fn unparseable_numbers_count_as_missing() {
        let error =
            validate_function_config(&fields(), Some(&json!({ "minQuantity": "abc", "percentage": 10 }))).unwrap_err();
        assert_eq!(error, "Field \"minQuantity\" is required");
    }
}
