//! Response compression (gzip or Brotli) for bodies large enough to benefit from it.

use std::convert::Infallible;
use std::future::ready;
use std::task::{Context, Poll};

use axum::extract::Request;
use axum::http::header::ACCEPT_ENCODING;
use axum::response::{IntoResponse, Response};
use futures::future::BoxFuture;
use tower::{Service, ServiceExt, service_fn};
use tower_http::compression::Compression;
use tower_http::compression::predicate::{Predicate, SizeAbove};

/// Smaller responses are sent as they are: compressing a body that fits in one
/// packet costs far more CPU than it saves, and often makes it larger.
const MIN_COMPRESSED_BODY_BYTES: u16 = 1024;

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
            if !SizeAbove::new(MIN_COMPRESSED_BODY_BYTES).should_compress(&response) {
                return Ok(response);
            }
            let respond = service_fn(move |_: Request<()>| ready(Ok::<_, Infallible>(std::mem::take(&mut response))));
            Ok(Compression::new(respond).oneshot(accepted).await?.into_response())
        })
    }
}
