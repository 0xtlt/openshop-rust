//! Authenticated fetch: every Admin API request carries the App Bridge session token when the
//! UI runs inside Shopify admin.

use crate::{
    http::{self, Method},
    json::js_string,
    shopify,
};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::fmt;

/// A failed request, carrying the message shown to the merchant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError(pub String);

impl fmt::Display for ApiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ApiError {}

/// `Authorization` header for a session token lookup. A missing, empty or failed token sends
/// the request without one.
pub fn authorization_header<E>(token: Result<Option<String>, E>) -> Option<String> {
    token
        .ok()
        .flatten()
        .filter(|token| !token.is_empty())
        .map(|token| format!("Bearer {token}"))
}

/// Request headers with `Authorization` set, keeping the caller's other headers.
pub fn with_authorization(headers: &[(&str, &str)], authorization: Option<String>) -> Vec<(String, String)> {
    let mut merged: Vec<(String, String)> = headers
        .iter()
        .filter(|(name, _)| authorization.is_none() || !name.eq_ignore_ascii_case("authorization"))
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect();
    if let Some(authorization) = authorization {
        merged.push(("Authorization".to_string(), authorization));
    }
    merged
}

/// The `error` member of a JSON error body, when there is one.
pub fn error_field(body: Option<&Value>) -> Option<String> {
    body?.get("error").filter(|error| !error.is_null()).map(js_string)
}

/// Message for a failed JSON request.
pub fn api_error_message(body: Option<&Value>, status: u16) -> String {
    match body.and_then(Value::as_object).and_then(|body| body.get("error")) {
        Some(error) => js_string(error),
        None => format!("Request failed with {status}"),
    }
}

/// Status and body of a completed request.
#[derive(Debug, Clone)]
pub struct ApiResponse {
    pub status: u16,
    pub ok: bool,
    body: String,
}

impl ApiResponse {
    /// The body as JSON, or `None` when it is empty or not JSON.
    pub fn value(&self) -> Option<Value> {
        serde_json::from_str(&self.body).ok()
    }

    pub fn json<T: DeserializeOwned>(&self) -> Result<T, ApiError> {
        serde_json::from_str(&self.body).map_err(|error| ApiError(error.to_string()))
    }

    /// `data.error` of the JSON body.
    pub fn error(&self) -> Option<String> {
        error_field(self.value().as_ref())
    }
}

async fn send(method: Method, path: &str, body: Option<&Value>) -> Result<http::Response, ApiError> {
    let content_type: &[(&str, &str)] = match body {
        Some(_) => &[("Content-Type", "application/json")],
        None => &[],
    };
    let authorization = authorization_header(shopify::id_token().await);
    let body = body.map(Value::to_string);
    let request = http::Request {
        method,
        url: path,
        headers: &with_authorization(content_type, authorization),
        body: body.as_deref(),
        signal: None,
    };
    http::send(request).await.map_err(ApiError)
}

/// Sends a request and reads the whole response body.
pub async fn api_request(method: Method, path: &str, body: Option<&Value>) -> Result<ApiResponse, ApiError> {
    let response = send(method, path, body).await?;
    Ok(ApiResponse {
        status: response.status(),
        ok: response.ok(),
        body: response.text().await.map_err(ApiError)?,
    })
}

pub async fn api_fetch(path: &str) -> Result<ApiResponse, ApiError> {
    api_request(Method::Get, path, None).await
}

/// `GET`s a download; returns the bytes and their content type.
pub async fn api_download(path: &str) -> Result<(Vec<u8>, String), ApiError> {
    let response = send(Method::Get, path, None).await?;
    let content_type = response.header("content-type").unwrap_or_default();
    Ok((response.bytes().await.map_err(ApiError)?, content_type))
}

/// `GET`s JSON, turning error statuses into the server's `error` message.
pub async fn api_json<T: DeserializeOwned>(path: &str) -> Result<T, ApiError> {
    let response = api_fetch(path).await?;
    if !response.ok {
        return Err(ApiError(api_error_message(response.value().as_ref(), response.status)));
    }
    response.json()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
        headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    #[test]
    fn adds_an_app_bridge_session_token_when_available() {
        let token: Result<_, ()> = Ok(Some("session-token".to_string()));
        let headers = with_authorization(&[], authorization_header(token));
        assert_eq!(header(&headers, "Authorization"), Some("Bearer session-token"));
    }

    #[test]
    fn preserves_existing_request_headers() {
        let token: Result<_, ()> = Ok(Some("session-token".to_string()));
        let headers = with_authorization(&[("X-Test", "kept")], authorization_header(token));
        assert_eq!(header(&headers, "Authorization"), Some("Bearer session-token"));
        assert_eq!(header(&headers, "X-Test"), Some("kept"));
    }

    #[test]
    fn continues_without_authorization_outside_shopify_admin() {
        let token: Result<Option<String>, ()> = Ok(None);
        let headers = with_authorization(&[("X-Test", "kept")], authorization_header(token));
        assert_eq!(header(&headers, "Authorization"), None);
        assert_eq!(authorization_header::<()>(Ok(Some(String::new()))), None);
    }

    #[test]
    fn continues_without_authorization_when_token_retrieval_fails() {
        let token: Result<Option<String>, &str> = Err("not ready");
        let headers = with_authorization(&[], authorization_header(token));
        assert_eq!(header(&headers, "Authorization"), None);
    }

    #[test]
    fn replaces_a_caller_supplied_authorization_header() {
        let headers = with_authorization(&[("authorization", "Basic x")], Some("Bearer y".to_string()));
        assert_eq!(headers, vec![("Authorization".to_string(), "Bearer y".to_string())]);
        let untouched = with_authorization(&[("authorization", "Basic x")], None);
        assert_eq!(header(&untouched, "Authorization"), Some("Basic x"));
    }

    #[test]
    fn extracts_api_error_messages() {
        assert_eq!(
            api_error_message(Some(&json!({ "error": "Not found" })), 404),
            "Not found"
        );
        assert_eq!(api_error_message(Some(&json!({ "error": 42 })), 400), "42");
        assert_eq!(api_error_message(Some(&json!({ "error": null })), 400), "null");
        assert_eq!(
            api_error_message(Some(&json!({ "message": "x" })), 500),
            "Request failed with 500"
        );
        assert_eq!(
            api_error_message(Some(&json!(["error"])), 500),
            "Request failed with 500"
        );
        assert_eq!(api_error_message(Some(&json!("error")), 502), "Request failed with 502");
        assert_eq!(api_error_message(None, 401), "Request failed with 401");
    }

    #[test]
    fn reads_the_optional_error_field() {
        assert_eq!(
            error_field(Some(&json!({ "error": "Flow is already running" }))).as_deref(),
            Some("Flow is already running")
        );
        assert_eq!(error_field(Some(&json!({ "error": null }))), None);
        assert_eq!(error_field(Some(&json!({ "ok": true }))), None);
        assert_eq!(error_field(None), None);
    }
}
