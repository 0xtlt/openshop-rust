//! Authenticated fetch: every Admin API request carries the App Bridge session token when the
//! UI runs inside Shopify admin.

use crate::{
    http::{self, Method},
    json::js_string,
    report::{log_error, report_error},
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

/// The headers of an Admin API request: `Content-Type` when it sends JSON, and
/// the App Bridge session token as `Authorization` when there is one.
pub fn session_headers(token: Option<String>, sends_json: bool) -> Vec<(String, String)> {
    let mut headers = Vec::with_capacity(2);
    if sends_json {
        headers.push(("Content-Type".to_string(), "application/json".to_string()));
    }
    if let Some(token) = token.filter(|token| !token.is_empty()) {
        headers.push(("Authorization".to_string(), format!("Bearer {token}")));
    }
    headers
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
    path: String,
    body: String,
}

impl ApiResponse {
    /// The body as JSON, or `None` when it is empty or not JSON.
    pub fn value(&self) -> Option<Value> {
        serde_json::from_str(&self.body).ok()
    }

    /// The body as `T`. A body that is not one is logged to the console, with the path it
    /// came from.
    pub fn json<T: DeserializeOwned>(&self) -> Result<T, ApiError> {
        serde_json::from_str(&self.body).map_err(|error| {
            let error = ApiError(error.to_string());
            log_error(
                &format!("The answer of {} is not what the UI expects", self.path),
                &error,
            );
            error
        })
    }

    /// `data.error` of the JSON body.
    pub fn error(&self) -> Option<String> {
        error_field(self.value().as_ref())
    }
}

async fn send(method: Method, path: &str, body: Option<&Value>) -> Result<http::Response, ApiError> {
    let token = shopify::id_token().await.unwrap_or_else(|error| {
        // The request goes without a session, and the server refuses it.
        log_error("Could not get a session token from App Bridge", &error);
        None
    });
    let headers = session_headers(token, body.is_some());
    let body = body.map(Value::to_string);
    let writes = method != Method::Get;
    let request = http::Request {
        method,
        url: path,
        headers: &headers,
        body: body.as_deref(),
        signal: None,
    };
    http::send(request).await.map_err(|message| {
        let error = ApiError(message);
        let context = format!("Request to {path} failed");
        if writes {
            // A page shows the answer to what it asked for, and there is none.
            report_error(&context, &error, &format!("Could not reach the server: {error}"));
        } else {
            log_error(&context, &error);
        }
        error
    })
}

/// Sends a request and reads the whole response body.
///
/// A request that gets no answer is logged to the console, and a write (anything but `GET`)
/// also shows an error toast: callers only have a response to show.
pub async fn api_request(method: Method, path: &str, body: Option<&Value>) -> Result<ApiResponse, ApiError> {
    let response = send(method, path, body).await?;
    let text = response.text().await.map_err(|message| {
        let error = ApiError(message);
        log_error(&format!("Reading the answer of {path} failed"), &error);
        error
    })?;
    Ok(ApiResponse {
        status: response.status(),
        ok: response.ok(),
        path: path.to_string(),
        body: text,
    })
}

pub async fn api_fetch(path: &str) -> Result<ApiResponse, ApiError> {
    api_request(Method::Get, path, None).await
}

/// `GET`s a download; returns the bytes and their content type.
pub async fn api_download(path: &str) -> Result<(Vec<u8>, String), ApiError> {
    let response = send(Method::Get, path, None).await?;
    if !response.ok() {
        // The error body is not a file to save.
        let body = response
            .text()
            .await
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok());
        return Err(ApiError(api_error_message(body.as_ref(), response.status())));
    }
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

/// [`api_json`] for what a page needs to open: a failure is logged and shown in a toast
/// (`what` is what could not be loaded, "the flows"), and gives `None`.
pub async fn api_json_or_report<T: DeserializeOwned>(path: &str, what: &str) -> Option<T> {
    match api_json(path).await {
        Ok(data) => Some(data),
        Err(error) => {
            report_error(
                &format!("Could not load {what}"),
                &error,
                &load_failure_message(what, &error),
            );
            None
        }
    }
}

/// The toast for something that could not be loaded.
pub fn load_failure_message(what: &str, error: &ApiError) -> String {
    format!("Could not load {what}: {error}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sends_the_session_token_when_there_is_one() {
        let header = |name: &str, value: &str| (name.to_string(), value.to_string());
        assert_eq!(
            session_headers(Some("session-token".to_string()), true),
            [
                header("Content-Type", "application/json"),
                header("Authorization", "Bearer session-token")
            ]
        );
        assert_eq!(
            session_headers(Some("session-token".to_string()), false),
            [header("Authorization", "Bearer session-token")]
        );
        assert!(session_headers(None, false).is_empty());
        assert!(session_headers(Some(String::new()), false).is_empty());
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
    fn tells_what_could_not_be_loaded_and_why() {
        assert_eq!(
            load_failure_message("the flows", &ApiError("Request failed with 500".to_string())),
            "Could not load the flows: Request failed with 500"
        );
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
