//! Outbound HTTP to Shopify, behind a trait so tests can run without network access.

use std::time::Duration;

use anyhow::Result;
use futures::future::BoxFuture;
use serde_json::Value;

/// The `Content-Type` that sends [`OutboundRequest::body`] as a URL-encoded form.
pub const FORM_CONTENT_TYPE: &str = "application/x-www-form-urlencoded";

/// A `POST` to Shopify. `body` is sent as JSON, or, by [`ReqwestTransport`],
/// as a URL-encoded form when the media type of the `Content-Type` of
/// `headers` is [`FORM_CONTENT_TYPE`] (parameters such as `charset` aside):
/// it is then an object of strings, one per field. A custom [`HttpTransport`]
/// that sends the client credentials request must do the same.
#[derive(Debug, Clone, PartialEq)]
pub struct OutboundRequest {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundResponse {
    pub status: u16,
    pub body: String,
}

impl OutboundResponse {
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn json(&self) -> Result<Value> {
        Ok(serde_json::from_str(&self.body)?)
    }
}

/// Sends `POST` requests, with a JSON body, or a form body when the request's
/// `Content-Type` says so. Shopify OAuth and Admin GraphQL only need that.
///
/// A request carries a secret, a token or a code: an implementation must not
/// follow a redirection, and returns the `3xx` as the answer, as
/// [`ReqwestTransport`] does.
pub trait HttpTransport: Send + Sync + 'static {
    fn post_json(&self, request: OutboundRequest) -> BoxFuture<'_, Result<OutboundResponse>>;
}

pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .user_agent(concat!("openshop-rust/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            // Every request carries a secret, a token or a code, in its body
            // or in `X-Shopify-Access-Token`, which a redirection would send
            // on to another host: a `3xx` is the answer, followed by no one.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_default();
        Self { client }
    }
}

impl Default for ReqwestTransport {
    fn default() -> Self {
        Self::new()
    }
}

/// Whether the `Content-Type` of `headers` is a URL-encoded form, whatever
/// its parameters and its case.
fn is_form(headers: &[(String, String)]) -> bool {
    headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("content-type")
            && value
                .split(';')
                .next()
                .is_some_and(|media_type| media_type.trim().eq_ignore_ascii_case(FORM_CONTENT_TYPE))
    })
}

