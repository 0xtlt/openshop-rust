//! Reports failures to Sentry: runs that fail, handlers that fail, panics.
//!
//! The application turns it on with the `sentry` setting of `app!`, which
//! takes its DSN. While no client is bound, and without the `sentry` feature,
//! nothing here does anything.

#[cfg(feature = "sentry")]
mod client;
#[cfg(feature = "sentry")]
mod integration;
#[cfg(feature = "sentry")]
mod transport;

use std::fmt;

#[cfg(feature = "sentry")]
pub use self::client::{SentryConfig, SentryGuard};

use crate::app::OpenShopConfig;

#[cfg(feature = "sentry")]
use {
    axum::extract::MatchedPath,
    sentry::protocol::Breadcrumb,
    sentry::{Hub, Level, SentryFuture, SentryFutureExt, User},
    std::sync::Arc,
    std::task::{Context, Poll},
};

/// A panic the framework caught and turned into an error. The panic hook
/// reports it where it happens, with its stack: it is not reported again as
/// the failure it became.
#[derive(Debug)]
pub(crate) struct Panicked(pub(crate) String);

impl Panicked {
    /// The error a caught panic becomes: `handler panicked`.
    pub(crate) fn error(what: impl Into<String>) -> anyhow::Error {
        Self(what.into()).into()
    }
}

impl fmt::Display for Panicked {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for Panicked {}

/// Starts reporting for a process of an application that gives a DSN. The
/// process is `dev`, `start` or `worker`: a tag of everything it reports.
#[cfg(feature = "sentry")]
pub(crate) fn start(config: &OpenShopConfig, process: &str) -> Option<SentryGuard> {
    let production = super::RuntimeEnv::from_env().production;
    let guard = config.sentry.as_ref()?.init(production);
    Hub::main().configure_scope(|scope| scope.set_tag("process", process));
    Some(guard)
}

#[cfg(not(feature = "sentry"))]
pub(crate) fn start(_config: &OpenShopConfig, _process: &str) -> Option<()> {
    None
}

/// A scope for a unit of work, when a client is bound to report to.
#[cfg(feature = "sentry")]
fn unit_hub() -> Option<Arc<Hub>> {
    Hub::with_active(|hub| Some(Arc::new(Hub::new_from_top(hub))))
}

/// Runs a unit of work, such as the run of a flow, with a scope of its own:
/// what it names and tags, and the trail its log lines leave, stay with the
/// events it reports.
pub(crate) async fn isolated<F: Future>(future: F) -> F::Output {
    #[cfg(feature = "sentry")]
    if let Some(hub) = unit_hub() {
        return future.bind_hub(hub).await;
    }
    future.await
}

/// What the events of a unit of work say about it.
pub(crate) struct Description<'a> {
    #[cfg(feature = "sentry")]
    scope: &'a mut sentry::Scope,
    #[cfg(not(feature = "sentry"))]
    scope: std::marker::PhantomData<&'a ()>,
}

impl Description<'_> {
    /// The name of the unit of work: `flow syncOrders`,
    /// `webhook orders/create`, `route GET /ping`. The kind is also a tag.
    pub(crate) fn unit(&mut self, kind: &str, name: impl fmt::Display) -> &mut Self {
        #[cfg(feature = "sentry")]
        {
            let name = name.to_string();
            self.scope.set_transaction(Some(&format!("{kind} {name}")));
            self.scope.set_tag(kind, name);
        }
        #[cfg(not(feature = "sentry"))]
        let _ = (kind, name, &self.scope);
        self
    }

    /// The shop the unit of work runs for. It is the user of its events: an
    /// issue counts the shops it reaches.
    pub(crate) fn shop(&mut self, shop: &str, shopify_app: &str) -> &mut Self {
        #[cfg(feature = "sentry")]
        {
            self.scope.set_tag("shop", shop);
            self.scope.set_tag("shopify_app", shopify_app);
            self.scope.set_user(Some(User {
                id: Some(shop.to_string()),
                ..Default::default()
            }));
        }
        #[cfg(not(feature = "sentry"))]
        let _ = (shop, shopify_app);
        self
    }

    pub(crate) fn tag(&mut self, key: &str, value: impl fmt::Display) -> &mut Self {
        #[cfg(feature = "sentry")]
        self.scope.set_tag(key, value);
        #[cfg(not(feature = "sentry"))]
        let _ = (key, value);
        self
    }
}

