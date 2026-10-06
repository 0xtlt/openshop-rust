//! Typed GraphQL operations, as built by the [`graphql!`](crate::graphql) macro.

use std::borrow::Cow;
use std::marker::PhantomData;

use anyhow::{Result, anyhow};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::client::ShopifyClient;
use crate::types::JsonMap;

/// A GraphQL operation, its variables, and the type `D` of its `data`.
///
/// [`graphql!`](crate::graphql) builds it after checking the operation against
/// Shopify's schema; [`ShopifyClient::execute`] runs it.
pub struct GraphqlOperation<D> {
    query: &'static str,
    variables: Result<JsonMap, String>,
    data: PhantomData<fn() -> D>,
}

impl<D> GraphqlOperation<D> {
    pub fn new(query: &'static str) -> Self {
        Self {
            query,
            variables: Ok(JsonMap::new()),
            data: PhantomData,
        }
    }

    /// The text of the operation.
    pub fn query(&self) -> &'static str {
        self.query
    }

    fn set(mut self, name: &str, value: serde_json::Result<Value>) -> Self {
        match (&mut self.variables, value) {
            (Ok(variables), Ok(value)) => {
                variables.insert(name.to_string(), value);
            }
            (Ok(_), Err(error)) => self.variables = Err(format!("Invalid value for variable `${name}`: {error}")),
            (Err(_), _) => {}
        }
        self
    }

    /// Sets a variable to a value convertible to `T`, the Rust type of its
    /// GraphQL type.
    pub fn variable<T: Serialize, V: IntoVariable<T>>(self, name: &str, value: V) -> Self {
        self.set(name, serde_json::to_value(value.into_variable()))
    }

    /// Sets a variable to any serializable value, without checking its type.
    pub fn json_variable<V: Serialize>(self, name: &str, value: V) -> Self {
        self.set(name, serde_json::to_value(value))
    }

    /// Replaces the variables with the fields of a serializable struct.
    pub fn variables<V: Serialize>(mut self, variables: V) -> Self {
        self.variables = match serde_json::to_value(variables) {
            Ok(Value::Object(variables)) => Ok(variables),
            Ok(Value::Null) => Ok(JsonMap::new()),
            Ok(_) => Err("The variables of an operation must serialize to an object".to_string()),
            Err(error) => Err(format!("Invalid variables: {error}")),
        };
        self
    }

    /// The text of the operation and its variables.
    pub fn into_parts(self) -> Result<(&'static str, JsonMap)> {
        match self.variables {
            Ok(variables) => Ok((self.query, variables)),
            Err(message) => Err(anyhow!("[openshop] {message}")),
        }
    }
}

impl<D> Clone for GraphqlOperation<D> {
    fn clone(&self) -> Self {
        Self {
            query: self.query,
            variables: self.variables.clone(),
            data: PhantomData,
        }
    }
}

impl<D> std::fmt::Debug for GraphqlOperation<D> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GraphqlOperation")
            .field("query", &self.query)
            .field("variables", &self.variables)
            .finish()
    }
}

impl ShopifyClient {
    /// Runs a typed operation and returns its `data`.
    ///
    /// Fails like [`ShopifyClient::graphql`], and when Shopify answers with
    /// data that does not fit the operation.
    pub async fn execute<D: DeserializeOwned>(&self, operation: GraphqlOperation<D>) -> Result<D> {
        let (query, variables) = operation.into_parts()?;
        let variables = (!variables.is_empty()).then_some(Value::Object(variables));
        let data = self.graphql(query, variables).await?;
        serde_path_to_error::deserialize(data).map_err(|error| {
            anyhow!(
                "[openshop] Shopify's answer does not match the operation at `{}`: {}",
                error.path(),
                error.inner()
            )
        })
    }
}

/// A Rust value accepted for a GraphQL variable whose Rust type is `T`.
///
/// `graphql!` picks `T` from the type of the variable: `String` for strings,
/// IDs, enum values and Shopify's string-encoded scalars, `i64` for `Int`,
/// `f64` for `Float`, `bool` for `Boolean`, `Value` for `JSON`, wrapped in
/// `Option` when the variable is nullable and in `Vec` for lists. The
/// implementations then accept the usual Rust spellings of those values:
/// `&str` for a string, any integer for an `Int`, a bare value for a nullable
/// variable, a slice for a list.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a value for a GraphQL variable of Rust type `{T}`",
    label = "the variable expects a value convertible to `{T}`"
)]
pub trait IntoVariable<T> {
    fn into_variable(self) -> T;
}

macro_rules! into_variable {
    ($target:ty: $($source:ty => |$value:ident| $convert:expr),+ $(,)?) => {$(
        impl IntoVariable<$target> for $source {
            fn into_variable(self) -> $target {
                let $value = self;
                $convert
            }
        }

        impl IntoVariable<Option<$target>> for $source {
            fn into_variable(self) -> Option<$target> {
                Some(IntoVariable::<$target>::into_variable(self))
            }
        }
    )+};
}

into_variable!(String:
    String => |value| value,
    &str => |value| value.to_string(),
    &String => |value| value.clone(),
    Cow<'_, str> => |value| value.into_owned(),
);
into_variable!(i64:
    i8 => |value| value.into(),
    i16 => |value| value.into(),
    i32 => |value| value.into(),
    i64 => |value| value,
    u8 => |value| value.into(),
    u16 => |value| value.into(),
    u32 => |value| value.into(),
    // Larger than any GraphQL `Int`: Shopify rejects the saturated value.
    u64 => |value| i64::try_from(value).unwrap_or(i64::MAX),
    usize => |value| i64::try_from(value).unwrap_or(i64::MAX),
    isize => |value| i64::try_from(value).unwrap_or(if value < 0 { i64::MIN } else { i64::MAX }),
);
into_variable!(f64:
    f32 => |value| value.into(),
    f64 => |value| value,
    i32 => |value| value.into(),
    u32 => |value| value.into(),
);
into_variable!(bool: bool => |value| value);
into_variable!(Value:
    Value => |value| value,
    &Value => |value| value.clone(),
);

