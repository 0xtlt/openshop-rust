//! Starting Sentry for a process, and what is then sent over HTTP.
//!
//! `SentryConfig::init` binds its client for the whole process: this file has
//! one test, and is a process of its own.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::routing::post;
use openshop::SentryConfig;
use openshop::sentry::{self, Level};

/// What a Sentry server was sent.
#[derive(Debug, Clone)]
struct Received {
    path: String,
    auth: String,
    body: String,
}

type Inbox = Arc<Mutex<Vec<Received>>>;

/// Keeps a request, and tells the sender to wait after the second one.
async fn receive(State(inbox): State<Inbox>, uri: Uri, headers: HeaderMap, body: Bytes) -> (StatusCode, HeaderMap) {
    let mut inbox = inbox.lock().unwrap();
    inbox.push(Received {
        path: uri.path().to_string(),
        auth: headers["x-sentry-auth"].to_str().unwrap().to_string(),
        body: String::from_utf8_lossy(&body).into_owned(),
    });
    let mut answer = HeaderMap::new();
    if inbox.len() < 2 {
        return (StatusCode::OK, answer);
    }
    answer.insert("retry-after", "60".parse().unwrap());
    (StatusCode::TOO_MANY_REQUESTS, answer)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_started_client_sends_events_and_waits_when_told_to() {
    let inbox = Inbox::default();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = Router::new()
        .route("/api/42/envelope/", post(receive))
        .with_state(inbox.clone());
    tokio::spawn(async move { axum::serve(listener, server).await });

    let config = SentryConfig::new(format!("http://public-key@127.0.0.1:{port}/42")).release("1.2.3");
    let guard = config.init(false);
    let sent = || {
        let client = sentry::Hub::main().client().expect("a client bound to the process");
        assert!(
            client.flush(Some(Duration::from_secs(10))),
            "the queue was not sent in time"
        );
        inbox.lock().unwrap().clone()
    };

    sentry::capture_message("the first", Level::Error);
    let received = sent();
    assert_eq!(received.len(), 1, "{received:#?}");
    assert_eq!(received[0].path, "/api/42/envelope/");
    assert!(
        received[0].auth.contains("sentry_key=public-key"),
        "{}",
        received[0].auth
    );
    let body = &received[0].body;
    assert!(body.contains(r#""type":"event""#), "{body}");
    assert!(body.contains(r#""message":"the first""#), "{body}");
    assert!(body.contains(r#""release":"1.2.3""#), "{body}");
    // Named after the process, which is not a production one here.
    assert!(body.contains(r#""environment":"development""#), "{body}");

    // A thread started later reports too: here its panic, through the hook the
    // client installed. Sentry answers this event with `429`.
    let panicked = std::thread::spawn(|| panic!("a thread gave up")).join();
    assert!(panicked.is_err());
    let received = sent();
    assert_eq!(received.len(), 2, "{received:#?}");
    assert!(received[1].body.contains("a thread gave up"), "{}", received[1].body);

    // Told to wait a minute, the client drops what it would have sent.
    sentry::capture_message("the third", Level::Error);
    assert_eq!(sent().len(), 2);

    // Once the guard is dropped, nothing is reported.
    drop(guard);
    sentry::capture_message("the fourth", Level::Error);
    assert_eq!(inbox.lock().unwrap().len(), 2);
}
