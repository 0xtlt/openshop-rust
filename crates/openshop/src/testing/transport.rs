//! A scripted replacement for Shopify HTTP calls.

use std::sync::{Arc, Mutex};

use anyhow::Result;
use futures::future::BoxFuture;
use serde_json::Value;

use crate::runtime::{HttpTransport, OutboundRequest, OutboundResponse};

type Responder = Arc<dyn Fn(&OutboundRequest) -> Result<OutboundResponse> + Send + Sync>;

/// Records every outbound request and answers with the configured responder.
/// Without a responder every request gets a 404.
pub struct MockTransport {
    responder: Mutex<Responder>,
    requests: Mutex<Vec<OutboundRequest>>,
}

impl Default for MockTransport {
    fn default() -> Self {
        Self {
            responder: Mutex::new(Arc::new(|_| {
                Ok(OutboundResponse {
                    status: 404,
                    body: "not mocked".to_string(),
                })
            })),
            requests: Mutex::new(Vec::new()),
        }
    }
}

impl MockTransport {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// A JSON response with the given status.
    pub fn json(status: u16, body: Value) -> OutboundResponse {
        OutboundResponse {
            status,
            body: body.to_string(),
        }
    }

    pub fn respond_with(
        &self,
        responder: impl Fn(&OutboundRequest) -> Result<OutboundResponse> + Send + Sync + 'static,
    ) {
        *self.responder.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Arc::new(responder);
    }

    /// Answers every request with the same JSON body and status 200.
    pub fn respond_json(&self, body: Value) {
        self.respond_with(move |_| Ok(Self::json(200, body.clone())));
    }

    pub fn requests(&self) -> Vec<OutboundRequest> {
        self.requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn last_request(&self) -> Option<OutboundRequest> {
        self.requests().pop()
    }

    pub fn clear(&self) {
        self.requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }
}

impl HttpTransport for MockTransport {
    fn post_json(&self, request: OutboundRequest) -> BoxFuture<'_, Result<OutboundResponse>> {
        let responder = self
            .responder
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        self.requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(request.clone());
        Box::pin(async move { responder(&request) })
    }
}
