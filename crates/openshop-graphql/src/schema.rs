//! The part of a GraphQL schema that checking and typing an operation needs:
//! type kinds, fields, arguments and enum values, without descriptions.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The introspection query whose result [`Schema::from_introspection`] reads.
pub const INTROSPECTION_QUERY: &str = "\
query IntrospectionQuery {
  __schema {
    queryType { name }
    mutationType { name }
    types {
      kind
      name
      fields(includeDeprecated: true) {
        name
        args { name type { ...TypeRef } defaultValue }
        type { ...TypeRef }
      }
      inputFields { name type { ...TypeRef } defaultValue }
      interfaces { name }
      enumValues(includeDeprecated: true) { name }
      possibleTypes { name }
    }
  }
}
fragment TypeRef on __Type {
  kind name
  ofType { kind name ofType { kind name ofType { kind name ofType { kind name ofType { kind name ofType { kind name } } } } } }
}
";

/// Shopify's Admin GraphQL schema for the API version the framework uses by default.
const BUNDLED_SCHEMA: &str = include_str!("../schema/admin-2026-04.schema.json");

/// A reference to a type, as written in GraphQL: `[Order!]!`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeRef {
    Named(String),
    List(Box<TypeRef>),
    NonNull(Box<TypeRef>),
}

impl TypeRef {
    /// Parses `[Order!]!`. Returns `None` for malformed input.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        if let Some(inner) = text.strip_suffix('!') {
            return Some(Self::NonNull(Box::new(Self::parse(inner)?)));
        }
        if let Some(inner) = text.strip_prefix('[') {
            return Some(Self::List(Box::new(Self::parse(inner.strip_suffix(']')?)?)));
        }
        let is_name = !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
        is_name.then(|| Self::Named(text.to_string()))
    }

    /// The innermost named type: `Order` for `[Order!]!`.
    pub fn name(&self) -> &str {
        match self {
            Self::Named(name) => name,
            Self::List(inner) | Self::NonNull(inner) => inner.name(),
        }
    }

    pub fn is_non_null(&self) -> bool {
        matches!(self, Self::NonNull(_))
    }

    /// The same type without its outer `!`.
    pub fn nullable(&self) -> &TypeRef {
        match self {
            Self::NonNull(inner) => inner,
            other => other,
        }
    }
}

impl fmt::Display for TypeRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Named(name) => formatter.write_str(name),
            Self::List(inner) => write!(formatter, "[{inner}]"),
            Self::NonNull(inner) => write!(formatter, "{inner}!"),
        }
    }
}

/// An argument or input field: its type, and whether the schema gives it a default value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputValue {
    pub ty: TypeRef,
    pub has_default: bool,
}

impl InputValue {
    /// A value must be provided: the type is non-null and has no default.
    pub fn is_required(&self) -> bool {
        self.ty.is_non_null() && !self.has_default
    }

    /// Reads the stored form: the type, followed by `=` when there is a default.
    fn parse(text: &str) -> Option<Self> {
        let (ty, has_default) = match text.strip_suffix('=') {
            Some(ty) => (ty, true),
            None => (text, false),
        };
        Some(Self {
            ty: TypeRef::parse(ty)?,
            has_default,
        })
    }
}

/// A field of an object or interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub ty: TypeRef,
    pub args: BTreeMap<String, InputValue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeDef {
    Scalar,
    Enum {
        values: Vec<String>,
    },
    Object {
        fields: BTreeMap<String, Field>,
        interfaces: Vec<String>,
    },
    Interface {
        fields: BTreeMap<String, Field>,
        possible_types: Vec<String>,
    },
    Union {
        possible_types: Vec<String>,
    },
    Input {
        fields: BTreeMap<String, InputValue>,
    },
}

