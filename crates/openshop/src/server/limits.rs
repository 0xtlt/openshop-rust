//! What one request, and all of them together, may take from the server.
//!
//! Both limits cover a request until the head of its response is ready. A body
//! that is already streaming (the event stream of a development page, a
//! download) is not counted: its handler returned.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Request, State};
use axum::http::header::RETRY_AFTER;
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use tokio::sync::Semaphore;

use super::http::json_error;

/// A request with no response after this long is answered with `503`, unless
/// the application sets `request_timeout`. An MCP tool may run for 30 seconds.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// Requests served at once, unless the application sets `max_concurrent_requests`.
pub const DEFAULT_MAX_CONCURRENT_REQUESTS: usize = 1024;

/// The probe of the platform must see the server answer when it is full.
const ALWAYS_SERVED: &str = "/health";

pub(crate) async fn time_out(State(limit): State<Duration>, request: Request, next: Next) -> Response {
    match tokio::time::timeout(limit, next.run(request)).await {
        Ok(response) => response,
        Err(_) => json_error(StatusCode::SERVICE_UNAVAILABLE, "Request timed out"),
    }
}

pub(crate) async fn shed_excess(State(slots): State<Arc<Semaphore>>, request: Request, next: Next) -> Response {
    if request.uri().path() == ALWAYS_SERVED {
        return next.run(request).await;
    }
    // Waiting would only move the queue into memory: the client is told to retry.
    let Ok(_slot) = slots.try_acquire_owned() else {
        let mut response = json_error(StatusCode::SERVICE_UNAVAILABLE, "Server is busy, try again");
        response.headers_mut().insert(RETRY_AFTER, HeaderValue::from_static("1"));
        return response;
    };
    next.run(request).await
}
