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
    sentry::protocol::{Breadcrumb, Event},
    sentry::{Hub, Level, SentryFuture, SentryFutureExt, User},
    std::sync::{Arc, Mutex},
    std::task::{Context, Poll},
};

/// A panic the framework caught and turned into an error. The panic hook
/// reports it where it happens, with its stack: it is not reported again as
/// the failure it became.
pub(crate) struct Panicked {
    message: String,
    /// The panic itself, when it was held back for the failure to report.
    #[cfg(feature = "sentry")]
    held: Mutex<Option<Box<Event<'static>>>>,
}

impl Panicked {
    /// The error a caught panic becomes: `handler panicked`.
    pub(crate) fn error(what: impl Into<String>) -> anyhow::Error {
        Self {
            message: what.into(),
            #[cfg(feature = "sentry")]
            held: Mutex::default(),
        }
        .into()
    }

    /// The error the panic of an attempt becomes, right where it is caught:
    /// it takes the panic [`holding_panics`] held back on this thread.
    pub(crate) fn of_attempt(what: impl Into<String>) -> anyhow::Error {
        Self {
            message: what.into(),
            #[cfg(feature = "sentry")]
            held: Mutex::new(integration::take_held().map(Box::new)),
        }
        .into()
    }

    #[cfg(feature = "sentry")]
    fn take_held(&self) -> Option<Box<Event<'static>>> {
        self.held.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take()
    }
}

impl fmt::Debug for Panicked {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("Panicked").field(&self.message).finish()
    }
}

impl fmt::Display for Panicked {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for Panicked {}

/// The failure never reached a report, for example when the run was taken
/// over meanwhile: the panic is reported on its own.
#[cfg(feature = "sentry")]
impl Drop for Panicked {
    fn drop(&mut self) {
        if let Some(event) = self.take_held()
            && let Some(hub) = integration::reporting_hub()
        {
            hub.capture_event(*event);
        }
    }
}

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
    integration::reporting_hub().map(|hub| Arc::new(Hub::new_from_top(hub)))
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

/// Runs the attempt of a run with its panics held back. A panic the runner
/// catches is then reported as the failure of the attempt, once it is known
/// whether the attempt will be made again: a warning if so, like any other
/// failure of an attempt that is not the last.
pub(crate) async fn holding_panics<F: Future>(future: F) -> F::Output {
    #[cfg(feature = "sentry")]
    {
        let mut future = std::pin::pin!(future);
        std::future::poll_fn(|cx| {
            let _holding = integration::Holding::enter();
            future.as_mut().poll(cx)
        })
        .await
    }
    #[cfg(not(feature = "sentry"))]
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
    if let Some(hub) = integration::reporting_hub() {
        capture(&hub, error, Level::Error);
    }
    #[cfg(not(feature = "sentry"))]
    let _ = error;
}

/// Reports a failure that is not final: the attempt of a run that will be
/// made again.
pub(crate) fn warning(error: &anyhow::Error) {
    #[cfg(feature = "sentry")]
    if let Some(hub) = integration::reporting_hub() {
        capture(&hub, error, Level::Warning);
    }
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
    if let Some(panicked) = error.downcast_ref::<Panicked>() {
        // The panic of an attempt, held back until now: it is the event, with
        // its stack. It stays fatal unless the attempt will be made again.
        if let Some(mut event) = panicked.take_held() {
            if level == Level::Warning {
                event.level = level;
            }
            hub.capture_event(*event);
            return;
        }
        if integration::reports_panics(hub) {
            return;
        }
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

#[cfg(all(test, feature = "sentry"))]
mod tests {
    use std::sync::mpsc::{channel, sync_channel};

    use sentry::test::TestTransport;

    use super::*;

    fn messages(transport: &TestTransport) -> Vec<String> {
        let events = transport.fetch_and_clear_events();
        let values = events.iter().flat_map(|event| event.exception.iter());
        values.filter_map(|exception| exception.value.clone()).collect()
    }

    #[test]
    fn a_thread_reports_with_the_client_the_process_has_now() {
        let started = |transport: &Arc<TestTransport>| {
            let kept = transport.clone();
            SentryConfig::new("https://public@sentry.invalid/1".to_string())
                .configure(move |options| options.transport(kept))
                .init(false)
        };
        // A thread that outlives the clients, as the threads of a runtime do.
        let (asked, asks) = channel::<(&'static str, std::sync::mpsc::SyncSender<()>)>();
        std::thread::spawn(move || {
            for (message, done) in asks {
                detached(
                    |unit| {
                        unit.unit("flow", "syncOrders");
                    },
                    &anyhow::anyhow!(message),
                );
                let _ = done.send(());
            }
        });
        let report = |message| {
            let (done, wait) = sync_channel(1);
            asked.send((message, done)).unwrap();
            wait.recv().unwrap();
        };

        let (first, second) = (TestTransport::new(), TestTransport::new());
        let guard = started(&first);
        report("with the first client");
        drop(guard);
        report("with no client");
        let guard = started(&second);
        report("with the second client");
        drop(guard);

        // The thread kept the first client, closed since: it reports with the
        // one the process has. Other tests of this process may report too.
        let (first, second) = (messages(&first), messages(&second));
        assert!(first.iter().any(|message| message == "with the first client"), "{first:?}");
        assert!(second.iter().any(|message| message == "with the second client"), "{second:?}");
        let all: Vec<_> = first.iter().chain(&second).collect();
        assert!(!all.iter().any(|message| *message == "with no client"), "{all:?}");
        assert!(!first.iter().any(|message| message == "with the second client"), "{first:?}");
    }
}