impl TypeDef {
    /// The kind of type, with its article, for error messages.
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::Scalar => "a scalar",
            Self::Enum { .. } => "an enum",
            Self::Object { .. } => "an object",
            Self::Interface { .. } => "an interface",
            Self::Union { .. } => "a union",
            Self::Input { .. } => "an input object",
        }
    }

    /// Objects, interfaces and unions: types that need a selection of subfields.
    pub fn is_composite(&self) -> bool {
        matches!(self, Self::Object { .. } | Self::Interface { .. } | Self::Union { .. })
    }

    /// Scalars, enums and input objects: types a variable can have.
    pub fn is_input(&self) -> bool {
        matches!(self, Self::Scalar | Self::Enum { .. } | Self::Input { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schema {
    /// The Shopify API version the schema was downloaded for, such as `2026-04`.
    pub api_version: String,
    pub query: String,
    pub mutation: Option<String>,
    pub types: BTreeMap<String, TypeDef>,
}

/// A schema file is malformed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaError(pub String);

impl fmt::Display for SchemaError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for SchemaError {}

fn schema_error(message: impl Into<String>) -> SchemaError {
    SchemaError(message.into())
}

// ─── Stored form ─────────────────────────────────────────────────────
//
// One JSON object per type, with one-letter keys: the Admin schema has
// thousands of types and is parsed every time a crate using it is compiled.

#[derive(Serialize, Deserialize)]
struct StoredSchema {
    #[serde(rename = "apiVersion")]
    api_version: String,
    query: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mutation: Option<String>,
    types: BTreeMap<String, StoredType>,
}

#[derive(Serialize, Deserialize)]
enum StoredType {
    #[serde(rename = "s")]
    Scalar,
    #[serde(rename = "e")]
    Enum(Vec<String>),
    #[serde(rename = "o")]
    Object(StoredComposite),
    #[serde(rename = "n")]
    Interface(StoredComposite),
    #[serde(rename = "u")]
    Union(Vec<String>),
    #[serde(rename = "i")]
    Input(BTreeMap<String, String>),
}

#[derive(Serialize, Deserialize)]
struct StoredComposite {
    #[serde(rename = "f")]
    fields: BTreeMap<String, StoredField>,
    /// Interfaces of an object, possible types of an interface.
    #[serde(rename = "t", default, skip_serializing_if = "Vec::is_empty")]
    types: Vec<String>,
}

/// `"ID!"`, or `["OrderConnection!", { "first": "Int", "reverse": "Boolean=" }]`.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum StoredField {
    Plain(String),
    WithArgs(String, BTreeMap<String, String>),
}

fn store_input(value: &InputValue) -> String {
    if value.has_default {
        format!("{}=", value.ty)
    } else {
        value.ty.to_string()
    }
}

fn load_input(owner: &str, name: &str, text: &str) -> Result<InputValue, SchemaError> {
    InputValue::parse(text).ok_or_else(|| schema_error(format!("Invalid type \"{text}\" for {owner}.{name}")))
}

fn store_fields(fields: &BTreeMap<String, Field>) -> BTreeMap<String, StoredField> {
    fields
        .iter()
        .map(|(name, field)| {
            let ty = field.ty.to_string();
            let stored = if field.args.is_empty() {
                StoredField::Plain(ty)
            } else {
                let args = field
                    .args
                    .iter()
                    .map(|(name, arg)| (name.clone(), store_input(arg)))
                    .collect();
                StoredField::WithArgs(ty, args)
            };
            (name.clone(), stored)
        })
        .collect()
}

fn load_fields(owner: &str, fields: BTreeMap<String, StoredField>) -> Result<BTreeMap<String, Field>, SchemaError> {
    fields
        .into_iter()
        .map(|(name, stored)| {
            let (ty, args) = match stored {
                StoredField::Plain(ty) => (ty, BTreeMap::new()),
                StoredField::WithArgs(ty, args) => (ty, args),
            };
            let field = Field {
                ty: TypeRef::parse(&ty)
                    .ok_or_else(|| schema_error(format!("Invalid type \"{ty}\" for {owner}.{name}")))?,
                args: args
                    .into_iter()
                    .map(|(arg, text)| {
                        let value = load_input(&format!("{owner}.{name}"), &arg, &text)?;
                        Ok((arg, value))
                    })
                    .collect::<Result<_, SchemaError>>()?,
            };
            Ok((name, field))
        })
        .collect()
}

impl Schema {
    /// The Shopify Admin schema shipped with the framework.
    pub fn bundled() -> &'static Schema {
        static BUNDLED: OnceLock<Schema> = OnceLock::new();
        BUNDLED.get_or_init(|| Schema::from_json(BUNDLED_SCHEMA).expect("the bundled schema is valid"))
    }

    /// Reads the form written by [`Schema::to_json`].
    pub fn from_json(json: &str) -> Result<Self, SchemaError> {
        let stored: StoredSchema =
            serde_json::from_str(json).map_err(|error| schema_error(format!("Invalid schema file: {error}")))?;
        let types = stored
            .types
            .into_iter()
            .map(|(name, stored)| {
                let definition = match stored {
                    StoredType::Scalar => TypeDef::Scalar,
                    StoredType::Enum(values) => TypeDef::Enum { values },
                    StoredType::Object(object) => TypeDef::Object {
                        fields: load_fields(&name, object.fields)?,
                        interfaces: object.types,
                    },
                    StoredType::Interface(interface) => TypeDef::Interface {
                        fields: load_fields(&name, interface.fields)?,
                        possible_types: interface.types,
                    },
                    StoredType::Union(possible_types) => TypeDef::Union { possible_types },
                    StoredType::Input(fields) => TypeDef::Input {
                        fields: fields
                            .into_iter()
                            .map(|(field, text)| {
                                let value = load_input(&name, &field, &text)?;
                                Ok((field, value))
                            })
                            .collect::<Result<_, SchemaError>>()?,
                    },
                };
                Ok((name, definition))
            })
            .collect::<Result<_, SchemaError>>()?;
        Ok(Self {
            api_version: stored.api_version,
            query: stored.query,
            mutation: stored.mutation,
            types,
        })
    }

    /// One type per line, so that a new API version produces a readable diff.
    pub fn to_json(&self) -> String {
        let types: BTreeMap<String, StoredType> = self
            .types
            .iter()
            .map(|(name, definition)| {
                let stored = match definition {
                    TypeDef::Scalar => StoredType::Scalar,
                    TypeDef::Enum { values } => StoredType::Enum(values.clone()),
                    TypeDef::Object { fields, interfaces } => StoredType::Object(StoredComposite {
                        fields: store_fields(fields),
                        types: interfaces.clone(),
                    }),
                    TypeDef::Interface { fields, possible_types } => StoredType::Interface(StoredComposite {
                        fields: store_fields(fields),
                        types: possible_types.clone(),
                    }),
                    TypeDef::Union { possible_types } => StoredType::Union(possible_types.clone()),
                    TypeDef::Input { fields } => StoredType::Input(
                        fields
                            .iter()
                            .map(|(name, value)| (name.clone(), store_input(value)))
                            .collect(),
                    ),
                };
                (name.clone(), stored)
            })
            .collect();

        let quoted = |text: &str| Value::from(text).to_string();
        let mut json = format!(
            "{{\"apiVersion\":{},\"query\":{}",
            quoted(&self.api_version),
            quoted(&self.query)
        );
        if let Some(mutation) = &self.mutation {
            json.push_str(&format!(",\"mutation\":{}", quoted(mutation)));
        }
        json.push_str(",\"types\":{");
        for (index, (name, stored)) in types.iter().enumerate() {
            json.push_str(if index == 0 { "\n" } else { ",\n" });
            let stored = serde_json::to_string(stored).expect("schema types serialize");
            json.push_str(&format!("{}:{stored}", quoted(name)));
        }
        json.push_str("\n}}\n");
        json
    }

    /// Builds a schema from the answer to [`INTROSPECTION_QUERY`]: the whole
    /// response, its `data`, or the `__schema` object.
    pub fn from_introspection(response: &Value, api_version: &str) -> Result<Self, SchemaError> {
        if let Some(errors) = response.get("errors").and_then(Value::as_array)
            && !errors.is_empty()
        {
            let messages: Vec<&str> = errors
                .iter()
                .filter_map(|error| error.get("message").and_then(Value::as_str))
                .collect();
            return Err(schema_error(format!("Introspection failed: {}", messages.join("; "))));
        }
        let schema = response
            .pointer("/data/__schema")
            .or_else(|| response.get("__schema"))
            .unwrap_or(response);
        let name_of = |pointer: &str| schema.pointer(pointer).and_then(Value::as_str).map(str::to_string);
        let query =
            name_of("/queryType/name").ok_or_else(|| schema_error("The introspection result has no `queryType`"))?;
        let listed = schema
            .get("types")
            .and_then(Value::as_array)
            .ok_or_else(|| schema_error("The introspection result has no `types`"))?;

        let mut types = BTreeMap::new();
        for entry in listed {
            let name = text(entry, "name")?;
            if name.starts_with("__") {
                continue;
            }
            let names = |key: &str| -> Vec<String> {
                let listed = entry.get(key).and_then(Value::as_array);
                listed
                    .into_iter()
                    .flatten()
                    .filter_map(|item| item.get("name").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect()
            };
            let definition = match text(entry, "kind")? {
                "SCALAR" => TypeDef::Scalar,
                "ENUM" => TypeDef::Enum {
                    values: names("enumValues"),
                },
                "OBJECT" => TypeDef::Object {
                    fields: introspected_fields(entry)?,
                    interfaces: names("interfaces"),
                },
                "INTERFACE" => TypeDef::Interface {
                    fields: introspected_fields(entry)?,
                    possible_types: names("possibleTypes"),
                },
                "UNION" => TypeDef::Union {
                    possible_types: names("possibleTypes"),
                },
                "INPUT_OBJECT" => TypeDef::Input {
                    fields: introspected_inputs(entry.get("inputFields"))?,
                },
                other => return Err(schema_error(format!("Unknown kind \"{other}\" for type {name}"))),
            };
            types.insert(name.to_string(), definition);
        }

        Ok(Self {
            api_version: api_version.to_string(),
            query,
            mutation: name_of("/mutationType/name"),
            types,
        })
    }

    pub fn get(&self, name: &str) -> Option<&TypeDef> {
        self.types.get(name)
    }

    /// The object types a value of `name` can be at run time.
    pub fn possible_types(&self, name: &str) -> Vec<&str> {
        match self.types.get_key_value(name) {
            Some((name, TypeDef::Object { .. })) => vec![name.as_str()],
            Some((_, TypeDef::Interface { possible_types, .. } | TypeDef::Union { possible_types })) => {
                possible_types.iter().map(String::as_str).collect()
            }
            _ => Vec::new(),
        }
    }
}

fn text<'a>(entry: &'a Value, key: &str) -> Result<&'a str, SchemaError> {
    entry
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| schema_error(format!("An introspected entry has no `{key}`")))
}

