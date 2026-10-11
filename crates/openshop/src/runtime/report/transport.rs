//! Sends envelopes to Sentry with the `reqwest` of the framework.
//!
//! The transports of the SDK come with an HTTP client and a TLS stack of
//! their own. This one sends from a thread of its own, as they do: a report is
//! never awaited by what fails, and what is queued can be sent while the
//! runtime of the application is busy or shutting down.
//!
//! Unlike theirs, the thread waits for its next envelope on its runtime: the
//! connection it keeps open is watched meanwhile, and one that Sentry closed
//! is not the one the next report is written to.

use std::sync::Arc;
use std::sync::mpsc::{SyncSender, sync_channel};
use std::time::{Duration, Instant};

use reqwest::StatusCode;
use reqwest::header::RETRY_AFTER;
use sentry::client_report::Reason;
use sentry::protocol::{EnvelopeFilterCallbacks, EnvelopeItem, ItemContainer};
use sentry::transports::{RateLimiter, RateLimitingCategory};
use sentry::{Envelope, Transport, TransportFactory, TransportOptions};
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::mpsc::{Receiver, Sender, channel};

/// Envelopes waiting to be sent, when the settings do not say. One more is dropped.
const DEFAULT_QUEUE: usize = 30;
/// How long a flush waits before asking a full queue again.
const FULL_QUEUE_RETRY: Duration = Duration::from_millis(10);

pub(super) struct HttpTransportFactory;

impl TransportFactory for HttpTransportFactory {
    fn create_transport_with_options(&self, options: TransportOptions) -> Arc<dyn Transport> {
        Arc::new(HttpTransport::new(options))
    }
}

// Nearly every task is an envelope: boxing them would cost an allocation per report.
#[allow(clippy::large_enum_variant)]
enum Task {
    Send(Envelope),
    Flush(SyncSender<()>),
}

struct HttpTransport {
    queue: Sender<Task>,
    lost: sentry::client_report::Recorder,
}

impl HttpTransport {
    fn new(options: TransportOptions) -> Self {
        let capacity = options
            .transport_channel_capacity
            .map_or(DEFAULT_QUEUE, |capacity| capacity.get());
        let (queue, tasks) = channel(capacity);
        let lost = options.client_report_recorder.clone();
        // Without the thread nothing is sent, and the application runs as it would.
        let _ = std::thread::Builder::new()
            .name("sentry-transport".to_string())
            .spawn(move || {
                if let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() {
                    runtime.block_on(send_all(tasks, options));
                }
            });
        Self { queue, lost }
    }
}

impl Transport for HttpTransport {
    fn send_envelope(&self, envelope: Envelope) {
        // A full queue drops the envelope: reporting never waits.
        let (envelope, reason) = match self.queue.try_send(Task::Send(envelope)) {
            Err(TrySendError::Full(Task::Send(envelope))) => (envelope, Reason::QueueOverflow),
            Err(TrySendError::Closed(Task::Send(envelope))) => (envelope, Reason::InternalError),
            _ => return,
        };
        self.lost.record_lost_data(&envelope, reason);
    }

    fn flush(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let (done, wait) = sync_channel(1);
        // A full queue has no room for the request either: it is asked again
        // until the time is up, so that closing never waits longer than that.
        let mut request = Task::Flush(done);
        loop {
            match self.queue.try_send(request) {
                Ok(()) => break,
                Err(TrySendError::Full(refused)) if Instant::now() < deadline => {
                    request = refused;
                    std::thread::sleep(FULL_QUEUE_RETRY);
                }
                Err(_) => return false,
            }
        }
        wait.recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .is_ok()
    }
}

fn client(options: &TransportOptions) -> reqwest::Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .user_agent(options.user_agent.as_ref())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .danger_accept_invalid_certs(options.accept_invalid_certs);
    if let Some(proxy) = &options.http_proxy {
        builder = builder.proxy(reqwest::Proxy::http(proxy.as_ref())?);
    }
    if let Some(proxy) = &options.https_proxy {
        builder = builder.proxy(reqwest::Proxy::https(proxy.as_ref())?);
    }
    builder.build()
}

/// What Sentry limits an item of an envelope with.
fn category(item: &EnvelopeItem) -> RateLimitingCategory {
    match item {
        EnvelopeItem::Event(_) => RateLimitingCategory::Error,
        EnvelopeItem::SessionUpdate(_) | EnvelopeItem::SessionAggregates(_) => RateLimitingCategory::Session,
        EnvelopeItem::Transaction(_) => RateLimitingCategory::Transaction,
        EnvelopeItem::Attachment(_) => RateLimitingCategory::Attachment,
        EnvelopeItem::ItemContainer(ItemContainer::Logs(_)) => RateLimitingCategory::LogItem,
        EnvelopeItem::ItemContainer(ItemContainer::Metrics(_)) => RateLimitingCategory::TraceMetric,
        _ => RateLimitingCategory::Any,
    }
}

