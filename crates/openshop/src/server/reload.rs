//! Development: a page of the admin reloads when the server is started again.
//!
//! `openshop dev` starts the server again when the sources change. A page is
//! served with the version of the server, the process that renders it, and
//! listens to [`PATH`], an event stream that names the version being served:
//! a page that hears another one reloads.

use std::convert::Infallible;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::Extension;
use axum::extract::Request;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures::Stream;
use futures::future::BoxFuture;
use futures::stream::unfold;
use tokio_util::sync::CancellationToken;
use tower::Service;
use uuid::Uuid;

/// The event stream a page of the admin UI listens to in development.
pub(crate) const PATH: &str = "/__openshop/reload";

/// How long a page waits before it listens again to a server that went away.
const RETRY: Duration = Duration::from_secs(1);

/// Tells the event streams of a server that it closes: a server waits for the
/// requests in flight, and a page would listen until the end.
#[derive(Clone)]
pub(crate) struct WithClosing<S> {
    pub(crate) inner: S,
    pub(crate) closing: Closing,
}

/// The service is cloned for each request: shared, where a token of its own
/// would take at each of them the lock all the tokens have in common.
#[derive(Clone)]
pub(crate) struct Closing(pub(crate) Arc<CancellationToken>);

impl<S, B> Service<Request<B>> for WithClosing<S>
where
    S: Service<Request<B>>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), S::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut request: Request<B>) -> Self::Future {
        if request.uri().path() == PATH {
            request.extensions_mut().insert(self.closing.clone());
        }
        self.inner.call(request)
    }
}

/// What a development server serves, as a version a page can compare: the
/// process, which another start replaces.
pub(crate) struct Served {
    instance: String,
}

impl Served {
    pub(crate) fn new() -> Self {
        Self {
            instance: Uuid::new_v4().simple().to_string(),
        }
    }

    /// The version a page loads now.
    fn version(&self) -> &str {
        &self.instance
    }

    /// Adds to the page the script that reloads it when the version it was
    /// served is no longer the one being served.
    pub(crate) fn inject(&self, html: &str) -> String {
        let version = self.version();
        let script = format!(
            r#"<script>
      (() => {{
        const listen = () => {{
          const events = new EventSource("{PATH}");
          events.onmessage = (event) => {{
            if (event.data !== "{version}") location.reload();
          }};
          // A tunnel answers for a server that restarts, and the browser then gives up.
          events.onerror = () => {{
            if (events.readyState === EventSource.CLOSED) setTimeout(listen, {retry});
          }};
        }};
        listen();
      }})();
    </script>
  "#,
            retry = RETRY.as_millis(),
        );
        match html.find("</head>") {
            Some(head_end) => format!("{}{script}{}", &html[..head_end], &html[head_end..]),
            None => format!("{html}{script}"),
        }
    }
}

/// A page listening: told the version once, then kept until the server closes.
struct Listener {
    served: Arc<Served>,
    told: bool,
    closing: BoxFuture<'static, ()>,
}

impl Listener {
    /// The version being served, then nothing until the server closes: the
    /// next server is another version.
    async fn next(mut self) -> Option<(Result<Event, Infallible>, Self)> {
        if self.told {
            self.closing.as_mut().await;
            return None;
        }
        self.told = true;
        let event = Event::default().retry(RETRY).data(self.served.version());
        Some((Ok(event), self))
    }
}

/// `GET` [`PATH`]: the version being served, until the server closes.
pub(crate) async fn listen(
    served: Arc<Served>,
    closing: Option<Extension<Closing>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let listener = Listener {
        served,
        told: false,
        closing: match closing {
            Some(Extension(Closing(closing))) => Box::pin(CancellationToken::clone(&closing).cancelled_owned()),
            // A router served by another server than `serve`: until the page goes away.
            None => Box::pin(std::future::pending()),
        },
    };
    Sse::new(unfold(listener, Listener::next)).keep_alive(KeepAlive::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn served() -> Served {
        Served {
            instance: "server".to_string(),
        }
    }

    #[test]
    fn two_servers_do_not_serve_the_same_version() {
        assert_ne!(Served::new().version(), Served::new().version());
    }

    #[test]
    fn adds_the_script_at_the_end_of_the_head() {
        let html = served().inject("<html><head><title>OpenShop</title></head><body></body></html>");
        let script = html.find("<script>").expect("a script");
        assert!(html.find("</title>").unwrap() < script && script < html.find("</head>").unwrap());
        assert!(html.contains(r#"new EventSource("/__openshop/reload")"#), "{html}");
        assert!(html.contains(r#"event.data !== "server""#), "{html}");

        // A page without a head is still given the script.
        assert!(served().inject("<div></div>").starts_with("<div></div><script>"));
    }
}
