//! Outbound HTTP to Shopify, behind a trait so tests can run without network access.

use std::time::Duration;

use anyhow::Result;
use futures::future::BoxFuture;
use serde_json::Value;

/// The `Content-Type` that sends [`OutboundRequest::body`] as a URL-encoded form.
pub const FORM_CONTENT_TYPE: &str = "application/x-www-form-urlencoded";

/// A `POST` to Shopify. `body` is sent as JSON, or as a URL-encoded form when
/// `headers` give the `Content-Type` [`FORM_CONTENT_TYPE`]: it is then an
/// object of strings, one per field.
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

/// Sends `POST` requests, with a JSON or a form body. Shopify OAuth and Admin
/// GraphQL only need that.
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

impl HttpTransport for ReqwestTransport {
    fn post_json(&self, request: OutboundRequest) -> BoxFuture<'_, Result<OutboundResponse>> {
        Box::pin(async move {
            let form = request.headers.iter().any(|(name, value)| {
                name.eq_ignore_ascii_case("content-type") && value.eq_ignore_ascii_case(FORM_CONTENT_TYPE)
            });
            let mut builder = if form {
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
    async fn sends_json_otherwise() {
        let request = received(
            vec![("Content-Type".to_string(), "application/json".to_string())],
            json!({ "query": "{ shop { name } }" }),
        )
        .await;
        assert!(request.ends_with("\r\n\r\n{\"query\":\"{ shop { name } }\"}"), "{request}");
    }
}
