//! Helpers for building HTTP responses in server routes.

use axum::http::header::CONTENT_TYPE;
use serde::Serialize;

pub use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
pub use axum::response::{IntoResponse, Response};

/// `200 OK` with a JSON body.
pub fn json(body: impl Serialize) -> Response {
    json_with_status(StatusCode::OK, body)
}

pub fn json_with_status(status: StatusCode, body: impl Serialize) -> Response {
    match serde_json::to_vec(&body) {
        Ok(bytes) => (status, [(CONTENT_TYPE, "application/json")], bytes).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// `200 OK` with a plain-text body.
pub fn text(body: impl Into<String>) -> Response {
    ([(CONTENT_TYPE, "text/plain; charset=utf-8")], body.into()).into_response()
}

/// An empty response with the given status.
pub fn status(status: StatusCode) -> Response {
    status.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_json_and_text_responses() {
        let response = json(serde_json::json!({ "ok": true }));
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[CONTENT_TYPE], "application/json");

        let response = json_with_status(StatusCode::CREATED, serde_json::json!({ "id": 1 }));
        assert_eq!(response.status(), StatusCode::CREATED);

        let response = text("pong");
        assert_eq!(response.headers()[CONTENT_TYPE], "text/plain; charset=utf-8");
        assert_eq!(status(StatusCode::NO_CONTENT).status(), StatusCode::NO_CONTENT);
    }
}
