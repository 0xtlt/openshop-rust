//! Small HTTP helpers shared by the route modules.

use axum::Json;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use indexmap::IndexMap;
use serde_json::{Value, json};

use crate::engine::step::error_message;
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

/// Whether the error comes from the server's own machinery (database,
/// network, files, URLs) rather than from what the caller asked for. Its text
/// can hold a query, an address or a key, so it never goes to a client.
pub(crate) fn is_internal_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.is::<sqlx::Error>()
            || cause.is::<reqwest::Error>()
            || cause.is::<std::io::Error>()
            || cause.is::<url::ParseError>()
    })
}

/// What a client may read about a failure.
pub(crate) struct ClientError {
    pub message: String,
    /// Set for an internal error: the id of the log line that has the cause.
    pub error_id: Option<String>,
}

/// The message of an error the caller can act on, as it is. An internal error
/// is logged and reported with a new id, and the client gets a generic message
/// and that id.
pub(crate) fn client_error(what: &str, error: &anyhow::Error) -> ClientError {
    if !is_internal_error(error) {
        return ClientError {
            message: error_message(error),
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
