//! JavaScript-flavoured rendering of JSON values.

use serde_json::Value;

pub use openshop_shared::json::js_string;

/// `JSON.stringify(value, null, 2)`.
pub fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}

/// `JSON.stringify(value)`.
pub fn compact(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

/// `String(value ?? '')`.
pub fn js_string_or_empty(value: &Value) -> String {
    if value.is_null() {
        String::new()
    } else {
        js_string(value)
    }
}

/// `Object.entries(value)`.
pub fn entries(value: &Value) -> Vec<(String, Value)> {
    match value {
        Value::Object(map) => map.iter().map(|(key, value)| (key.clone(), value.clone())).collect(),
        Value::Array(items) => items
            .iter()
            .enumerate()
            .map(|(index, item)| (index.to_string(), item.clone()))
            .collect(),
        Value::String(text) => text
            .chars()
            .enumerate()
            .map(|(index, character)| (index.to_string(), Value::String(character.to_string())))
            .collect(),
        _ => Vec::new(),
    }
}

/// JavaScript truthiness of a JSON value.
pub fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|float| float != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pretty_prints_like_json_stringify() {
        let value = json!({ "limit": 10, "tags": ["a", "b"], "nested": {}, "empty": [] });
        assert_eq!(
            pretty(&value),
            "{\n  \"limit\": 10,\n  \"tags\": [\n    \"a\",\n    \"b\"\n  ],\n  \"nested\": {},\n  \"empty\": []\n}"
        );
        assert_eq!(
            compact(&json!({ "a": [1, "x"], "b": null })),
            r#"{"a":[1,"x"],"b":null}"#
        );
    }

    #[test]
    fn keeps_object_key_order() {
        let value: Value = serde_json::from_str(r#"{"zeta":1,"alpha":2}"#).unwrap();
        assert_eq!(compact(&value), r#"{"zeta":1,"alpha":2}"#);
    }

    #[test]
    fn coerces_values_to_strings() {
        assert_eq!(js_string_or_empty(&json!(null)), "");
        assert_eq!(js_string_or_empty(&json!(false)), "false");
        assert_eq!(js_string_or_empty(&json!(2.0)), "2");
    }

    #[test]
    fn lists_entries() {
        assert_eq!(
            entries(&json!({ "orderId": 1, "ok": true })),
            vec![("orderId".to_string(), json!(1)), ("ok".to_string(), json!(true))]
        );
        assert_eq!(entries(&json!(["x"])), vec![("0".to_string(), json!("x"))]);
        assert!(entries(&json!(5)).is_empty());
    }

    #[test]
    fn evaluates_truthiness() {
        assert!(is_truthy(&json!({})));
        assert!(is_truthy(&json!("x")));
        assert!(!is_truthy(&json!("")));
        assert!(!is_truthy(&json!(0)));
        assert!(!is_truthy(&json!(null)));
    }
}
