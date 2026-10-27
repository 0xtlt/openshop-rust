//! A small JSON Schema validator for MCP tool arguments and flow inputs.
//!
//! Supported keywords: `type`, `enum`, `const`, `minimum`, `maximum`,
//! `exclusiveMinimum`, `exclusiveMaximum`, `minLength`, `maxLength`, `required`,
//! `properties`, `additionalProperties: false`, `items`, `anyOf`, `oneOf` and
//! local `$ref`. Unknown keywords are ignored.

use serde_json::Value;

use crate::types::JsonMap;

fn is_integer(value: &Value) -> bool {
    match value {
        Value::Number(number) => {
            number.is_i64() || number.is_u64() || number.as_f64().is_some_and(|float| float.fract() == 0.0)
        }
        _ => false,
    }
}

fn type_matches(value: &Value, json_type: &str) -> bool {
    match json_type {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "number" => value.as_f64().is_some_and(f64::is_finite),
        "integer" => is_integer(value),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => false,
    }
}

fn expected_types(schema: &JsonMap) -> Option<Vec<&str>> {
    match schema.get("type")? {
        Value::String(json_type) => Some(vec![json_type.as_str()]),
        Value::Array(types) => types.iter().map(Value::as_str).collect(),
        _ => None,
    }
}

/// Primitive equality as in JavaScript's `Object.is`: objects and arrays never match.
fn same_primitive(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(left), Value::Number(right)) => left.as_f64() == right.as_f64(),
        (Value::Object(_) | Value::Array(_), _) => false,
        _ => left == right,
    }
}

fn format_number(number: f64) -> String {
    if number.fract() == 0.0 && number.abs() < 1e15 {
        format!("{}", number as i64)
    } else {
        number.to_string()
    }
}

fn resolve_ref<'a>(root: &'a Value, reference: &str) -> Option<&'a Value> {
    root.pointer(reference.strip_prefix('#')?)
}

fn validate_value(value: &Value, schema: &Value, root: &Value, path: &str, depth: usize) -> Option<String> {
    let schema = schema.as_object()?;
    if depth > 64 {
        return None;
    }

    if let Some(target) = schema
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|reference| resolve_ref(root, reference))
        && let Some(error) = validate_value(value, target, root, path, depth + 1)
    {
        return Some(error);
    }

    let types = expected_types(schema);
    if let Some(types) = &types
        && !types.iter().any(|json_type| type_matches(value, json_type))
    {
        return Some(format!("{path} must be {}", types.join(" or ")));
    }

    if let Some(options) = schema.get("enum").and_then(Value::as_array)
        && !options.iter().any(|option| same_primitive(option, value))
    {
        let listed: Vec<String> = options.iter().map(Value::to_string).collect();
        return Some(format!("{path} must be one of {}", listed.join(", ")));
    }

    if let Some(constant) = schema.get("const")
        && constant != value
    {
        return Some(format!("{path} must be {constant}"));
    }

    for keyword in ["anyOf", "oneOf"] {
        if let Some(alternatives) = schema.get(keyword).and_then(Value::as_array)
            && !alternatives.is_empty()
        {
            let errors: Vec<String> = alternatives
                .iter()
                .filter_map(|alternative| validate_value(value, alternative, root, path, depth + 1))
                .collect();
            if errors.len() == alternatives.len() {
                return errors.into_iter().next();
            }
        }
    }

    let has_type = |json_type: &str| types.as_ref().is_some_and(|types| types.contains(&json_type));

    if (has_type("number") || has_type("integer"))
        && let Some(number) = value.as_f64()
    {
        if let Some(minimum) = schema.get("minimum").and_then(Value::as_f64)
            && number < minimum
        {
            return Some(format!("{path} must be >= {}", format_number(minimum)));
        }
        if let Some(maximum) = schema.get("maximum").and_then(Value::as_f64)
            && number > maximum
        {
            return Some(format!("{path} must be <= {}", format_number(maximum)));
        }
        if let Some(minimum) = schema.get("exclusiveMinimum").and_then(Value::as_f64)
            && number <= minimum
        {
            return Some(format!("{path} must be > {}", format_number(minimum)));
        }
        if let Some(maximum) = schema.get("exclusiveMaximum").and_then(Value::as_f64)
            && number >= maximum
        {
            return Some(format!("{path} must be < {}", format_number(maximum)));
        }
    }

    if has_type("string")
        && let Some(text) = value.as_str()
    {
        let length = text.chars().count() as u64;
        if let Some(minimum) = schema.get("minLength").and_then(Value::as_u64)
            && length < minimum
        {
            return Some(format!("{path} must be at least {minimum} characters"));
        }
        if let Some(maximum) = schema.get("maxLength").and_then(Value::as_u64)
            && length > maximum
        {
            return Some(format!("{path} must be at most {maximum} characters"));
        }
    }

    if has_type("object")
        && let Some(object) = value.as_object()
    {
        let empty = JsonMap::new();
        let properties = schema.get("properties").and_then(Value::as_object).unwrap_or(&empty);

        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for key in required.iter().filter_map(Value::as_str) {
                if !object.contains_key(key) {
                    return Some(format!("{path}.{key} is required"));
                }
            }
        }

        if schema.get("additionalProperties") == Some(&Value::Bool(false)) {
            for key in object.keys() {
                if !properties.contains_key(key) {
                    return Some(format!("{path}.{key} is not allowed"));
                }
            }
        }

        for (key, child_schema) in properties {
            let Some(child) = object.get(key) else {
                continue;
            };
            if let Some(error) = validate_value(child, child_schema, root, &format!("{path}.{key}"), depth + 1) {
                return Some(error);
            }
        }
    }

    if has_type("array")
        && let Some(items) = value.as_array()
        && let Some(item_schema) = schema.get("items")
    {
        for (index, item) in items.iter().enumerate() {
            if let Some(error) = validate_value(item, item_schema, root, &format!("{path}[{index}]"), depth + 1) {
                return Some(error);
            }
        }
    }

    None
}

