//! Response compression (gzip or Brotli) for bodies large enough to benefit
//! from it, while they are sent: [`CompressLarge`].

use std::convert::Infallible;
use std::future::ready;
use std::task::{Context, Poll};

use axum::extract::Request;
use axum::http::HeaderValue;
use axum::http::header::{ACCEPT_ENCODING, CONTENT_ENCODING, ETAG};
use axum::response::{IntoResponse, Response};
use futures::future::BoxFuture;
use tower::{Service, ServiceExt, service_fn};
use tower_http::compression::predicate::{Predicate, SizeAbove};
use tower_http::compression::{Compression, CompressionLevel};

/// Smaller responses are sent as they are: compressing a body that fits in one
/// packet costs far more CPU than it saves, and often makes it larger.
const MIN_COMPRESSED_BODY_BYTES: u16 = 1024;

/// The level of a body compressed while it is sent: tower-http's default,
/// Brotli quality 4 and gzip level 6.
const STREAMED_LEVEL: CompressionLevel = CompressionLevel::Default;

/// The weak form of an ETag.
fn weak_etag(etag: &HeaderValue) -> HeaderValue {
    if etag.as_bytes().starts_with(b"W/") {
        return etag.clone();
    }
    HeaderValue::from_bytes(&[b"W/", etag.as_bytes()].concat()).unwrap_or_else(|_| etag.clone())
}

/// A compressed body is another representation than the one the ETag names, so
/// the ETag of a response that was compressed is weak.
fn weaken_etag(response: &mut Response) {
    if !response.headers().contains_key(CONTENT_ENCODING) {
        return;
    }
    if let Some(etag) = response.headers().get(ETAG).map(weak_etag) {
        response.headers_mut().insert(ETAG, etag);
    }
}

/// Compresses the responses of the inner service when the client accepts it.
///
/// tower-http wraps every response it is given in a body of several kilobytes,
/// compressed or not, so the small ones never reach it.
#[derive(Clone)]
pub(crate) struct CompressLarge<S>(pub(crate) S);

impl<S> Service<Request> for CompressLarge<S>
where
    S: Service<Request, Response = Response, Error = Infallible>,
    S::Future: Send + 'static,
{
    type Response = Response;
    type Error = Infallible;
    type Future = BoxFuture<'static, Result<Response, Infallible>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        self.0.poll_ready(cx)
    }

    fn call(&mut self, request: Request) -> Self::Future {
        // All that tower-http reads of the request, kept until the response is known.
        let mut accepted = Request::new(());
        for encoding in request.headers().get_all(ACCEPT_ENCODING) {
            accepted.headers_mut().append(ACCEPT_ENCODING, encoding.clone());
        }
        let response = self.0.call(request);
        Box::pin(async move {
            let mut response = response.await?;
            if !SizeAbove::new(u64::from(MIN_COMPRESSED_BODY_BYTES)).should_compress(&response) {
                return Ok(response);
            }
            let respond = service_fn(move |_: Request<()>| ready(Ok::<_, Infallible>(std::mem::take(&mut response))));
            let mut compressed = Compression::new(respond)
                .quality(STREAMED_LEVEL)
                .oneshot(accepted)
                .await?
                .into_response();
            weaken_etag(&mut compressed);
            Ok(compressed)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_compressed_body_has_a_weak_etag() {
        assert_eq!(weak_etag(&HeaderValue::from_static("\"a\"")), "W/\"a\"");
        assert_eq!(weak_etag(&HeaderValue::from_static("W/\"a\"")), "W/\"a\"");
    }
}
