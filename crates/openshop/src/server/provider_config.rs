//! Validation and public projection of provider configuration.

use serde_json::Value;

use crate::types::{Field, FieldType, Fields, JsonMap, Provider, ProviderTransformer, is_missing};
use crate::validators::describe;

/// JavaScript's `Number(string)` for the inputs a form can send.
fn js_number(text: &str) -> Value {
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

/// The error for a value a validator refused. The summary of a validator may
/// repeat the value, so for a password it is not forwarded: a secret never
/// leaves in an error.
fn validation_error(name: &str, field: &Field, summary: &str) -> String {
    match field.field_type {
        FieldType::Password => format!("Field \"{name}\" is invalid"),
        _ => format!("Field \"{name}\": {summary}"),
    }
}

/// Form inputs arrive as strings: numbers and checkboxes are coerced to their
/// types (`"3"` is 3, `"true"` is true). A value that is not of the type of its
/// field, even after that, is refused with an error naming the field. A null or
/// an empty string counts as no value.
fn coerce_field_value(name: &str, field: &Field, value: Option<Value>) -> Result<Option<Value>, String> {
    let refuse = |expected: &str, value: &Value| Err(format!("Field \"{name}\": must be {expected} (was {})", describe(value)));
    let Some(value) = value.filter(|value| !is_missing(Some(value))) else {
        return Ok(None);
    };
    match (field.field_type, &value) {
        (FieldType::Text | FieldType::Password, Value::String(_)) => Ok(Some(value)),
        (FieldType::Text, other) => refuse("a string", other),
        // A secret is not repeated in the error.
        (FieldType::Password, _) => Err(format!("Field \"{name}\": must be a string")),
        (FieldType::Number, Value::Number(_)) => Ok(Some(value)),
        (FieldType::Number, Value::String(text)) if !text.trim().is_empty() => match js_number(text) {
            Value::Null => refuse("a number", &value),
            number => Ok(Some(number)),
        },
        // A blank number (spaces only) is no value.
        (FieldType::Number, Value::String(_)) => Ok(None),
        (FieldType::Number, other) => refuse("a number", other),
        (FieldType::Checkbox, Value::Bool(_)) => Ok(Some(value)),
        (FieldType::Checkbox, Value::String(text)) if text == "true" || text == "false" => {
            Ok(Some(Value::Bool(text == "true")))
        }
        (FieldType::Checkbox, other) => refuse("a boolean", other),
        (FieldType::Select, Value::String(text))
            if field
                .options
                .iter()
                .flatten()
                .any(|option| option.value == *text) =>
        {
            Ok(Some(value))
        }
        (FieldType::Select, other) => Err(format!(
            "Field \"{name}\": must be one of {} (was {})",
            field
                .options
                .iter()
                .flatten()
                .map(|option| format!("\"{}\"", option.value))
                .collect::<Vec<_>>()
                .join(", "),
            describe(other)
        )),
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

/// Validates submitted configuration: the one of a provider, or of a Shopify
/// Function instance, which has no transformer and no stored values. A blank
/// password keeps the stored one.
pub fn parse_provider_config(
    fields: &Fields,
    transformer: Option<&ProviderTransformer>,
    raw_config: Option<&Value>,
    existing_config: &JsonMap,
) -> Result<JsonMap, String> {
    let empty = JsonMap::new();
    let input = raw_config.and_then(Value::as_object).unwrap_or(&empty);
    let mut data = JsonMap::new();
    let mut kept = Vec::new();

    for (name, field) in fields {
        let submitted = input.get(name).cloned();

        // A blank password keeps the stored value as it is: it was checked and
        // transformed when it was saved, and is neither again.
        let value = match existing_config.get(name) {
            Some(existing) if field.field_type == FieldType::Password && is_missing(submitted.as_ref()) => {
                kept.push(name.as_str());
                Some(existing.clone())
            }
            _ => coerce_field_value(name, field, submitted)?,
        };
        if field.is_required() && is_missing(value.as_ref()) {
            return Err(format!("Field \"{name}\" is required"));
        }
        if let Some(value) = value.filter(|value| !is_missing(Some(value))) {
            data.insert(name.clone(), value);
        }
    }

    let mut transformed = match transformer {
        Some(transformer) => match transformer(data) {
            Value::Object(transformed) => transformed,
            _ => return Err("Provider transformer must return an object".to_string()),
        },
        None => data,
    };

    for (name, field) in fields {
        let value = transformed.get(name).cloned();
        if field.is_required() && is_missing(value.as_ref()) {
            return Err(format!("Field \"{name}\" is required"));
        }

        if kept.contains(&name.as_str()) {
            continue;
        }
        if let (Some(validate), Some(value)) = (&field.validate, value.filter(|value| !is_missing(Some(value)))) {
            let validated = validate(&value).map_err(|summary| validation_error(name, field, &summary))?;
            transformed.insert(name.clone(), validated);
        }
    }

    Ok(transformed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::FieldOption;
    use crate::validators;
    use serde_json::json;

    fn object(value: Value) -> JsonMap {
        value.as_object().cloned().unwrap()
    }

    fn parse(provider: &Provider, raw_config: Option<&Value>, existing_config: &JsonMap) -> Result<JsonMap, String> {
        parse_provider_config(&provider.fields, provider.transformer.as_ref(), raw_config, existing_config)
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
        let parsed = parse(
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
        let parsed = parse(
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
        let error = parse(&warehouse(), Some(&json!({})), &JsonMap::new()).unwrap_err();
        assert_eq!(error, "Field \"endpoint\" is required");

        let error = parse(&warehouse(), None, &JsonMap::new()).unwrap_err();
        assert_eq!(error, "Field \"endpoint\" is required");

        let error = parse(
            &warehouse(),
            Some(&json!({ "endpoint": "not a url", "apiKey": "k", "enabled": true, "retries": 1 })),
            &JsonMap::new(),
        )
        .unwrap_err();
        assert!(error.starts_with("Field \"endpoint\": "), "{error}");

        let error = parse(
            &warehouse(),
            Some(&json!({ "endpoint": "https://w.test", "apiKey": "k", "enabled": true, "retries": 99 })),
            &JsonMap::new(),
        )
        .unwrap_err();
        assert!(error.starts_with("Field \"retries\": "), "{error}");
    }

    fn typed() -> Provider {
        Provider::new("typed")
            .field(
                "mode",
                Field::select(
                    "Mode",
                    [FieldOption::new("Live", "live"), FieldOption::new("Test", "test")],
                ),
            )
            .field("retries", Field::number("Retries").optional())
            .field("sandbox", Field::checkbox("Sandbox").optional())
            .field("label", Field::text("Label").optional())
            .field("token", Field::password("Token").optional())
    }

    fn typed_error(config: Value) -> String {
        parse(&typed(), Some(&config), &JsonMap::new()).unwrap_err()
    }

    #[test]
    fn refuses_a_select_value_that_is_not_an_option() {
        assert_eq!(
            typed_error(json!({ "mode": "prod" })),
            "Field \"mode\": must be one of \"live\", \"test\" (was \"prod\")"
        );
        assert_eq!(
            typed_error(json!({ "mode": 1 })),
            "Field \"mode\": must be one of \"live\", \"test\" (was 1)"
        );
        let parsed = parse(&typed(), Some(&json!({ "mode": "test" })), &JsonMap::new()).unwrap();
        assert_eq!(parsed["mode"], "test");
    }

    #[test]
    fn refuses_a_number_that_is_not_one() {
        assert_eq!(
            typed_error(json!({ "mode": "live", "retries": "abc" })),
            "Field \"retries\": must be a number (was \"abc\")"
        );
        assert_eq!(
            typed_error(json!({ "mode": "live", "retries": "Infinity" })),
            "Field \"retries\": must be a number (was \"Infinity\")"
        );
        assert_eq!(
            typed_error(json!({ "mode": "live", "retries": true })),
            "Field \"retries\": must be a number (was true)"
        );
        let parsed = parse(
            &typed(),
            Some(&json!({ "mode": "live", "retries": " 2.5 " })),
            &JsonMap::new(),
        )
        .unwrap();
        assert_eq!(parsed["retries"], 2.5);
        let parsed = parse(
            &typed(),
            Some(&json!({ "mode": "live", "retries": "" })),
            &JsonMap::new(),
        )
        .unwrap();
        assert!(!parsed.contains_key("retries"));
    }

    #[test]
    fn refuses_a_checkbox_that_is_not_a_boolean() {
        for bad in [json!("yes"), json!("on"), json!(1), json!([])] {
            let error = typed_error(json!({ "mode": "live", "sandbox": bad }));
            assert!(error.starts_with("Field \"sandbox\": must be a boolean (was "), "{error}");
        }
        for (sent, saved) in [(json!(true), true), (json!("true"), true), (json!("false"), false)] {
            let parsed =
                parse(&typed(), Some(&json!({ "mode": "live", "sandbox": sent })), &JsonMap::new())
                    .unwrap();
            assert_eq!(parsed["sandbox"], saved);
        }
    }

    #[test]
    fn refuses_text_and_password_that_are_not_strings() {
        assert_eq!(
            typed_error(json!({ "mode": "live", "label": 5 })),
            "Field \"label\": must be a string (was 5)"
        );
        assert_eq!(
            typed_error(json!({ "mode": "live", "token": { "a": 1 } })),
            "Field \"token\": must be a string"
        );
    }

    #[test]
    fn never_repeats_a_secret_in_an_error() {
        let provider = Provider::new("erp")
            .field("token", Field::password("Token").validate(validators::non_empty()))
            .transformer(|mut data| {
                if let Some(Value::String(token)) = data.get("token").cloned() {
                    data.insert("token".into(), json!({ "accessToken": token }));
                }
                Value::Object(data)
            });
        // The summary of a validator is not forwarded for a password.
        let error =
            parse(&provider, Some(&json!({ "token": "secret-token" })), &JsonMap::new()).unwrap_err();
        assert_eq!(error, "Field \"token\" is invalid");

        // A stored password is neither validated nor transformed again.
        let stored = object(json!({ "token": { "accessToken": "secret-token" } }));
        let parsed = parse(&provider, Some(&json!({ "token": "" })), &stored).unwrap();
        assert_eq!(parsed, stored);

        // A validator of the application that repeats the value in its summary.
        let chatty = Provider::new("custom").field(
            "token",
            Field::password("Token").validate(|value| match value.as_str() {
                Some(token) => Ok(json!({ "accessToken": token })),
                None => Err(format!("invalid token: {value}")),
            }),
        );
        let stored = parse(&chatty, Some(&json!({ "token": "review-secret" })), &JsonMap::new()).unwrap();
        assert_eq!(parse(&chatty, Some(&json!({})), &stored).unwrap(), stored);
        let refusing = Provider::new("custom").field(
            "token",
            Field::password("Token").validate(|value| Err(format!("invalid token: {value}"))),
        );
        let error = parse(&refusing, Some(&json!({ "token": "review-secret" })), &JsonMap::new())
            .unwrap_err();
        assert_eq!(error, "Field \"token\" is invalid");

        let plain = Provider::new("erp").field("token", Field::password("Token"));
        let parsed = parse(&plain, Some(&json!({})), &stored).unwrap();
        assert_eq!(parsed, stored);
    }

    #[test]
    fn ignores_fields_that_are_not_declared() {
        let parsed = parse(
            &typed(),
            Some(&json!({ "mode": "live", "extra": "x", "__proto__": { "a": 1 } })),
            &JsonMap::new(),
        )
        .unwrap();
        assert_eq!(parsed, object(json!({ "mode": "live" })));
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
        let parsed = parse(
            &provider,
            Some(&json!({ "endpoint": "https://warehouse.test/" })),
            &JsonMap::new(),
        )
        .unwrap();
        assert_eq!(parsed["endpoint"], "https://warehouse.test");

        let broken = Provider::new("warehouse")
            .field("endpoint", Field::text("Endpoint"))
            .transformer(|_| Value::Null);
        let error = parse(&broken, Some(&json!({ "endpoint": "x" })), &JsonMap::new()).unwrap_err();
        assert_eq!(error, "Provider transformer must return an object");
    }
}