/// Validates `value` against `schema`; `path` prefixes error messages.
pub fn validate_json_schema(value: &Value, schema: &Value, path: &str) -> Result<(), String> {
    match validate_value(value, schema, schema, path, 0) {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// Missing arguments are treated as `{}`. Without a schema any object is accepted.
pub fn validate_mcp_tool_arguments(input_schema: Option<&Value>, value: Option<&Value>) -> Result<JsonMap, String> {
    let arguments = match value {
        None => JsonMap::new(),
        Some(Value::Object(arguments)) => arguments.clone(),
        Some(_) => return Err("arguments must be an object".to_string()),
    };

    let default_schema = serde_json::json!({ "type": "object" });
    let schema = input_schema.unwrap_or(&default_schema);
    validate_json_schema(&Value::Object(arguments.clone()), schema, "arguments")?;
    Ok(arguments)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn check(schema: Value, value: Value) -> Result<JsonMap, String> {
        validate_mcp_tool_arguments(Some(&schema), Some(&value))
    }

    #[test]
    fn accepts_missing_arguments_as_an_empty_object() {
        assert_eq!(validate_mcp_tool_arguments(None, None), Ok(JsonMap::new()));
    }

    #[test]
    fn rejects_non_object_arguments() {
        for value in [json!([]), json!("x"), json!(1), Value::Null] {
            assert_eq!(
                validate_mcp_tool_arguments(None, Some(&value)),
                Err("arguments must be an object".to_string())
            );
        }
    }

    #[test]
    fn enforces_required_and_additional_properties() {
        let schema = json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["shop", "query"],
            "properties": { "shop": { "type": "string" }, "query": { "type": "string" } },
        });
        assert_eq!(
            check(schema.clone(), json!({ "shop": "a" })),
            Err("arguments.query is required".to_string())
        );
        assert_eq!(
            check(schema.clone(), json!({ "shop": "a", "query": "q", "extra": 1 })),
            Err("arguments.extra is not allowed".to_string())
        );
        assert!(check(schema, json!({ "shop": "a", "query": "q" })).is_ok());
    }

    #[test]
    fn checks_types_including_unions_and_integers() {
        let schema = json!({
            "type": "object",
            "properties": {
                "limit": { "type": "integer" },
                "tag": { "type": ["string", "null"] },
                "flag": { "type": "boolean" },
            },
        });
        assert_eq!(
            check(schema.clone(), json!({ "limit": 1.5 })),
            Err("arguments.limit must be integer".to_string())
        );
        assert!(check(schema.clone(), json!({ "limit": 2.0 })).is_ok());
        assert_eq!(
            check(schema.clone(), json!({ "tag": 3 })),
            Err("arguments.tag must be string or null".to_string())
        );
        assert!(check(schema.clone(), json!({ "tag": null })).is_ok());
        assert_eq!(
            check(schema, json!({ "flag": "true" })),
            Err("arguments.flag must be boolean".to_string())
        );
    }

    #[test]
    fn checks_numeric_bounds_and_enums() {
        let schema = json!({
            "type": "object",
            "properties": {
                "limit": { "type": "number", "minimum": 1, "maximum": 200 },
                "mode": { "type": "string", "enum": ["a", "b"] },
            },
        });
        assert_eq!(
            check(schema.clone(), json!({ "limit": 0 })),
            Err("arguments.limit must be >= 1".to_string())
        );
        assert_eq!(
            check(schema.clone(), json!({ "limit": 201 })),
            Err("arguments.limit must be <= 200".to_string())
        );
        assert_eq!(
            check(schema, json!({ "mode": "c" })),
            Err("arguments.mode must be one of \"a\", \"b\"".to_string())
        );
    }

    #[test]
    fn validates_nested_objects_and_array_items() {
        let schema = json!({
            "type": "object",
            "properties": {
                "items": { "type": "array", "items": { "type": "object", "required": ["id"], "properties": { "id": { "type": "string" } } } },
            },
        });
        assert_eq!(
            check(schema.clone(), json!({ "items": [{ "id": "a" }, {}] })),
            Err("arguments.items[1].id is required".to_string())
        );
        assert_eq!(
            check(schema, json!({ "items": [{ "id": 1 }] })),
            Err("arguments.items[0].id must be string".to_string())
        );
    }

    #[test]
    fn follows_local_refs_and_any_of() {
        let schema = json!({
            "type": "object",
            "required": ["limit"],
            "properties": {
                "limit": { "$ref": "#/$defs/Limit" },
                "note": { "anyOf": [{ "type": "string" }, { "type": "null" }] },
            },
            "$defs": { "Limit": { "type": "integer", "exclusiveMinimum": 0 } },
        });
        assert_eq!(
            validate_json_schema(&json!({ "limit": 0 }), &schema, "input"),
            Err("input.limit must be > 0".to_string())
        );
        assert_eq!(
            validate_json_schema(&json!({ "limit": 3, "note": 4 }), &schema, "input"),
            Err("input.note must be string".to_string())
        );
        assert_eq!(
            validate_json_schema(&json!({ "limit": 3, "note": null }), &schema, "input"),
            Ok(())
        );
    }

    #[test]
    fn ignores_schemas_that_are_not_objects() {
        assert_eq!(validate_json_schema(&json!(1), &json!(true), "input"), Ok(()));
    }
}
