//! Server-rendered views of admin pages: what a `#[view]` that returns
//! [`Markup`] is given, and what a form action answers.
//!
//! The server renders the page: it runs the access rules, then the loaders the
//! view names, then the view, a function of their outputs that returns HTML.
//! A form of the page posts to the URL of the page, with the name of the
//! action in its `_action` field; the action answers an [`Outcome`] (a toast,
//! a redirect) or any other value, and the page is rendered again.

use std::any::Any;
use std::cell::RefCell;
use std::future::Future;

use anyhow::Result;
use maud::Markup;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use super::{AdminContext, AdminPublicError};
use crate::types::InputSchema;

/// The field of a form that names the action it runs.
pub const ACTION_FIELD: &str = "_action";

/// A request for the view of a page, as the server renders it: the context of
/// the page, and the query string of its URL, which the loaders that take an
/// input read.
#[derive(Clone)]
pub struct PageRequest {
    pub ctx: AdminContext,
    /// The query string of the page URL, without `?`.
    pub query: String,
}

impl PageRequest {
    /// Refuses the page with `403 FORBIDDEN` when `rule` says no: the
    /// `authorize` rule of a loader the view reads.
    pub async fn authorize<F, Fut>(&self, rule: F) -> Result<()>
    where
        F: FnOnce(AdminContext) -> Fut,
        Fut: Future<Output = Result<bool>>,
    {
        if rule(self.ctx.clone()).await? {
            Ok(())
        } else {
            Err(AdminPublicError::new("FORBIDDEN", "Forbidden").status(403).into())
        }
    }

    /// Runs a loader without input.
    pub async fn load<R, F, Fut>(&self, loader: F) -> Result<R>
    where
        F: FnOnce(AdminContext) -> Fut,
        Fut: Future<Output = Result<R>>,
    {
        loader(self.ctx.clone()).await
    }

    /// Runs a loader whose input is read from the query string of the page:
    /// `?status=active&page=2` for `struct Filter { status: String, page: u32 }`.
    /// A value that does not satisfy the input schema is `400 INVALID_INPUT`.
    pub async fn load_with<T, R, F, Fut>(&self, loader: F) -> Result<R>
    where
        T: DeserializeOwned + schemars::JsonSchema,
        F: FnOnce(AdminContext, T) -> Fut,
        Fut: Future<Output = Result<R>>,
    {
        let schema = InputSchema::of::<T>();
        let value = schema
            .parse(fields_to_json(&schema.json_schema, &parse_fields(self.query.as_bytes())))
            .map_err(|summary| AdminPublicError::new("INVALID_INPUT", summary).status(400))?;
        loader(self.ctx.clone(), serde_json::from_value(value)?).await
    }
}

/// What a form action answers to the page: a toast, a page to go to, or both.
/// Without a redirect, the page the form was sent from is rendered again.
///
/// ```
/// use openshop::admin::Outcome;
///
/// let saved = Outcome::toast("Review saved");
/// let created = Outcome::redirect("/reviews/42").with_toast("Review created");
/// # let _ = (saved, created);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Outcome {
    pub toast: Option<String>,
    /// The toast reports a failure.
    pub error: bool,
    /// A path of the admin, such as `/reviews/42`.
    pub redirect: Option<String>,
}

impl Outcome {
    /// Renders the page again and shows `message` in a toast.
    pub fn toast(message: impl Into<String>) -> Self {
        Self::default().with_toast(message)
    }

    /// Renders the page again and shows `message` in an error toast.
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            error: true,
            ..Self::toast(message)
        }
    }

    /// Goes to another page of the admin, such as `/reviews/42`.
    pub fn redirect(path: impl Into<String>) -> Self {
        Self {
            redirect: Some(path.into()),
            ..Self::default()
        }
    }

    pub fn with_toast(mut self, message: impl Into<String>) -> Self {
        self.toast = Some(message.into());
        self
    }
}

tokio::task_local! {
    /// The outcome of the action a form runs, kept while it runs.
    static OUTCOME: RefCell<Option<Outcome>>;
}

/// Runs `future`, an action, and gives back what it returned with the
/// [`Outcome`] it answered, if it answered one.
pub(crate) async fn with_outcome<F: Future>(future: F) -> (F::Output, Option<Outcome>) {
    OUTCOME
        .scope(RefCell::new(None), async move {
            let output = future.await;
            let outcome = OUTCOME.with(|outcome| outcome.borrow_mut().take());
            (output, outcome)
        })
        .await
}

/// What a page function returned, as JSON. An [`Outcome`] is also kept for
/// the form that ran the function. What `#[admin_page]` calls.
#[doc(hidden)]
pub fn output<R: Serialize + 'static>(output: R) -> Result<Value> {
    if let Some(outcome) = (&output as &dyn Any).downcast_ref::<Outcome>() {
        // Outside a form there is nobody to tell.
        let _ = OUTCOME.try_with(|kept| *kept.borrow_mut() = Some(outcome.clone()));
    }
    Ok(serde_json::to_value(output)?)
}

