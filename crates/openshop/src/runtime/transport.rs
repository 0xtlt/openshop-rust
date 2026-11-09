//! Outbound HTTP to Shopify: the [`Transport`] of `shopify_api`, which every
//! exchange with Shopify goes through, and which tests replace.
//!
//! `shopify_api` is used without an HTTP client of its own: its requests go
//! through [`ReqwestTransport`], on the `reqwest` and the TLS of the rest of
//! the application.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use futures::StreamExt;
use shopify_api::transport::{BoxFuture, StreamingResponse};
pub use shopify_api::transport::{HttpRequest, HttpResponse, Method, Transport, TransportError};

/// The `User-Agent` of every request to Shopify.
pub(crate) const USER_AGENT: &str = concat!("openshop-rust/", env!("CARGO_PKG_VERSION"));

/// The most of an answer that is read whole: 10 MiB. A GraphQL answer of
/// Shopify or a token are far below. A server that sends more is broken or
/// hostile, and the body of such an answer is held in memory. The results of
/// a bulk operation are streamed, whatever their size.
const MAX_RESPONSE_BYTES: usize = 10 * 1024 * 1024;

/// The transport of an application, as `shopify_api` takes one by value: each
/// app of `shopify_api` shares the one [`Arc`].
#[derive(Clone)]
pub(crate) struct SharedTransport(pub Arc<dyn Transport>);

impl Transport for SharedTransport {
    fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, TransportError>> {
        self.0.send(request)
    }

    fn send_streaming(&self, request: HttpRequest) -> BoxFuture<'_, Result<StreamingResponse, TransportError>> {
        self.0.send_streaming(request)
    }
}

/// Sends the requests of `shopify_api` with `reqwest`: no redirections, a
/// 10-second connection timeout and a 60-second request timeout, unless a
/// request asks for another.
///
/// Every request carries a secret, a token or a code, in its body or in
/// `X-Shopify-Access-Token`, which a redirection would send on to another
/// host: a `3xx` is the answer, followed by no one.
pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    /// Fails when `reqwest` cannot build the client, rather than fall back
    /// to one that follows redirections and never times out.
    pub fn new() -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("[openshop] Failed to build the HTTP client of Shopify requests")?;
        Ok(Self { client })
    }

    fn builder(&self, request: &HttpRequest) -> reqwest::RequestBuilder {
        let mut builder = match request.method {
            Method::Get => self.client.get(&request.url),
            _ => self.client.post(&request.url),
        };
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        if !request.body.is_empty() {
            builder = builder.body(request.body.clone());
        }
        builder
    }
}

/// A failure of `reqwest`, without its address, which may hold a signature.
fn failed(error: reqwest::Error) -> TransportError {
    let error = error.without_url();
    if error.is_timeout() {
        TransportError::timeout(error.to_string()).with_source(error)
    } else {
        TransportError::new(error.to_string()).with_source(error)
    }
}

fn headers(response: &reqwest::Response) -> Vec<(String, String)> {
    response
        .headers()
        .iter()
        .filter_map(|(name, value)| Some((name.to_string(), value.to_str().ok()?.to_string())))
        .collect()
}

