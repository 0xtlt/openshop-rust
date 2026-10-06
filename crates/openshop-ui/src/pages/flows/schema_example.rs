//! Builds the example payload pre-filled in the "Run flow" modal from a flow's JSON Schema.

use serde_json::{Map, Number, Value, json};

const MAX_DEPTH: usize = 16;

/// Example input for an object schema, or `None` when it declares no properties.
pub fn schema_to_example(schema: Option<&Value>) -> Option<Map<String, Value>> {
    let root = schema?;
    object_example(root, root, 0)
}

fn resolve<'a>(root: &'a Value, schema: &'a Value, depth: usize) -> Option<&'a Value> {
    if depth > MAX_DEPTH {
        return None;
    }
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        let target = root.pointer(reference.strip_prefix('#')?)?;
        return resolve(root, target, depth + 1);
    }
    let variants = schema
        .get("anyOf")
        .or_else(|| schema.get("oneOf"))
        .and_then(Value::as_array);
    match variants {
        Some(variants) => variants
            .iter()
            .filter_map(|variant| resolve(root, variant, depth + 1))
            .find(|variant| type_name(variant) != Some("null")),
        None => Some(schema),
    }
}

fn type_name(schema: &Value) -> Option<&str> {
    match schema.get("type")? {
        Value::String(name) => Some(name),
        Value::Array(names) => names.iter().filter_map(Value::as_str).find(|name| *name != "null"),
        _ => None,
    }
}

fn object_example(root: &Value, schema: &Value, depth: usize) -> Option<Map<String, Value>> {
    let schema = resolve(root, schema, depth)?;
    let properties = schema.get("properties")?.as_object()?;
    let example: Map<String, Value> = properties
        .iter()
        .map(|(key, property)| (key.clone(), value_example(root, property, depth + 1)))
        .collect();
    (!example.is_empty()).then_some(example)
}

fn number_example(schema: &Value) -> Value {
    let bound = |keyword: &str| schema.get(keyword).filter(|value| value.is_number());
    if let Some(minimum) = bound("minimum") {
        return minimum.clone();
    }
    match bound("exclusiveMinimum") {
        Some(exclusive) => match exclusive.as_i64() {
            Some(integer) => json!(integer + 1),
            None => Number::from_f64(exclusive.as_f64().unwrap_or(0.0) + 1.0).map_or(json!(0), Value::Number),
        },
        None => json!(0),
    }
}

fn value_example(root: &Value, schema: &Value, depth: usize) -> Value {
    if let Some(default) = schema.get("default") {
        return default.clone();
    }
    let Some(schema) = resolve(root, schema, depth) else {
        return Value::Null;
    };
    if let Some(value) = schema.get("default").or_else(|| schema.get("const")) {
        return value.clone();
    }
    if let Some(first) = schema
        .get("enum")
        .and_then(Value::as_array)
        .and_then(|values| values.first())
    {
        return first.clone();
    }
    match type_name(schema) {
        Some("number" | "integer") => number_example(schema),
        Some("string") => json!(""),
        Some("boolean") => json!(false),
        Some("array") => json!([]),
        Some("object") => Value::Object(object_example(root, schema, depth).unwrap_or_default()),
        _ if schema.get("properties").is_some() => {
            Value::Object(object_example(root, schema, depth).unwrap_or_default())
        }
        _ => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example(schema: Value) -> Option<Value> {
        schema_to_example(Some(&schema)).map(Value::Object)
    }

    #[test]
    fn returns_none_without_properties() {
        assert_eq!(schema_to_example(None), None);
        assert_eq!(example(json!(null)), None);
        assert_eq!(example(json!({ "type": "object" })), None);
        assert_eq!(example(json!({ "type": "object", "properties": {} })), None);
        assert_eq!(example(json!({ "type": "string" })), None);
        assert_eq!(example(json!(true)), None);
    }

    #[test]
    fn fills_scalars_by_type() {
        let schema = json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": "SyncInput",
            "type": "object",
            "properties": {
                "limit": { "type": "integer", "format": "uint32", "minimum": 1 },
                "ratio": { "type": "number", "exclusiveMinimum": 0.5 },
                "after": { "type": "integer", "exclusiveMinimum": 10 },
                "offset": { "type": "integer" },
                "cursor": { "type": "string" },
                "dryRun": { "type": "boolean" },
                "tags": { "type": "array", "items": { "type": "string" } }
            },
            "required": ["limit"]
        });
        assert_eq!(
            example(schema),
            Some(json!({
                "limit": 1,
                "ratio": 1.5,
                "after": 11,
                "offset": 0,
                "cursor": "",
                "dryRun": false,
                "tags": []
            }))
        );
    }

    #[test]
    fn prefers_default_then_const_then_enum() {
        let schema = json!({
            "type": "object",
            "properties": {
                "mode": { "type": "string", "enum": ["full", "delta"] },
                "kind": { "const": "orders" },
                "pageSize": { "type": "integer", "minimum": 1, "default": 50 },
                "note": { "type": ["string", "null"], "default": null }
            }
        });
        assert_eq!(
            example(schema),
            Some(json!({ "mode": "full", "kind": "orders", "pageSize": 50, "note": null }))
        );
    }

    #[test]
    fn resolves_refs_options_and_nested_objects() {
        let schema = json!({
            "type": "object",
            "properties": {
                "window": { "$ref": "#/$defs/Window" },
                "shop": { "anyOf": [{ "$ref": "#/$defs/Shop" }, { "type": "null" }] },
                "label": { "type": ["string", "null"] },
                "meta": { "type": "object" }
            },
            "$defs": {
                "Window": {
                    "type": "object",
                    "properties": {
                        "days": { "type": "integer", "minimum": 0 },
                        "status": { "$ref": "#/$defs/Status" }
                    }
                },
                "Status": { "type": "string", "enum": ["open", "closed"] },
                "Shop": { "type": "object", "properties": { "domain": { "type": "string" } } }
            }
        });
        assert_eq!(
            example(schema),
            Some(json!({
                "window": { "days": 0, "status": "open" },
                "shop": { "domain": "" },
                "label": "",
                "meta": {}
            }))
        );
    }

    #[test]
    fn resolves_a_root_reference_and_keeps_property_order() {
        let schema: Value = serde_json::from_str(
            r##"{
                "$ref": "#/$defs/Input",
                "$defs": { "Input": { "type": "object", "properties": {
                    "zeta": { "type": "string" }, "alpha": { "type": "boolean" } } } }
            }"##,
        )
        .unwrap();
        let example = schema_to_example(Some(&schema)).unwrap();
        assert_eq!(example.keys().collect::<Vec<_>>(), ["zeta", "alpha"]);
    }

    #[test]
    fn survives_unknown_and_cyclic_schemas() {
        let schema = json!({
            "type": "object",
            "properties": {
                "anything": {},
                "missing": { "$ref": "#/$defs/Nope" },
                "node": { "$ref": "#/$defs/Node" }
            },
            "$defs": {
                "Node": { "type": "object", "properties": { "next": { "$ref": "#/$defs/Node" } } }
            }
        });
        let example = schema_to_example(Some(&schema)).unwrap();
        assert_eq!(example["anything"], Value::Null);
        assert_eq!(example["missing"], Value::Null);
        assert!(example["node"].is_object());
    }
}
