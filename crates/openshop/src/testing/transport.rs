//! A scripted replacement for Shopify HTTP calls.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use serde_json::Value;
use shopify_api::transport::{BoxFuture, HttpRequest, HttpResponse, Method, Transport, TransportError};

use super::shopify::{FakeShopify, GraphqlCall};
use crate::locked;

/// The `Content-Type` of a request whose body is a URL-encoded form, such as
/// the client credentials request.
pub const FORM_CONTENT_TYPE: &str = "application/x-www-form-urlencoded";

/// A request sent to Shopify, as [`MockTransport`] records it. `body` is the
/// JSON of the request, or, for a URL-encoded form, an object of strings, one
/// per field; `Null` without a body. `Debug` hides the headers and the body
/// fields that hold a secret: an access token, a client secret, an
/// authorization code.
#[derive(Clone, PartialEq)]
pub struct OutboundRequest {
    pub method: &'static str,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Value,
}

impl OutboundRequest {
    fn of(request: &HttpRequest) -> Self {
        let form = request
            .header("content-type")
            .and_then(|value| value.split(';').next())
            .is_some_and(|media_type| media_type.trim().eq_ignore_ascii_case(FORM_CONTENT_TYPE));
        let body = if request.body.is_empty() {
            Value::Null
        } else if form {
            Value::Object(
                request
                    .form()
                    .into_iter()
                    .map(|(name, value)| (name, Value::String(value)))
                    .collect(),
            )
        } else {
            request
                .json()
                .unwrap_or_else(|| Value::String(String::from_utf8_lossy(&request.body).into_owned()))
        };
        Self {
            method: match request.method {
                Method::Get => "GET",
                _ => "POST",
            },
            url: request.url.clone(),
            headers: request.headers.clone(),
            body,
        }
    }

    /// The first value of a header, whatever the case of its name.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(given, _)| given.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

impl std::fmt::Debug for OutboundRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use crate::runtime::redact::{is_secret_name, redacted_json};
        let headers: Vec<(&str, &str)> = self
            .headers
            .iter()
            .map(|(name, value)| {
                let shown = if is_secret_name(name) {
                    "[redacted]"
                } else {
                    value.as_str()
                };
                (name.as_str(), shown)
            })
            .collect();
        f.debug_struct("OutboundRequest")
            .field("method", &self.method)
            .field("url", &self.url)
            .field("headers", &headers)
            .field("body", &redacted_json(&self.body))
            .finish()
    }
}

/// What [`MockTransport`] answers: a status and a body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundResponse {
    pub status: u16,
    pub body: String,
}

impl OutboundResponse {
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

type Responder = Arc<dyn Fn(&OutboundRequest) -> Result<OutboundResponse> + Send + Sync>;

/// Records every outbound request and answers it: a GraphQL operation its
/// [`shopify`](Self::shopify) answers by name, anything else with the
/// configured responder. Without a responder, a GraphQL operation nothing
/// answers gets a GraphQL error that names it, and any other request a `404`.
/// A responder that fails is a request that did not reach Shopify.
#[derive(Default)]
pub struct MockTransport {
    responder: Mutex<Option<Responder>>,
    shopify: FakeShopify,
    requests: Mutex<Vec<OutboundRequest>>,
    delay: Mutex<Duration>,
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

    /// Answers what [`shopify`](Self::shopify) does not: the requests that
    /// are not GraphQL (the token requests), and the operations it has no
    /// answer for.
    pub fn respond_with(
        &self,
        responder: impl Fn(&OutboundRequest) -> Result<OutboundResponse> + Send + Sync + 'static,
    ) {
        *locked(&self.responder) = Some(Arc::new(responder));
    }

    /// The Admin API behind this transport, which answers GraphQL operations
    /// by name before the responder does. `ctx.shopify` of a
    /// [`TestContext`](super::TestContext). Clones share their answers.
    pub fn shopify(&self) -> FakeShopify {
        self.shopify.clone()
    }

    /// Makes every request wait `delay` before it is answered, as a slow
    /// Shopify would. The request is recorded when it is sent.
    pub fn delay_responses(&self, delay: Duration) {
        *locked(&self.delay) = delay;
    }

    /// Answers every request [`shopify`](Self::shopify) does not with the
    /// same JSON body and status 200.
    pub fn respond_json(&self, body: Value) {
        self.respond_with(move |_| Ok(Self::json(200, body.clone())));
    }

    pub fn requests(&self) -> Vec<OutboundRequest> {
        locked(&self.requests).clone()
    }

    pub fn last_request(&self) -> Option<OutboundRequest> {
        self.requests().pop()
    }

    pub fn clear(&self) {
        locked(&self.requests).clear();
    }
}

impl Transport for MockTransport {
    fn send(&self, request: HttpRequest) -> BoxFuture<'_, std::result::Result<HttpResponse, TransportError>> {
        let responder = locked(&self.responder).clone();
        let request = OutboundRequest::of(&request);
        locked(&self.requests).push(request.clone());
        let delay = *locked(&self.delay);
        let shopify = self.shopify.clone();
        Box::pin(async move {
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            let call = GraphqlCall::of(&request);
            let answered = call.as_ref().and_then(|call| shopify.answer(call));
            let response = match (answered, responder, &call) {
                (Some(answer), _, _) => answer,
                (None, Some(responder), _) => responder(&request),
                (None, None, Some(call)) => Ok(shopify.unanswered_response(call)),
                (None, None, None) => Ok(OutboundResponse {
                    status: 404,
                    body: "not mocked".to_string(),
                }),
            };
            match response {
                Ok(response) => Ok(HttpResponse::new(response.status, response.body.into_bytes())
                    .with_header("Content-Type", "application/json")),
                Err(error) => Err(TransportError::new(format!("{error:#}"))),
            }
        })
    }
}
