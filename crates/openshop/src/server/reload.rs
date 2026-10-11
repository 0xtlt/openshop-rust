//! Development: a page of the admin UI reloads when what it was served changes.
//!
//! `openshop dev` starts the server again when the sources change, and trunk
//! builds the bundle of the application again. The shell is served with the
//! version it was read at and listens to [`PATH`], an event stream that names
//! the version being served: a page that hears another one reloads.

use std::convert::Infallible;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
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

use super::UiSource;

/// The event stream a page of the admin UI listens to in development.
pub(crate) const PATH: &str = "/__openshop/reload";

/// How often a bundle served from disk is looked at.
const POLL: Duration = Duration::from_millis(250);
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

/// What a development server serves, as a version a page can compare.
pub(crate) struct Served {
    /// Another process serves under another name.
    instance: String,
    /// The shell of a bundle served from disk. It names the files of a build
    /// by their content: another build is another shell.
    index: Option<PathBuf>,
}

impl Served {
    pub(crate) fn new(ui: &UiSource) -> Self {
        Self {
            instance: Uuid::new_v4().simple().to_string(),
            index: match ui {
                UiSource::Dir(directory) => Some(directory.join("index.html")),
                _ => None,
            },
        }
    }

    /// The version a page would load now. `None` while a build replaces the bundle.
    fn version(&self) -> Option<String> {
        let built = match &self.index {
            Some(index) => {
                let mut hasher = DefaultHasher::new();
                std::fs::read(index).ok()?.hash(&mut hasher);
                hasher.finish()
            }
            None => 0,
        };
        Some(format!("{}-{built:x}", self.instance))
    }

    /// Adds to the shell the script that reloads the page when the version it
    /// was served is no longer the one being served.
    pub(crate) fn inject(&self, html: &str) -> String {
        let version = self.version().unwrap_or_default();
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

/// A page listening: what it was last told, until the server closes.
struct Listener {
    served: Arc<Served>,
    told: Option<String>,
    closing: BoxFuture<'static, ()>,
}

impl Listener {
    /// The next version to tell the page: the current one first, then each new one.
    async fn next(mut self) -> Option<(Result<Event, Infallible>, Self)> {
        // trunk moves the files of a build into place one by one: a version is
        // told once it has been seen twice.
        let mut settling = None;
        loop {
            let version = self.served.version();
            if version.is_some() && version != self.told && (self.told.is_none() || version == settling) {
                self.told = version.clone();
                let event = Event::default().retry(RETRY).data(version.unwrap_or_default());
                return Some((Ok(event), self));
            }
            settling = version.filter(|version| Some(version) != self.told.as_ref());
            tokio::select! {
                _ = &mut self.closing => return None,
                _ = tokio::time::sleep(POLL) => {}
            }
        }
    }
}

/// `GET` [`PATH`]: the version being served, then each one that replaces it.
pub(crate) async fn listen(
    served: Arc<Served>,
    closing: Option<Extension<Closing>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let listener = Listener {
        served,
        told: None,
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

    fn served(index: Option<PathBuf>) -> Served {
        Served {
            instance: "server".to_string(),
            index,
        }
    }

    #[test]
    fn names_the_server_and_the_build_of_a_bundle_on_disk() {
        assert_eq!(served(None).version().as_deref(), Some("server-0"));

        let directory = std::env::temp_dir().join(format!("openshop-reload-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let on_disk = served(Some(directory.join("index.html")));
        // A build is replacing the bundle: there is nothing to load yet.
        assert_eq!(on_disk.version(), None);

        let build = |script: &str| {
            std::fs::write(directory.join("index.html"), format!("<script src={script}></script>")).unwrap();
            on_disk.version().expect("a version")
        };
        let version = build("app-1a2b.js");
        assert!(version.starts_with("server-") && version != "server-0", "{version}");
        assert_ne!(build("app-3c4d.js"), version);
        // A build that changes nothing is the same version: the page stays as it is.
        assert_eq!(build("app-1a2b.js"), version);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn two_servers_do_not_serve_the_same_version() {
        let ui = UiSource::Embedded;
        assert_ne!(Served::new(&ui).version(), Served::new(&ui).version());
    }

    #[test]
    fn adds_the_script_at_the_end_of_the_head() {
        let html = served(None).inject("<html><head><title>OpenShop</title></head><body></body></html>");
        let script = html.find("<script>").expect("a script");
        assert!(html.find("</title>").unwrap() < script && script < html.find("</head>").unwrap());
        assert!(html.contains(r#"new EventSource("/__openshop/reload")"#), "{html}");
        assert!(html.contains(r#"event.data !== "server-0""#), "{html}");

        // A shell without a head is still given the script.
        assert!(served(None).inject("<div></div>").starts_with("<div></div><script>"));
    }
}