/// Sends the envelopes in the order they were queued, until the client is closed.
async fn send_all(mut tasks: Receiver<Task>, options: TransportOptions) {
    let client = match client(&options) {
        Ok(client) => client,
        Err(error) => {
            tracing::warn!("[openshop] Sentry is off: its HTTP client could not be built: {error}");
            return;
        }
    };
    let url = options.dsn.envelope_api_url().to_string();
    let auth = options.dsn.to_auth(Some(&options.user_agent)).to_string();
    let lost = &options.client_report_recorder;
    let mut limits = RateLimiter::new();

    while let Some(task) = tasks.recv().await {
        let envelope = match task {
            Task::Send(envelope) => envelope,
            Task::Flush(done) => {
                let _ = done.send(());
                continue;
            }
        };
        // Sentry said to wait: for everything, or for what some items are.
        if !limits.is_enabled(RateLimitingCategory::Any) {
            lost.record_lost_data(&envelope, Reason::RatelimitBackoff);
            continue;
        }
        let allowed = envelope.filter(EnvelopeFilterCallbacks::new(
            |item: &EnvelopeItem| limits.is_enabled(category(item)),
            |item| lost.record_lost_data(&item, Reason::RatelimitBackoff),
        ));
        let Some(envelope) = allowed else { continue };
        let mut body = Vec::new();
        if envelope.to_writer(&mut body).is_err() {
            lost.record_lost_data(&envelope, Reason::InternalError);
            continue;
        }

        match client.post(&url).header("X-Sentry-Auth", &auth).body(body).send().await {
            Ok(response) => {
                let header = |name: &str| response.headers().get(name).and_then(|value| value.to_str().ok());
                let status = response.status();
                if let Some(limit) = header("x-sentry-rate-limits") {
                    limits.update_from_sentry_header(limit);
                } else if let Some(delay) = header(RETRY_AFTER.as_str()) {
                    limits.update_from_retry_after(delay);
                } else if status == StatusCode::TOO_MANY_REQUESTS {
                    limits.update_from_429();
                }
                // Sentry counts what it refuses with a `429` itself.
                if (status.is_client_error() || status.is_server_error()) && status != StatusCode::TOO_MANY_REQUESTS {
                    tracing::debug!("[openshop] Sentry answered {status} to a report");
                    lost.record_lost_data(&envelope, Reason::SendError);
                }
            }
            Err(error) => {
                tracing::debug!("[openshop] A report could not be sent to Sentry: {error}");
                lost.record_lost_data(&envelope, Reason::NetworkError);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::sync::Mutex;

    use axum::body::Bytes;
    use axum::http::HeaderMap;
    use axum::routing::post;
    use sentry::ClientOptions;
    use sentry::protocol::{Event, Transaction};

    use super::*;

    #[test]
    fn a_limit_on_a_category_drops_the_items_of_that_category_only() {
        // A Sentry that takes everything, and says to hold transactions for a minute.
        let received = Arc::new(Mutex::new(Vec::<String>::new()));
        let inbox = received.clone();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind(("127.0.0.1", 0)))
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = axum::Router::new().route(
            "/api/1/envelope/",
            post(move |body: Bytes| async move {
                inbox.lock().unwrap().push(String::from_utf8_lossy(&body).into_owned());
                let mut limits = HeaderMap::new();
                limits.insert("x-sentry-rate-limits", "60:transaction:organization".parse().unwrap());
                limits
            }),
        );
        runtime.spawn(async move { axum::serve(listener, server).await });

        let options = ClientOptions::new().dsn(&format!("http://key@127.0.0.1:{port}/1"));
        let transport = HttpTransport::new(TransportOptions::try_from_client_options(&options).unwrap());
        let sent = || {
            assert!(transport.flush(Duration::from_secs(10)));
            received.lock().unwrap().len()
        };

        transport.send_envelope(Event::default().into());
        assert_eq!(sent(), 1);
        transport.send_envelope(Transaction::default().into());
        assert_eq!(sent(), 1);
        transport.send_envelope(Event::default().into());
        assert_eq!(sent(), 2);
        assert!(received.lock().unwrap().iter().all(|body| body.contains(r#""type":"event""#)));
    }

    #[test]
    fn a_flush_gives_up_when_its_time_is_up_even_with_a_full_queue() {
        // A server that accepts connections and never answers.
        let silent = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = silent.local_addr().unwrap().port();
        let options = ClientOptions::new()
            .dsn(&format!("http://key@127.0.0.1:{port}/1"))
            .transport_channel_capacity(1);
        let transport = HttpTransport::new(TransportOptions::try_from_client_options(&options).unwrap());

        // One envelope is being sent, one waits, the others are dropped.
        for _ in 0..4 {
            transport.send_envelope(Event::default().into());
        }
        let started = Instant::now();
        assert!(!transport.flush(Duration::from_millis(200)));
        assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
    }
}
