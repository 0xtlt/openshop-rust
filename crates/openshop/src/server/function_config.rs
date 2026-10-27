//! Validation of Shopify Function instance configuration.

use serde_json::Value;

use super::provider_config::{coerce_field_value, validation_error};
use crate::types::{Fields, JsonMap, is_missing};

pub fn validate_function_config(fields: &Fields, raw_config: Option<&Value>) -> Result<JsonMap, String> {
    let empty = JsonMap::new();
    let input = raw_config.and_then(Value::as_object).unwrap_or(&empty);
    let mut config = JsonMap::new();

    for (name, field) in fields {
        let value = coerce_field_value(name, field, input.get(name).cloned())?;
        if field.is_required() && is_missing(value.as_ref()) {
            return Err(format!("Field \"{name}\" is required"));
        }

        let Some(value) = value.filter(|value| !is_missing(Some(value))) else {
            continue;
        };
        let value = match &field.validate {
            Some(validate) => validate(&value).map_err(|summary| validation_error(name, field, &summary))?,
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
    use crate::types::Field;
    use indexmap::IndexMap;
    use serde_json::json;

    fn fields() -> Fields {
        IndexMap::from([
            (
                "minQuantity".to_string(),
                Field::number("Minimum quantity").validate(validators::integer_min(1)),
            ),
            (
                "percentage".to_string(),
                Field::number("Discount percentage").validate(validators::range(Some(0.0), Some(100.0))),
            ),
            (
                "excludedTags".to_string(),
                Field::text("Excluded product tags").optional(),
            ),
            ("stackable".to_string(), Field::checkbox("Stackable").optional()),
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
    fn does_not_repeat_a_password_in_a_validator_error() {
        let fields = IndexMap::from([(
            "token".to_string(),
            Field::password("Token").validate(|value| Err(format!("invalid token: {value}"))),
        )]);
        let error = validate_function_config(&fields, Some(&json!({ "token": "function-secret" }))).unwrap_err();
        assert_eq!(error, "Field \"token\" is invalid");
    }

    #[test]
    fn refuses_values_of_the_wrong_type() {
        let error =
            validate_function_config(&fields(), Some(&json!({ "minQuantity": "abc", "percentage": 10 }))).unwrap_err();
        assert_eq!(error, "Field \"minQuantity\": must be a number (was \"abc\")");
        let error = validate_function_config(
            &fields(),
            Some(&json!({ "minQuantity": 1, "percentage": 10, "stackable": "yes" })),
        )
        .unwrap_err();
        assert_eq!(error, "Field \"stackable\": must be a boolean (was \"yes\")");
        let error = validate_function_config(
            &fields(),
            Some(&json!({ "minQuantity": 1, "percentage": 10, "excludedTags": 3 })),
        )
        .unwrap_err();
        assert_eq!(error, "Field \"excludedTags\": must be a string (was 3)");
    }
}
