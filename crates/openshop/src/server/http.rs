//! Small HTTP helpers shared by the route modules.

use std::panic::AssertUnwindSafe;

use axum::Json;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use futures::FutureExt;
use indexmap::IndexMap;
use serde_json::{Value, json};

use crate::engine::step::error_message;
use crate::runtime::report::Panicked;
use crate::runtime::{logger, report};
use crate::types::JsonMap;

pub(crate) fn json_response(status: StatusCode, body: Value) -> Response {
    (status, Json(body)).into_response()
}

pub(crate) fn json_error(status: StatusCode, message: impl Into<String>) -> Response {
    json_response(status, json!({ "error": message.into() }))
}

pub(crate) fn not_found() -> Response {
    (StatusCode::NOT_FOUND, "404 Not Found").into_response()
}

/// Query parameters as a map; the first value wins for repeated keys.
pub fn parse_query(raw: Option<&str>) -> IndexMap<String, String> {
    let mut query = IndexMap::new();
    for (key, value) in url::form_urlencoded::parse(raw.unwrap_or_default().as_bytes()) {
        query.entry(key.into_owned()).or_insert_with(|| value.into_owned());
    }
    query
}

/// `%text%` for `LIKE ... ESCAPE '\'`, with what `LIKE` reads as a pattern
/// (`%`, `_`, `\`) escaped: the text is matched as it is written.
pub(crate) fn containing(text: &str) -> String {
    let mut pattern = String::with_capacity(text.len() + 2);
    pattern.push('%');
    for character in text.chars() {
        if matches!(character, '\\' | '%' | '_') {
            pattern.push('\\');
        }
        pattern.push(character);
    }
    pattern.push('%');
    pattern
}

/// Runs code of the application: a panic in it fails it as an error it
/// returned would, `<what> panicked`.
pub(crate) async fn catch_panic<T>(
    what: &str,
    work: impl Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    AssertUnwindSafe(work)
        .catch_unwind()
        .await
        .unwrap_or_else(|_| Err(Panicked::error(format!("{what} panicked"))))
}

/// Reports a failure of code of the application, and logs it under `message`.
pub(crate) fn report_failure(message: impl AsRef<str>, error: &anyhow::Error) {
    report::error(error);
    logger::error_with(message, json!({ "error": error_message(error) }));
}

/// Escapes text for an HTML element or a quoted attribute.
pub(crate) fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            other => escaped.push(other),
        }
    }
    escaped
}

pub(crate) fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

/// A JSON object body, or `{}` when the body is missing or not an object.
pub(crate) fn json_object_body(body: &[u8]) -> JsonMap {
    match serde_json::from_slice(body) {
        Ok(Value::Object(object)) => object,
        _ => JsonMap::new(),
    }
}

/// A short id that ties a response to the log line of its error.
pub(crate) fn error_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..12].to_string()
}

/// An error whose message is meant for the client: a refused input, a missing
/// resource. The Admin API and the MCP endpoint show the message of this error
/// as it is; any other error that reaches them is logged under an id, and the
/// client reads the id: the text of an error that nobody wrote for a client
/// can hold a query, an address, a key or a value of the caller.
///
/// `bail!(PublicError::new("shop is not installed"))`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicError(pub String);

impl PublicError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl std::fmt::Display for PublicError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PublicError {}

/// A [`PublicError`] as an `anyhow::Error`, for the errors the framework raises
/// for a client.
pub(crate) fn public_error(message: impl Into<String>) -> anyhow::Error {
    PublicError::new(message).into()
}

/// The message of the first error of the chain that is written for a client.
fn public_message(error: &anyhow::Error) -> Option<String> {
    use crate::admin::AdminPublicError;
    use crate::declare::InvalidInput;
    use crate::engine::errors::{FlowConcurrencyError, InvalidFlowInput, UnknownFlowError};
    use crate::server::log_query::InvalidLogQuery;

    error.chain().find_map(|cause| {
        if let Some(public) = cause.downcast_ref::<PublicError>() {
            return Some(public.to_string());
        }
        macro_rules! shown {
            ($($kind:ty),*) => {
                $(if let Some(known) = cause.downcast_ref::<$kind>() {
                    return Some(known.to_string());
                })*
            };
        }
        shown!(
            AdminPublicError,
            InvalidInput,
            FlowConcurrencyError,
            InvalidFlowInput,
            UnknownFlowError,
            InvalidLogQuery
        );
        None
    })
}

/// What a client may read about a failure.
pub(crate) struct ClientError {
    pub message: String,
    /// Set for an internal error: the id of the log line that has the cause.
    pub error_id: Option<String>,
}

/// The message of an error written for the client, as it is. Any other error is
/// logged and reported with a new id, and the client gets a generic message and
/// that id.
pub(crate) fn client_error(what: &str, error: &anyhow::Error) -> ClientError {
    if let Some(message) = public_message(error) {
        return ClientError {
            message,
            error_id: None,
        };
    }
    let id = error_id();
    logger::error_with(
        format!("[openshop] {what}"),
        json!({ "errorId": id, "error": error_message(error) }),
    );
    report::error(error);
    ClientError {
        message: format!("Internal error (error id {id})"),
        error_id: Some(id),
    }
}

/// An unexpected failure: logged with an id, and reported as a bare 500 that
/// carries the id in `x-error-id`.
pub(crate) struct ApiError(anyhow::Error);

impl<E: Into<anyhow::Error>> From<E> for ApiError {
    fn from(error: E) -> Self {
        Self(error.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let id = error_id();
        logger::error_with(
            "[openshop] Unhandled request error",
            json!({ "errorId": id, "error": error_message(&self.0) }),
        );
        report::error(&self.0);
        let mut response = (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response();
        if let Ok(value) = HeaderValue::from_str(&id) {
            response.headers_mut().insert("x-error-id", value);
        }
        response
    }
}

pub(crate) type ApiResult = Result<Response, ApiError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_queries_with_first_value_winning() {
        let query = parse_query(Some("shop=a.myshopify.com&x=1&x=2&space=a+b&enc=%2Fpath"));
        assert_eq!(query["shop"], "a.myshopify.com");
        assert_eq!(query["x"], "1");
        assert_eq!(query["space"], "a b");
        assert_eq!(query["enc"], "/path");
        assert!(parse_query(None).is_empty());
    }

    #[test]
    fn reads_object_bodies_only() {
        assert_eq!(json_object_body(br#"{"a":1}"#)["a"], 1);
        assert!(json_object_body(b"[1]").is_empty());
        assert!(json_object_body(b"nope").is_empty());
        assert!(json_object_body(b"").is_empty());
    }
}