/// The fields of a form or a query string, in order, a name given several times kept each time.
pub(crate) fn parse_fields(encoded: &[u8]) -> Vec<(String, String)> {
    url::form_urlencoded::parse(encoded).into_owned().collect()
}

/// The JSON types a property of a schema accepts: `"type": "boolean"` or `"type": ["integer", "null"]`.
fn property_types(property: &Value) -> Vec<&str> {
    match property.get("type") {
        Some(Value::String(kind)) => vec![kind.as_str()],
        Some(Value::Array(kinds)) => kinds.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    }
}

/// A field of a form as the JSON value the schema of the input expects: a
/// boolean (`true`, `on`, `1`), a number, or text. Empty text is `null` for a
/// property that may be `null`.
fn field_value(value: &str, types: &[&str]) -> Value {
    if value.is_empty() && types.contains(&"null") {
        return Value::Null;
    }
    if types.contains(&"boolean") {
        match value {
            "true" | "on" | "1" => return Value::Bool(true),
            "false" | "off" | "0" | "" => return Value::Bool(false),
            _ => {}
        }
    }
    if types.contains(&"integer")
        && let Ok(number) = value.parse::<i64>()
    {
        return Value::from(number);
    }
    if types.contains(&"number")
        && let Some(number) = value.parse::<f64>().ok().and_then(serde_json::Number::from_f64)
    {
        return Value::Number(number);
    }
    Value::String(value.to_string())
}

/// The fields of a form, or of a query string, as the JSON object the input
/// schema describes. Each field is converted to the type of its property; a
/// property that is an array collects every field of its name; a boolean
/// property without a field is `false`, as an unchecked checkbox sends none.
/// Fields that start with `_` (`_action`) are the page's, not the input's.
pub(crate) fn fields_to_json(schema: &Value, fields: &[(String, String)]) -> Value {
    let properties = schema.get("properties").and_then(Value::as_object);
    let property = |name: &str| properties.and_then(|properties| properties.get(name));
    let mut object = Map::new();
    for (name, value) in fields.iter().filter(|(name, _)| !name.starts_with('_')) {
        let types = property(name).map(property_types).unwrap_or_default();
        if types.contains(&"array") {
            let items = property(name)
                .and_then(|property| property.get("items"))
                .map(property_types)
                .unwrap_or_default();
            let entry = object.entry(name.clone()).or_insert_with(|| Value::Array(Vec::new()));
            if let Value::Array(entries) = entry {
                entries.push(field_value(value, &items));
            }
        } else {
            object.insert(name.clone(), field_value(value, &types));
        }
    }
    if let Some(properties) = properties {
        for (name, property) in properties {
            let types = property_types(property);
            if !object.contains_key(name) && types.contains(&"boolean") && !types.contains(&"null") {
                object.insert(name.clone(), Value::Bool(false));
            }
        }
    }
    Value::Object(object)
}

/// The view of a page, as the server renders it.
pub type AdminView =
    std::sync::Arc<dyn Fn(PageRequest) -> futures::future::BoxFuture<'static, Result<Markup>> + Send + Sync>;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(serde::Deserialize, schemars::JsonSchema)]
    #[serde(rename_all = "camelCase")]
    #[allow(dead_code)]
    struct Filter {
        status: Option<String>,
        page: Option<u32>,
        hidden: bool,
        tags: Vec<String>,
        min_price: Option<f64>,
    }

    #[test]
    fn reads_a_form_as_the_input_schema_types_it() {
        let schema = serde_json::to_value(schemars::schema_for!(Filter)).unwrap();
        let fields = parse_fields(b"_action=save&status=active&page=2&tags=a&tags=b&minPrice=&extra=x");
        assert_eq!(
            fields_to_json(&schema, &fields),
            json!({
                "status": "active",
                "page": 2,
                "tags": ["a", "b"],
                "minPrice": null,
                "extra": "x",
                // An unchecked checkbox sends nothing.
                "hidden": false,
            })
        );
        let checked = fields_to_json(&schema, &parse_fields(b"hidden=on&page=x"));
        assert_eq!(checked["hidden"], json!(true));
        // What is not a number stays text, for the schema to refuse.
        assert_eq!(checked["page"], json!("x"));
        assert!(InputSchema::of::<Filter>().parse(checked).is_err());
    }

    #[tokio::test]
    async fn keeps_the_outcome_of_an_action() {
        let (value, outcome) = with_outcome(async { output(Outcome::toast("Saved")) }).await;
        assert_eq!(outcome, Some(Outcome::toast("Saved")));
        assert_eq!(value.unwrap()["toast"], json!("Saved"));
        // Another value is no outcome, and outside a form nothing is kept.
        let (_, outcome) = with_outcome(async { output(json!({ "toast": "no" })) }).await;
        assert_eq!(outcome, None);
        assert!(output(Outcome::toast("alone")).is_ok());
    }
}