/// Describes the unit of work that is running, in one write of its scope.
pub(crate) fn describe(describe: impl FnOnce(&mut Description<'_>)) {
    #[cfg(feature = "sentry")]
    Hub::with_active(|hub| hub.configure_scope(|scope| describe(&mut Description { scope })));
    #[cfg(not(feature = "sentry"))]
    describe(&mut Description {
        scope: std::marker::PhantomData,
    });
}

/// Reports a failure the framework answered or recorded.
pub(crate) fn error(error: &anyhow::Error) {
    #[cfg(feature = "sentry")]
    Hub::with_active(|hub| capture(hub, error, Level::Error));
    #[cfg(not(feature = "sentry"))]
    let _ = error;
}

/// Reports a failure that is not final: the attempt of a run that will be
/// made again.
pub(crate) fn warning(error: &anyhow::Error) {
    #[cfg(feature = "sentry")]
    Hub::with_active(|hub| capture(hub, error, Level::Warning));
    #[cfg(not(feature = "sentry"))]
    let _ = error;
}

/// Reports the failure of something that is not running: a run nobody
/// attempted, a cron that queued nothing. The event has a scope of its own.
pub(crate) fn detached(describe: impl FnOnce(&mut Description<'_>), error: &anyhow::Error) {
    #[cfg(feature = "sentry")]
    if let Some(hub) = unit_hub() {
        hub.configure_scope(|scope| describe(&mut Description { scope }));
        capture(&hub, error, Level::Error);
    }
    #[cfg(not(feature = "sentry"))]
    let _ = (describe, error);
}

#[cfg(feature = "sentry")]
fn capture(hub: &Hub, error: &anyhow::Error, level: Level) {
    if error.is::<Panicked>() && integration::reports_panics(hub) {
        return;
    }
    let mut event = sentry::integrations::anyhow::event_from_error(error);
    event.level = level;
    hub.capture_event(event);
}

/// Where the log lines of a run are left: the breadcrumbs of the events it
/// reports.
#[derive(Clone)]
pub(crate) struct Trail {
    #[cfg(feature = "sentry")]
    hub: Option<Arc<Hub>>,
}

impl Trail {
    /// The trail of the unit of work that is running. A line logged from a
    /// task the run spawned is still left on it.
    pub(crate) fn current() -> Self {
        Self {
            #[cfg(feature = "sentry")]
            hub: Hub::with_active(|hub| Some(hub.clone())),
        }
    }

    /// Leaves a line, without its payload: what a flow logs for its admin is
    /// not what it agreed to send elsewhere.
    pub(crate) fn leave(&self, level: &str, message: &str) {
        #[cfg(feature = "sentry")]
        if let Some(hub) = &self.hub {
            hub.add_breadcrumb(|| Breadcrumb {
                category: Some("flow".to_string()),
                level: match level {
                    "warn" => Level::Warning,
                    "error" => Level::Error,
                    _ => Level::Info,
                },
                message: Some(message.to_string()),
                ..Default::default()
            });
        }
        #[cfg(not(feature = "sentry"))]
        let _ = (level, message);
    }
}

/// Gives each request a scope of its own, named after its method and the
/// route it matched, as the route is declared: the path of a request, which
/// can carry an identifier or a secret, is never sent.
#[cfg(feature = "sentry")]
#[derive(Clone)]
pub(crate) struct IsolateRequests<S>(pub(crate) S);

#[cfg(feature = "sentry")]
impl<S, B> tower::Service<axum::http::Request<B>> for IsolateRequests<S>
where
    S: tower::Service<axum::http::Request<B>>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = futures::future::Either<S::Future, SentryFuture<S::Future>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), S::Error>> {
        self.0.poll_ready(cx)
    }

    fn call(&mut self, request: axum::http::Request<B>) -> Self::Future {
        let Some(hub) = unit_hub() else {
            return futures::future::Either::Left(self.0.call(request));
        };
        if let Some(route) = request.extensions().get::<MatchedPath>() {
            let name = format!("{} {}", request.method(), route.as_str());
            hub.configure_scope(|scope| scope.set_transaction(Some(&name)));
        }
        futures::future::Either::Right(self.0.call(request).bind_hub(hub))
    }
}