fn introspected_type(value: Option<&Value>) -> Result<TypeRef, SchemaError> {
    let value = value.ok_or_else(|| schema_error("An introspected entry has no `type`"))?;
    let inner = || introspected_type(value.get("ofType").filter(|inner| !inner.is_null()));
    Ok(match text(value, "kind")? {
        "NON_NULL" => TypeRef::NonNull(Box::new(inner()?)),
        "LIST" => TypeRef::List(Box::new(inner()?)),
        _ => TypeRef::Named(text(value, "name")?.to_string()),
    })
}

fn introspected_inputs(values: Option<&Value>) -> Result<BTreeMap<String, InputValue>, SchemaError> {
    values
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|value| {
            let input = InputValue {
                ty: introspected_type(value.get("type"))?,
                has_default: value.get("defaultValue").is_some_and(|default| !default.is_null()),
            };
            Ok((text(value, "name")?.to_string(), input))
        })
        .collect()
}

fn introspected_fields(entry: &Value) -> Result<BTreeMap<String, Field>, SchemaError> {
    entry
        .get("fields")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|field| {
            let definition = Field {
                ty: introspected_type(field.get("type"))?,
                args: introspected_inputs(field.get("args"))?,
            };
            Ok((text(field, "name")?.to_string(), definition))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_and_prints_type_references() {
        for text in ["Order", "Order!", "[Order]", "[Order!]!", "[[Int!]]!"] {
            assert_eq!(TypeRef::parse(text).unwrap().to_string(), text);
        }
        let list = TypeRef::parse("[Order!]!").unwrap();
        assert_eq!(list.name(), "Order");
        assert!(list.is_non_null());
        assert_eq!(list.nullable().to_string(), "[Order!]");
        for invalid in ["", "!", "[Order", "Order]", "Or der", "[]"] {
            assert_eq!(TypeRef::parse(invalid), None, "{invalid}");
        }
    }

    fn introspection() -> Value {
        let named = |kind: &str, name: &str| json!({ "kind": kind, "name": name, "ofType": null });
        let non_null = |inner: Value| json!({ "kind": "NON_NULL", "name": null, "ofType": inner });
        let list = |inner: Value| json!({ "kind": "LIST", "name": null, "ofType": inner });
        json!({ "data": { "__schema": {
            "queryType": { "name": "QueryRoot" },
            "mutationType": { "name": "Mutation" },
            "types": [
                { "kind": "OBJECT", "name": "QueryRoot", "interfaces": [], "fields": [{
                    "name": "orders",
                    "type": non_null(list(non_null(named("OBJECT", "Order")))),
                    "args": [
                        { "name": "first", "type": named("SCALAR", "Int"), "defaultValue": null },
                        { "name": "reverse", "type": named("SCALAR", "Boolean"), "defaultValue": "false" },
                    ],
                }] },
                { "kind": "OBJECT", "name": "Mutation", "interfaces": [], "fields": [] },
                { "kind": "OBJECT", "name": "Order", "interfaces": [{ "name": "Node" }], "fields": [
                    { "name": "id", "type": non_null(named("SCALAR", "ID")), "args": [] },
                ] },
                { "kind": "INTERFACE", "name": "Node", "possibleTypes": [{ "name": "Order" }], "fields": [
                    { "name": "id", "type": non_null(named("SCALAR", "ID")), "args": [] },
                ] },
                { "kind": "UNION", "name": "Resource", "possibleTypes": [{ "name": "Order" }] },
                { "kind": "ENUM", "name": "SortKey", "enumValues": [{ "name": "ID" }, { "name": "CREATED_AT" }] },
                { "kind": "INPUT_OBJECT", "name": "OrderInput", "inputFields": [
                    { "name": "id", "type": non_null(named("SCALAR", "ID")), "defaultValue": null },
                    { "name": "tags", "type": list(non_null(named("SCALAR", "String"))), "defaultValue": "[]" },
                ] },
                { "kind": "SCALAR", "name": "ID" },
                { "kind": "OBJECT", "name": "__Type", "interfaces": [], "fields": [] },
            ],
        } } })
    }

    #[test]
    fn reads_an_introspection_result() {
        let schema = Schema::from_introspection(&introspection(), "2026-04").unwrap();

        assert_eq!(schema.api_version, "2026-04");
        assert_eq!(schema.query, "QueryRoot");
        assert_eq!(schema.mutation.as_deref(), Some("Mutation"));
        assert!(schema.get("__Type").is_none());

        let Some(TypeDef::Object { fields, .. }) = schema.get("QueryRoot") else {
            panic!("QueryRoot is an object");
        };
        let orders = &fields["orders"];
        assert_eq!(orders.ty.to_string(), "[Order!]!");
        assert!(!orders.args["first"].has_default);
        assert!(orders.args["reverse"].has_default);

        let Some(TypeDef::Input { fields }) = schema.get("OrderInput") else {
            panic!("OrderInput is an input object");
        };
        assert!(fields["id"].is_required());
        assert!(!fields["tags"].is_required());

        assert_eq!(schema.possible_types("Node"), ["Order"]);
        assert_eq!(schema.possible_types("Resource"), ["Order"]);
        assert_eq!(schema.possible_types("Order"), ["Order"]);
        assert!(schema.possible_types("SortKey").is_empty());
        assert_eq!(
            schema.get("SortKey"),
            Some(&TypeDef::Enum {
                values: vec!["ID".to_string(), "CREATED_AT".to_string()]
            })
        );
    }

    #[test]
    fn the_stored_form_round_trips() {
        let schema = Schema::from_introspection(&introspection(), "2026-04").unwrap();
        let json = schema.to_json();

        assert_eq!(Schema::from_json(&json).unwrap(), schema);
        // One type per line, in name order.
        assert!(json.contains("\n\"Order\":{\"o\":{\"f\":{\"id\":\"ID!\"},\"t\":[\"Node\"]}},\n"));
        assert!(json.contains("\"orders\":[\"[Order!]!\",{\"first\":\"Int\",\"reverse\":\"Boolean=\"}]"));
        assert!(json.contains("\n\"OrderInput\":{\"i\":{\"id\":\"ID!\",\"tags\":\"[String!]=\"}},\n"));
        assert!(json.contains("\n\"ID\":\"s\",\n"));
    }

    #[test]
    fn reports_malformed_input() {
        assert_eq!(
            Schema::from_introspection(&json!({ "errors": [{ "message": "Throttled" }] }), "2026-04"),
            Err(SchemaError("Introspection failed: Throttled".to_string()))
        );
        assert_eq!(
            Schema::from_introspection(&json!({ "data": { "__schema": {} } }), "2026-04"),
            Err(SchemaError("The introspection result has no `queryType`".to_string()))
        );
        assert!(
            Schema::from_json("{")
                .unwrap_err()
                .0
                .starts_with("Invalid schema file: ")
        );
        assert_eq!(
            Schema::from_json(r#"{"apiVersion":"2026-04","query":"Q","types":{"Q":{"o":{"f":{"a":"[Int"}}}}}"#),
            Err(SchemaError("Invalid type \"[Int\" for Q.a".to_string()))
        );
    }

    #[test]
    fn the_bundled_schema_is_the_admin_api() {
        let schema = Schema::bundled();
        assert_eq!(schema.api_version, "2026-04");
        assert_eq!(schema.query, "QueryRoot");
        assert_eq!(schema.mutation.as_deref(), Some("Mutation"));
    }
}