/// The body of `response`, read as it arrives and given up on as soon as it
/// is over `limit` bytes: a `Content-Length` that is too large is refused
/// before anything is read, and a body without one, or lying about it, is
/// counted.
pub(crate) async fn read_body(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>, TransportError> {
    let too_large = || {
        TransportError::new(format!(
            "[openshop] The response is larger than {limit} bytes: not read"
        ))
    };
    if response.content_length().is_some_and(|length| length > limit as u64) {
        return Err(too_large());
    }
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(failed)? {
        if body.len() + chunk.len() > limit {
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

impl Transport for ReqwestTransport {
    fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, TransportError>> {
        Box::pin(async move {
            let mut builder = self.builder(&request);
            if let Some(timeout) = request.timeout {
                builder = builder.timeout(timeout);
            }
            let response = builder.send().await.map_err(failed)?;
            let status = response.status().as_u16();
            let headers = headers(&response);
            let mut answer = HttpResponse::new(status, read_body(response, MAX_RESPONSE_BYTES).await?);
            for (name, value) in headers {
                answer = answer.with_header(name, value);
            }
            Ok(answer)
        })
    }

    /// The answer once its headers arrive, its body read as it is polled:
    /// `request.timeout` bounds the wait for the headers and for each chunk.
    fn send_streaming(&self, request: HttpRequest) -> BoxFuture<'_, Result<StreamingResponse, TransportError>> {
        Box::pin(async move {
            let wait = request.timeout;
            // The timeout of the client would cover the whole body.
            let builder = self.builder(&request).timeout(Duration::from_secs(60 * 60 * 24));
            let response = match wait {
                Some(wait) => tokio::time::timeout(wait, builder.send())
                    .await
                    .map_err(|_| TransportError::timeout(format!("no answer within {wait:?}")))?,
                None => builder.send().await,
            }
            .map_err(failed)?;
            let status = response.status().as_u16();
            let headers = headers(&response);
            let body = futures::stream::unfold(Some(response), move |response| async move {
                let mut response = response?;
                let chunk = match wait {
                    Some(wait) => match tokio::time::timeout(wait, response.chunk()).await {
                        Ok(chunk) => chunk.map_err(failed),
                        Err(_) => Err(TransportError::timeout(format!("the body stalled for {wait:?}"))),
                    },
                    None => response.chunk().await.map_err(failed),
                };
                match chunk {
                    Ok(Some(chunk)) => Some((Ok(chunk.to_vec()), Some(response))),
                    Ok(None) => None,
                    // The stream ends after its error.
                    Err(error) => Some((Err(error), None)),
                }
            })
            .boxed();
            let mut answer = StreamingResponse::new(status, body);
            for (name, value) in headers {
                answer = answer.with_header(name, value);
            }
            Ok(answer)
        })
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    /// A local server that answers one request with `head`, then `body`;
    /// its address.
    async fn answering(head: String, body: Vec<u8>) -> String {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let url = format!("http://{}/admin/oauth/access_token", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = [0u8; 4096];
            let _ = stream.read(&mut buffer).await.unwrap();
            let _ = stream.write_all(head.as_bytes()).await;
            let _ = stream.write_all(&body).await;
            let _ = stream.shutdown().await;
        });
        url
    }

    fn post(url: &str) -> HttpRequest {
        HttpRequest::new(Method::Post, url)
            .with_header("X-Shopify-Access-Token", "offline-token")
            .with_body(b"client_secret=dev-app-secret".to_vec())
    }

    #[tokio::test]
    async fn a_redirection_is_the_answer_and_is_never_followed() {
        let elsewhere = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let location = format!("http://{}/elsewhere", elsewhere.local_addr().unwrap());
        let head = format!("HTTP/1.1 307 Temporary Redirect\r\nlocation: {location}\r\ncontent-length: 0\r\n\r\n");
        let url = answering(head, Vec::new()).await;

        let response = ReqwestTransport::new().unwrap().send(post(&url)).await.unwrap();
        assert_eq!(response.status, 307);
        let reached = tokio::time::timeout(Duration::from_millis(300), elsewhere.accept())
            .await
            .is_ok();
        assert!(!reached, "the request was sent on");
    }

    #[tokio::test]
    async fn what_a_transport_answers_whole_is_capped_at_ten_mebibytes() {
        assert_eq!(MAX_RESPONSE_BYTES, 10 * 1024 * 1024);
        let head = format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n", MAX_RESPONSE_BYTES + 1);
        let url = answering(head, Vec::new()).await;
        let error = ReqwestTransport::new().unwrap().send(post(&url)).await.unwrap_err();
        assert!(error.to_string().contains("larger than 10485760 bytes"), "{error}");

        // A body without a length that goes on is counted.
        let url = answering(
            "HTTP/1.1 200 OK\r\nconnection: close\r\n\r\n".to_string(),
            vec![b'x'; 2048],
        )
        .await;
        let response = reqwest::get(url).await.unwrap();
        assert!(read_body(response, 1024).await.is_err());
    }

    #[tokio::test]
    async fn a_streamed_answer_is_read_as_it_is_polled() {
        let url = answering(
            "HTTP/1.1 200 OK\r\nx-request-id: abc\r\nconnection: close\r\n\r\n".to_string(),
            b"{\"id\":1}\n{\"id\":2}\n".to_vec(),
        )
        .await;
        let mut response = ReqwestTransport::new()
            .unwrap()
            .send_streaming(HttpRequest::new(Method::Get, &url))
            .await
            .unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.header("x-request-id"), Some("abc"));
        let mut body = Vec::new();
        while let Some(chunk) = response.body.next().await {
            body.extend(chunk.unwrap());
        }
        assert_eq!(body, b"{\"id\":1}\n{\"id\":2}\n");
    }
}