impl HttpTransport for ReqwestTransport {
    fn post_json(&self, request: OutboundRequest) -> BoxFuture<'_, Result<OutboundResponse>> {
        Box::pin(async move {
            let mut builder = if is_form(&request.headers) {
                self.client.post(&request.url).body(serde_urlencoded::to_string(&request.body)?)
            } else {
                self.client.post(&request.url).json(&request.body)
            };
            for (name, value) in &request.headers {
                builder = builder.header(name, value);
            }
            let response = builder.send().await?;
            let status = response.status().as_u16();
            let body = response.text().await?;
            Ok(OutboundResponse { status, body })
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    /// Sends `request` to a local server and returns what it received.
    async fn received(headers: Vec<(String, String)>, body: Value) -> String {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let url = format!("http://{}/token", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut received = Vec::new();
            let mut buffer = [0u8; 4096];
            // The request ends with its body, whose length the headers give.
            loop {
                let read = stream.read(&mut buffer).await.unwrap();
                received.extend_from_slice(&buffer[..read]);
                let text = String::from_utf8_lossy(&received).to_string();
                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                    let length = head
                        .lines()
                        .find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length: ").map(str::to_string))
                        .and_then(|length| length.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if body.len() >= length {
                        break;
                    }
                }
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}")
                .await
                .unwrap();
            String::from_utf8(received).unwrap()
        });
        let response = ReqwestTransport::new()
            .post_json(OutboundRequest { url, headers, body })
            .await
            .unwrap();
        assert_eq!(response, OutboundResponse { status: 200, body: "{}".to_string() });
        server.await.unwrap()
    }

    #[tokio::test]
    async fn sends_a_form_when_the_content_type_says_so() {
        let request = received(
            vec![("Content-Type".to_string(), FORM_CONTENT_TYPE.to_string())],
            json!({ "grant_type": "client_credentials", "client_id": "a b&c" }),
        )
        .await
        .to_ascii_lowercase();
        assert!(request.contains("content-type: application/x-www-form-urlencoded\r\n"), "{request}");
        assert!(request.ends_with("\r\n\r\ngrant_type=client_credentials&client_id=a+b%26c"), "{request}");
    }

    #[tokio::test]
    async fn sends_a_form_whatever_the_parameters_of_the_content_type() {
        let request = received(
            vec![(
                "content-type".to_string(),
                "Application/X-WWW-Form-URLEncoded; charset=utf-8".to_string(),
            )],
            json!({ "grant_type": "client_credentials" }),
        )
        .await;
        assert!(request.ends_with("\r\n\r\ngrant_type=client_credentials"), "{request}");
    }

    #[test]
    fn reads_the_media_type_of_the_content_type() {
        let headers = |value: &str| vec![("Content-Type".to_string(), value.to_string())];
        assert!(is_form(&headers(FORM_CONTENT_TYPE)));
        assert!(is_form(&headers("application/x-www-form-urlencoded; charset=utf-8")));
        assert!(is_form(&headers(" APPLICATION/X-WWW-FORM-URLENCODED ;charset=UTF-8")));
        assert!(!is_form(&headers("application/json")));
        assert!(!is_form(&headers("application/x-www-form-urlencoded-extra")));
        assert!(!is_form(&headers("text/plain; application/x-www-form-urlencoded")));
        assert!(!is_form(&[("Accept".to_string(), FORM_CONTENT_TYPE.to_string())]));
    }

    /// Sends `request` to a local server that answers `307` towards another
    /// local port; returns the answer, and whether that port was reached.
    async fn redirected(headers: Vec<(String, String)>, body: Value) -> (OutboundResponse, bool) {
        let elsewhere = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let location = format!("http://{}/elsewhere", elsewhere.local_addr().unwrap());
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let url = format!("http://{}/admin/oauth/access_token", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = [0u8; 4096];
            let _ = stream.read(&mut buffer).await.unwrap();
            let answer = format!(
                "HTTP/1.1 307 Temporary Redirect\r\nlocation: {location}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
            );
            stream.write_all(answer.as_bytes()).await.unwrap();
        });
        let response = ReqwestTransport::new()
            .post_json(OutboundRequest { url, headers, body })
            .await
            .unwrap();
        server.await.unwrap();
        let reached = tokio::time::timeout(Duration::from_millis(300), elsewhere.accept())
            .await
            .is_ok();
        (response, reached)
    }

    #[tokio::test]
    async fn a_redirection_is_the_answer_and_is_never_followed() {
        // A token request: its body holds the app's secret.
        let (response, reached) = redirected(
            vec![("Content-Type".to_string(), FORM_CONTENT_TYPE.to_string())],
            json!({ "grant_type": "client_credentials", "client_secret": "dev-app-secret" }),
        )
        .await;
        assert_eq!(response.status, 307);
        assert!(!response.ok());
        assert!(!reached, "the token request was sent on");

        // An Admin API request: its header holds the shop's token.
        let (response, reached) = redirected(
            vec![
                ("Content-Type".to_string(), "application/json".to_string()),
                ("X-Shopify-Access-Token".to_string(), "offline-token".to_string()),
            ],
            json!({ "query": "{ shop { name } }" }),
        )
        .await;
        assert_eq!(response.status, 307);
        assert!(!reached, "the Admin API request was sent on");
    }

    #[tokio::test]
    async fn sends_json_otherwise() {
        let request = received(
            vec![("Content-Type".to_string(), "application/json".to_string())],
            json!({ "query": "{ shop { name } }" }),
        )
        .await;
        assert!(request.ends_with("\r\n\r\n{\"query\":\"{ shop { name } }\"}"), "{request}");
    }
}
