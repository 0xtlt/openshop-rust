//! Server-rendered views of admin pages: what a `#[view]` that returns
//! [`Markup`] is given, and what a form action answers.
//!
//! The server renders the page: it runs the access rules, then the loaders the
//! view names, then the view, a function of their outputs that returns HTML.
//! A form of the page posts to the URL of the page, with the name of the
//! action in its `_action` field; the action answers an [`Outcome`] (a toast,
//! a redirect) or any other value, and the page is rendered again. A form the
//! page refuses is rendered again with what was sent and the error of each
//! field: the [`FormState`] of a view that reads it.

use std::any::Any;
use std::cell::RefCell;
use std::future::Future;
use std::sync::LazyLock;

use anyhow::Result;
use indexmap::IndexMap;
use maud::Markup;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use super::{AdminContext, AdminPublicError};
use crate::types::InputSchema;

/// The field of a form that names the action it runs.
pub const ACTION_FIELD: &str = "_action";

/// A request for the view of a page, as the server renders it: the context of
/// the page, the query string of its URL, which the loaders that take an
/// input read, and the form the page refused, if it refused one.
#[derive(Clone)]
pub struct PageRequest {
    pub ctx: AdminContext,
    /// The query string of the page URL, without `?`.
    pub query: String,
    /// The form the page refused, rendered again: what a `form` parameter of
    /// the view receives. Empty otherwise.
    pub form: FormState,
}

/// A form the page refused, as its view shows it again: the values that were
/// sent and the error of each field. What a `#[view]` receives in a parameter
/// named `form`; empty when the page is rendered for anything else.
///
/// It is filled when the input schema of the action refuses the form (`422`),
/// when the action answers [`Outcome::invalid`], and when it fails with an
/// [`AdminPublicError`] that has field errors. A view that reads it shows the
/// errors itself, next to its fields; one that does not keeps the page as it
/// was, with an error toast.
///
/// ```
/// use maud::html;
/// use openshop::admin::FormState;
///
/// fn title_field(form: &FormState, saved: &str) -> maud::Markup {
///     html! {
///         s-text-field name="title" label="Title"
///             value=(form.value_or("title", saved)) error=[form.error("title")] {}
///     }
/// }
/// # let _ = title_field(&FormState::default(), "Great");
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FormState {
    /// The action of the form, what it sent in `_action`. `None` when no form
    /// was refused.
    pub action: Option<String>,
    /// The fields that were sent, in order, a name sent several times kept
    /// each time.
    pub values: Vec<(String, String)>,
    /// The error of each field, by name: `Required`, `Must be at least 3 characters`.
    pub errors: IndexMap<String, String>,
    /// What the action said of the form as a whole, when it said something.
    pub message: Option<String>,
}

static NO_FORM: LazyLock<FormState> = LazyLock::new(FormState::default);

impl FormState {
    /// Whether a form was refused: the page is rendered again for it.
    pub fn is_refused(&self) -> bool {
        self.action.is_some()
    }

    /// The state of the form of the action `action`, or an empty one when the
    /// refused form is another one: a page with several forms shows the
    /// errors on the one that was sent. `form.of(Self::SAVE_REVIEW)`.
    pub fn of(&self, action: &str) -> &FormState {
        if self.action.as_deref() == Some(action) { self } else { &NO_FORM }
    }

    /// The value sent for the field `name`, the first one when it was sent
    /// several times.
    pub fn value(&self, name: &str) -> Option<&str> {
        self.values
            .iter()
            .find(|(field, _)| field == name)
            .map(|(_, value)| value.as_str())
    }

    /// The value sent for `name`, or `saved`, what the page shows otherwise:
    /// the `value` of a field rendered again after an error.
    pub fn value_or<'a>(&'a self, name: &str, saved: &'a str) -> &'a str {
        if self.is_refused() { self.value(name).unwrap_or_default() } else { saved }
    }

    /// Every value sent for `name`: the boxes checked of a group, the options of a multiple choice.
    pub fn values(&self, name: &str) -> impl Iterator<Item = &str> {
        self.values
            .iter()
            .filter(move |(field, _)| field == name)
            .map(|(_, value)| value.as_str())
    }

