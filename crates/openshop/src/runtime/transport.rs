//! Outbound HTTP to Shopify, behind a trait so tests can run without network access.

use std::time::Duration;

use anyhow::Result;
use futures::future::BoxFuture;
use serde_json::Value;

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

/// Sends JSON `POST` requests. Shopify OAuth and Admin GraphQL only need that.
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
            let mut builder = self.client.post(&request.url).json(&request.body);
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
