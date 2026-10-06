//! Small HTTP helpers shared by the route modules.

use axum::Json;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use indexmap::IndexMap;
use serde_json::{Value, json};

use crate::engine::step::error_message;
use crate::runtime::logger;
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

/// An unexpected failure: logged, and reported as a bare 500.
pub(crate) struct ApiError(anyhow::Error);

impl<E: Into<anyhow::Error>> From<E> for ApiError {
    fn from(error: E) -> Self {
        Self(error.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        logger::error_with(
            "[openshop] Unhandled request error",
            json!({ "error": error_message(&self.0) }),
        );
        (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
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