    /// The error of the field `name`: what its `error` attribute shows,
    /// `error=[form.error("title")]`.
    pub fn error(&self, name: &str) -> Option<&str> {
        self.errors.get(name).map(String::as_str)
    }
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
    /// The error of each field of the form: the form is refused, see
    /// [`Outcome::invalid`].
    #[serde(skip_serializing_if = "IndexMap::is_empty")]
    pub field_errors: IndexMap<String, String>,
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

    /// Refuses the form: the page is rendered again with what was sent and
    /// `errors`, the error of each field by name, in the [`FormState`] of its
    /// view (`422`). A view that does not read it keeps the page as it was,
    /// with an error toast. `.with_toast(…)` adds an error toast.
    ///
    /// ```
    /// use openshop::admin::Outcome;
    ///
    /// let taken = Outcome::invalid([("handle", "Already taken")]);
    /// # let _ = taken;
    /// ```
    pub fn invalid<I, K, V>(errors: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        Self {
            error: true,
            field_errors: errors
                .into_iter()
                .map(|(field, message)| (field.into(), message.into()))
                .collect(),
            ..Self::default()
        }
    }

    pub fn with_toast(mut self, message: impl Into<String>) -> Self {
        self.toast = Some(message.into());
        self
    }
}

/// The error of a field as a form shows it, from what the input schema says
/// of its value: `is required` is `Required`, `must be integer` is `Must be a
/// whole number`.
pub(crate) fn field_message(error: &str) -> String {
    let error = error.strip_suffix(" or null").unwrap_or(error);
    match error {
        "is required" => "Required".to_string(),
        "must be integer" => "Must be a whole number".to_string(),
        "must be number" => "Must be a number".to_string(),
        "must be boolean" => "Must be true or false".to_string(),
        _ => {
            let mut letters = error.chars();
            letters
                .next()
                .map(|first| first.to_uppercase().chain(letters).collect())
                .unwrap_or_default()
        }
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

/// The JSON types a property of a schema accepts: `"type": "boolean"` or
/// `"type": ["integer", "null"]`, through a local `$ref` and the alternatives
/// of `anyOf` and `oneOf` (`Option<Mode>` is `anyOf: [{ "$ref": … }, { "type": "null" }]`).
fn property_types<'a>(property: &'a Value, root: &'a Value) -> Vec<&'a str> {
    fn collect<'a>(property: &'a Value, root: &'a Value, depth: usize, types: &mut Vec<&'a str>) {
        if depth > 8 {
            return;
        }
        match property.get("type") {
            Some(Value::String(kind)) => types.push(kind.as_str()),
            Some(Value::Array(kinds)) => types.extend(kinds.iter().filter_map(Value::as_str)),
            _ => {}
        }
        if let Some(target) = property
            .get("$ref")
            .and_then(Value::as_str)
            .and_then(|reference| reference.strip_prefix('#'))
            .and_then(|pointer| root.pointer(pointer))
        {
            collect(target, root, depth + 1, types);
        }
        for keyword in ["anyOf", "oneOf"] {
            for alternative in property.get(keyword).and_then(Value::as_array).into_iter().flatten() {
                collect(alternative, root, depth + 1, types);
            }
        }
    }
    let mut types = Vec::new();
    collect(property, root, 0, &mut types);
    types
}

/// A field of a form as the JSON value the schema of the input expects: a
/// boolean (`true`, `on`, `1`), a number, or text. Empty text is `null` for a
/// property that may be `null`, and no value at all for a number: the field
/// was left empty, the schema says whether it is required.
fn field_value(value: &str, types: &[&str]) -> Option<Value> {
    if value.is_empty() && types.contains(&"null") {
        return Some(Value::Null);
    }
    if value.is_empty() && !types.is_empty() && types.iter().all(|kind| matches!(*kind, "integer" | "number")) {
        return None;
    }
    Some(typed_value(value, types))
}

fn typed_value(value: &str, types: &[&str]) -> Value {
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
pub(crate) fn fields_to_json<'a>(schema: &'a Value, fields: &[(String, String)]) -> Value {
    let properties = schema.get("properties").and_then(Value::as_object);
    let property = |name: &str| properties.and_then(|properties| properties.get(name));
    let types_of = |property: &'a Value| property_types(property, schema);
    let mut object = Map::new();
    for (name, value) in fields.iter().filter(|(name, _)| !name.starts_with('_')) {
        let types = property(name).map(types_of).unwrap_or_default();
        if types.contains(&"array") {
            let items = property(name)
                .and_then(|property| property.get("items"))
                .map(types_of)
                .unwrap_or_default();
            let entry = object.entry(name.clone()).or_insert_with(|| Value::Array(Vec::new()));
            if let (Value::Array(entries), Some(item)) = (entry, field_value(value, &items)) {
                entries.push(item);
            }
        } else if let Some(value) = field_value(value, &types) {
            object.insert(name.clone(), value);
        }
    }
    if let Some(properties) = properties {
        for (name, property) in properties {
            let types = types_of(property);
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

    #[derive(serde::Deserialize, schemars::JsonSchema)]
    #[serde(rename_all = "camelCase")]
    #[allow(dead_code)]
    enum Mode {
        Fast,
        Slow,
    }

    #[derive(serde::Deserialize, schemars::JsonSchema)]
    #[serde(rename_all = "camelCase")]
    #[allow(dead_code)]
    struct Review {
        #[schemars(length(min = 3))]
        title: String,
        rating: u8,
        mode: Option<Mode>,
    }

    #[test]
    fn a_field_left_empty_is_a_missing_number_and_a_missing_choice() {
        let schema = serde_json::to_value(schemars::schema_for!(Review)).unwrap();
        let input = fields_to_json(&schema, &parse_fields(b"title=ok&rating=&mode="));
        assert_eq!(input, json!({ "title": "ok", "mode": null }));
        let refused = crate::server::mcp::schema::property_errors(&input, &schema);
        let shown: Vec<(&str, String)> = refused
            .iter()
            .map(|(field, error)| (field.as_str(), field_message(error)))
            .collect();
        assert_eq!(
            shown,
            [
                ("title", "Must be at least 3 characters".to_string()),
                ("rating", "Required".to_string()),
            ]
        );
        assert_eq!(field_message("must be integer or null"), "Must be a whole number");
        let typed = fields_to_json(&schema, &parse_fields(b"title=Great&rating=4&mode=slow"));
        assert_eq!(typed, json!({ "title": "Great", "rating": 4, "mode": "slow" }));
    }

    #[test]
    fn a_refused_form_gives_back_what_was_sent_and_its_errors() {
        let form = FormState {
            action: Some("saveReview".to_string()),
            values: parse_fields(b"_action=saveReview&title=ok&tags=a&tags=b"),
            errors: IndexMap::from([("title".to_string(), "Too short".to_string())]),
            message: None,
        };
        assert!(form.is_refused());
        assert_eq!(form.value_or("title", "Saved title"), "ok");
        // A field the form did not send is empty, not the saved value.
        assert_eq!(form.value_or("note", "Saved note"), "");
        assert_eq!(form.values("tags").collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(form.error("title"), Some("Too short"));
        assert_eq!(form.of("saveReview"), &form);
        // Another form of the page is shown as it was.
        let other = form.of("deleteReview");
        assert!(!other.is_refused());
        assert_eq!(other.value_or("title", "Saved title"), "Saved title");
        assert_eq!(other.error("title"), None);
    }

    #[test]
    fn an_invalid_outcome_carries_the_error_of_each_field() {
        let outcome = Outcome::invalid([("handle", "Already taken")]).with_toast("Check the form");
        assert!(outcome.error);
        assert_eq!(outcome.field_errors["handle"], "Already taken");
        let json = serde_json::to_value(&outcome).unwrap();
        assert_eq!(json["field_errors"]["handle"], "Already taken");
        // Without field errors, nothing is said of them.
        assert!(serde_json::to_value(Outcome::toast("Saved")).unwrap().get("field_errors").is_none());
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