impl<T, V: IntoVariable<T>> IntoVariable<Option<T>> for Option<V> {
    fn into_variable(self) -> Option<T> {
        self.map(IntoVariable::into_variable)
    }
}

impl<T, V: IntoVariable<T>> IntoVariable<Vec<T>> for Vec<V> {
    fn into_variable(self) -> Vec<T> {
        self.into_iter().map(IntoVariable::into_variable).collect()
    }
}

impl<T, V: IntoVariable<T> + Clone> IntoVariable<Vec<T>> for &[V] {
    fn into_variable(self) -> Vec<T> {
        self.iter().cloned().map(IntoVariable::into_variable).collect()
    }
}

impl<T, V: IntoVariable<T> + Clone> IntoVariable<Vec<T>> for &Vec<V> {
    fn into_variable(self) -> Vec<T> {
        self.as_slice().into_variable()
    }
}

impl<T, V: IntoVariable<T>, const N: usize> IntoVariable<Vec<T>> for [V; N] {
    fn into_variable(self) -> Vec<T> {
        self.into_iter().map(IntoVariable::into_variable).collect()
    }
}

impl<T, V: IntoVariable<T>> IntoVariable<Option<Vec<T>>> for Vec<V> {
    fn into_variable(self) -> Option<Vec<T>> {
        Some(self.into_variable())
    }
}

impl<T, V: IntoVariable<T> + Clone> IntoVariable<Option<Vec<T>>> for &[V] {
    fn into_variable(self) -> Option<Vec<T>> {
        Some(self.into_variable())
    }
}

impl<T, V: IntoVariable<T> + Clone> IntoVariable<Option<Vec<T>>> for &Vec<V> {
    fn into_variable(self) -> Option<Vec<T>> {
        Some(self.into_variable())
    }
}

impl<T, V: IntoVariable<T>, const N: usize> IntoVariable<Option<Vec<T>>> for [V; N] {
    fn into_variable(self) -> Option<Vec<T>> {
        Some(self.into_variable())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn variables<D>(operation: GraphqlOperation<D>) -> Value {
        Value::Object(operation.into_parts().unwrap().1)
    }

    #[test]
    fn converts_the_usual_spellings_of_a_value() {
        let operation = GraphqlOperation::<()>::new("query { shop { id } }")
            .variable::<String, _>("id", "gid://shopify/Order/1")
            .variable::<Option<String>, _>("query", "status:open")
            .variable::<Option<String>, _>("after", None::<String>)
            .variable::<i64, _>("first", 10_u32)
            .variable::<i64, _>("count", 3_usize)
            .variable::<Option<i64>, _>("last", Some(2))
            .variable::<f64, _>("ratio", 1.5_f32)
            .variable::<bool, _>("reverse", true)
            .variable::<Vec<String>, _>("ids", ["a", "b"])
            .variable::<Vec<String>, _>("tags", &["x".to_string()][..])
            .variable::<Option<Vec<i64>>, _>("quantities", vec![1_u8, 2])
            .variable::<Vec<Option<String>>, _>("holes", vec![Some("a"), None])
            .variable::<Value, _>("payload", json!({ "a": 1 }))
            .json_variable("input", json!({ "title": "Shirt" }));

        assert_eq!(operation.query(), "query { shop { id } }");
        assert_eq!(
            variables(operation),
            json!({
                "id": "gid://shopify/Order/1",
                "query": "status:open",
                "after": null,
                "first": 10,
                "count": 3,
                "last": 2,
                "ratio": 1.5,
                "reverse": true,
                "ids": ["a", "b"],
                "tags": ["x"],
                "quantities": [1, 2],
                "holes": ["a", null],
                "payload": { "a": 1 },
                "input": { "title": "Shirt" },
            })
        );
    }

    #[test]
    fn integers_that_do_not_fit_are_saturated() {
        let operation = GraphqlOperation::<()>::new("query { shop { id } }")
            .variable::<i64, _>("big", u64::MAX)
            .variable::<i64, _>("fits", 7_u64);
        assert_eq!(variables(operation), json!({ "big": i64::MAX, "fits": 7 }));
    }

    #[test]
    fn variables_can_come_from_a_struct() {
        #[derive(Serialize)]
        struct Variables {
            first: i64,
            #[serde(skip_serializing_if = "Option::is_none")]
            after: Option<String>,
        }
        let operation = GraphqlOperation::<()>::new("query { shop { id } }")
            .variable::<i64, _>("ignored", 1)
            .variables(Variables { first: 5, after: None });
        assert_eq!(variables(operation), json!({ "first": 5 }));

        let error = GraphqlOperation::<()>::new("query { shop { id } }")
            .variables(vec![1])
            .into_parts()
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "[openshop] The variables of an operation must serialize to an object"
        );
    }

    #[test]
    fn a_value_that_does_not_serialize_is_reported_when_the_operation_runs() {
        struct Broken;
        impl Serialize for Broken {
            fn serialize<S: serde::Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("no"))
            }
        }
        let error = GraphqlOperation::<()>::new("query { shop { id } }")
            .json_variable("input", Broken)
            .variable::<i64, _>("first", 1)
            .into_parts()
            .unwrap_err();
        assert_eq!(error.to_string(), "[openshop] Invalid value for variable `$input`: no");
    }
}
