//! Response compression (gzip or Brotli) for bodies large enough to benefit from it.

use std::convert::Infallible;
use std::future::ready;
use std::task::{Context, Poll};

use axum::extract::Request;
use axum::http::{HeaderMap, HeaderValue};
use axum::http::header::{ACCEPT_ENCODING, CONTENT_ENCODING, ETAG};
use axum::response::{IntoResponse, Response};
use futures::future::BoxFuture;
use tower::{Service, ServiceExt, service_fn};
use tower_http::compression::Compression;
use tower_http::compression::predicate::{Predicate, SizeAbove};

/// Smaller responses are sent as they are: compressing a body that fits in one
/// packet costs far more CPU than it saves, and often makes it larger.
const MIN_COMPRESSED_BODY_BYTES: u16 = 1024;

/// Whether a body of this length is large enough to be compressed.
pub(crate) fn is_compressible(body_len: usize) -> bool {
    body_len > usize::from(MIN_COMPRESSED_BODY_BYTES)
}

/// Whether the request accepts an encoding this server compresses with.
pub(crate) fn accepts_compression(headers: &HeaderMap) -> bool {
    headers
        .get_all(ACCEPT_ENCODING)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(|coding| coding.split(';').next().unwrap_or_default().trim())
        .any(|coding| coding.eq_ignore_ascii_case("gzip") || coding.eq_ignore_ascii_case("br") || coding == "*")
}

/// The weak form of an ETag.
pub(crate) fn weak_etag(etag: &HeaderValue) -> HeaderValue {
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
            let mut compressed = Compression::new(respond).oneshot(accepted).await?.into_response();
            weaken_etag(&mut compressed);
            Ok(compressed)
        })
    }
}
