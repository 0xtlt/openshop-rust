//! What the framework adds to the Sentry client of an application: panics
//! reported without waiting, and events sent without what a URL carries of
//! secrets, wherever the event has one: messages, tags, extra data, contexts,
//! breadcrumbs, and the request.

use std::cell::{Cell, RefCell};
use std::panic::{self, PanicHookInfo};
use std::sync::{Arc, LazyLock, Once};

use regex::{Captures, Regex};
use sentry::integrations::panic::PanicIntegration;
use sentry::protocol::{Context, Event, Map, Request, Value};
use sentry::{ClientOptions, Hub, Integration};
use uuid::Uuid;

/// The integration of the framework, part of every client it starts.
pub(super) struct Reports;

static HOOK: Once = Once::new();

impl Integration for Reports {
    fn name(&self) -> &'static str {
        "openshop"
    }

    fn setup(&self, _options: &mut ClientOptions) {
        HOOK.call_once(|| {
            let next = panic::take_hook();
            panic::set_hook(Box::new(move |info| {
                report_panic(info);
                next(info);
            }));
        });
    }

    fn process_event(&self, mut event: Event<'static>, _options: &ClientOptions) -> Option<Event<'static>> {
        scrub_event(&mut event);
        Some(event)
    }
}

/// Everything of an event that can carry a URL or a credential.
fn scrub_event(event: &mut Event<'static>) {
    let exceptions = event.exception.iter_mut().filter_map(|exception| exception.value.as_mut());
    let breadcrumbs = event.breadcrumbs.iter_mut().filter_map(|breadcrumb| breadcrumb.message.as_mut());
    exceptions.chain(breadcrumbs).chain(event.message.as_mut()).for_each(scrub);
    if let Some(entry) = &mut event.logentry {
        scrub(&mut entry.message);
        entry.params.iter_mut().for_each(scrub_value);
    }
    for exception in &mut event.exception {
        if let Some(mechanism) = &mut exception.mechanism {
            scrub_map(&mut mechanism.data);
        }
    }
    for breadcrumb in &mut event.breadcrumbs {
        scrub_map(&mut breadcrumb.data);
    }
    event.tags.values_mut().for_each(scrub);
    scrub_map(&mut event.extra);
    event.contexts.values_mut().for_each(scrub_context);
    if let Some(user) = &mut event.user {
        scrub_map(&mut user.other);
    }
    if let Some(request) = &mut event.request {
        scrub_request(request);
    }
}

fn scrub_map(map: &mut Map<String, Value>) {
    map.values_mut().for_each(scrub_value);
}

/// Scrubs the strings of a JSON value, at any depth.
fn scrub_value(value: &mut Value) {
    match value {
        Value::String(text) => scrub(text),
        Value::Array(items) => items.iter_mut().for_each(scrub_value),
        Value::Object(map) => map.values_mut().for_each(scrub_value),
        _ => {}
    }
}

fn scrub_context(context: &mut Context) {
    match context {
        Context::Other(map) => scrub_map(map),
        Context::Response(response) => {
            response.cookies = response.cookies.take().map(|_| REDACTED.to_string());
            scrub_headers(&mut response.headers);
            if let Some(data) = &mut response.data {
                scrub_value(data);
            }
        }
        Context::Trace(trace) => {
            trace.description.iter_mut().for_each(scrub);
            scrub_map(&mut trace.data);
        }
        // Every other kind of context, those of the SDK that are yet to come
        // included, through its JSON form: all its strings, at any depth. One
        // that does not come back as a context is not sent.
        other => {
            let scrubbed = serde_json::to_value(&*other).ok().and_then(|mut json| {
                scrub_value(&mut json);
                serde_json::from_value::<Context>(json).ok()
            });
            *other = scrubbed.unwrap_or_else(|| Context::Other(Map::new()));
        }
    }
}

const REDACTED: &str = "[redacted]";

/// Whether the name of a header or of a variable says that its value is a secret.
fn is_secret_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    [
        "authorization",
        "cookie",
        "token",
        "secret",
        "password",
        "api-key",
        "api_key",
        "apikey",
        "hmac",
        "signature",
    ]
    .iter()
    .any(|word| name.contains(word))
}

/// The value of a header named like a secret is removed, the others are scrubbed.
fn scrub_headers(headers: &mut Map<String, String>) {
    for (name, value) in headers.iter_mut() {
        if is_secret_name(name) {
            *value = REDACTED.to_string();
        } else {
            scrub(value);
        }
    }
}

/// The request keeps its method and its address, without credentials, query
/// string or token; its cookies, body and secret headers are removed.
fn scrub_request(request: &mut Request) {
    request.url = request
        .url
        .take()
        .and_then(|url| sentry::protocol::Url::parse(&scrub_url(url.as_str())).ok());
    if request.query_string.as_ref().is_some_and(|query| !query.is_empty()) {
        request.query_string = Some(REDACTED.to_string());
    }
    request.cookies = request.cookies.take().map(|_| REDACTED.to_string());
    request.data = request.data.take().map(|_| REDACTED.to_string());
    scrub_headers(&mut request.headers);
    for (name, value) in request.env.iter_mut() {
        if is_secret_name(name) {
            *value = REDACTED.to_string();
        } else {
            scrub(value);
        }
    }
}

/// Whether a client is bound to the hub and still sends.
fn is_reporting(hub: &Hub) -> bool {
    hub.client().is_some_and(|client| client.is_enabled())
}

/// The hub to report on: the one of this thread, or the one of the process.
///
/// A thread keeps the client it first saw. One that started before Sentry
/// did has none, and one that outlived a client has a closed one: both
/// report with the client the process has now.
pub(super) fn reporting_hub() -> Option<Arc<Hub>> {
    Some(Hub::current())
        .filter(|hub| is_reporting(hub))
        .or_else(|| Some(Hub::main()).filter(|hub| is_reporting(hub)))
}

/// Whether a panic hook reports the panics of what runs on this hub.
pub(super) fn reports_panics(hub: &Hub) -> bool {
    hub.with_integration(|_: &Reports| true) || hub.with_integration(|_: &PanicIntegration| true)
}

thread_local! {
    /// Whether what this thread is running holds its panics back.
    static HOLDING: Cell<bool> = const { Cell::new(false) };
    /// The panic that was held back, until the failure it becomes takes it.
    static HELD: RefCell<Option<Event<'static>>> = const { RefCell::new(None) };
}

/// Holds back the panics of what a thread runs for as long as it is kept:
/// the attempt of a run, which knows only once it has failed whether it will
/// be made again, and so how much its panic matters.
pub(super) struct Holding {
    outer: bool,
}

impl Holding {
    pub(super) fn enter() -> Self {
        Self {
            outer: HOLDING.replace(true),
        }
    }
}

impl Drop for Holding {
    fn drop(&mut self) {
        HOLDING.set(self.outer);
        // Nothing took the panic: the code it happened in caught it itself, or
        // it is still unwinding. It is reported as it would have been.
        if !self.outer
            && let Some(event) = take_held()
            && let Some(hub) = reporting_hub()
        {
            hub.capture_event(event);
        }
    }
}

/// The panic this thread held back, for the failure it became.
pub(super) fn take_held() -> Option<Event<'static>> {
    HELD.try_with(RefCell::take).ok().flatten()
}

/// Reports a panic where it happens, in the scope of the unit of work that
/// was running.
///
/// The hook of the SDK then waits for the event to be sent. The framework
/// catches the panics of flows and handlers and goes on: a thread of the
/// runtime must not wait on Sentry each time one of them panics.
fn report_panic(info: &PanicHookInfo<'_>) {
    let Some(hub) = reporting_hub() else { return };
    // A client the application gave the hook of the SDK reports with that one.
    if !hub.with_integration(|_: &Reports| true) || hub.with_integration(|_: &PanicIntegration| true) {
        return;
    }
    let event = PanicIntegration::default().event_from_panic_info(info);
    if cfg!(panic = "abort") {
        // The process stops here: nothing else would send the event.
        hub.capture_event(event);
        if let Some(client) = hub.client() {
            client.flush(None);
        }
    } else if HOLDING.try_with(Cell::get).unwrap_or(false) {
        // One panic is held at a time: an earlier one nobody took is reported.
        if let Ok(Some(earlier)) = HELD.try_with(|held| held.replace(Some(event))) {
            hub.capture_event(earlier);
        }
    } else {
        hub.capture_event(event);
    }
}

/// A URL in a sentence: a scheme, `://`, and what follows up to a space.
static URL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\b[a-z][a-z0-9+.-]*://\S+").expect("valid regex"));

/// The shortest part of a path that is taken for a token.
const TOKEN_LENGTH: usize = 20;

/// Removes from a text what its URLs carry of secrets. The error of an HTTP
/// client or of a database names the URL it failed on, and nobody chose to
/// write that message.
fn scrub(text: &mut String) {
    if !text.contains("://") {
        return;
    }
    let scrubbed = URL.replace_all(text, |url: &Captures<'_>| scrub_url(&url[0]));
    if *scrubbed != **text {
        *text = scrubbed.into_owned();
    }
}

/// A URL without its credentials, its query string, a fragment that holds
/// values, and the parts of its path that look like a token.
///
/// When in doubt, more is removed: the credentials end at the last `@`, as a
/// password can hold one, and the query runs to the end of the word, as it
/// can hold any character that ends a URL in a sentence.
fn scrub_url(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let end = rest.find(['?', '#']).unwrap_or(rest.len());
    let (location, tail) = rest.split_at(end);
    let mut scrubbed = format!("{scheme}://");

    let location = match location.rfind('@') {
        Some(at) => {
            scrubbed.push_str("[redacted]@");
            &location[at + 1..]
        }
        None => location,
    };
    let mut parts = location.split('/');
    scrubbed.push_str(parts.next().unwrap_or_default());
    for part in parts {
        scrubbed.push('/');
        scrubbed.push_str(if is_token(part) { "[redacted]" } else { part });
    }

    match tail.as_bytes().first() {
        Some(b'?') => scrubbed.push_str("?[redacted]"),
        Some(b'#') if tail.contains('=') => scrubbed.push_str("#[redacted]"),
        _ => scrubbed.push_str(tail),
    }
    scrubbed
}

/// Whether a part of a path looks like a secret: long, made of letters and
/// digits, and not an identifier written as a UUID. The token of a webhook
/// URL is one.
fn is_token(part: &str) -> bool {
    part.len() >= TOKEN_LENGTH
        && part.bytes().any(|byte| byte.is_ascii_alphabetic())
        && part.bytes().any(|byte| byte.is_ascii_digit())
        && !(part.len() == 36 && Uuid::try_parse(part).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scrubbed(text: &str) -> String {
        let mut text = text.to_string();
        scrub(&mut text);
        text
    }

    #[test]
    fn removes_the_credentials_and_the_query_of_a_url() {
        assert_eq!(
            scrubbed("error sending request for url (https://api.example.com/orders?api_key=SECRET&page=2)"),
            "error sending request for url (https://api.example.com/orders?[redacted]"
        );
        assert_eq!(
            scrubbed("cannot reach postgresql://openshop:hunter2@db.internal:5432/openshop?sslmode=require now"),
            "cannot reach postgresql://[redacted]@db.internal:5432/openshop?[redacted] now"
        );
        assert_eq!(
            scrubbed("GET \"https://user@host/a?token=1\" then HTTPS://HOST/b?token=2"),
            "GET \"https://[redacted]@host/a?[redacted] then HTTPS://HOST/b?[redacted]"
        );
    }

    #[test]
    fn removes_more_rather_than_less() {
        // A query holds any character, the ones that end a URL in a sentence too.
        assert_eq!(
            scrubbed("GET https://api.example.com/search?filter=(tag:a)&api_key=SECRET failed"),
            "GET https://api.example.com/search?[redacted] failed"
        );
        assert_eq!(
            scrubbed("{\"url\":\"https://api.example.com/a?key=SECRET\",\"token\":\"OTHER\"}"),
            "{\"url\":\"https://api.example.com/a?[redacted]"
        );
        // A password holds an `@`, or a `/` that was not encoded.
        assert_eq!(scrubbed("postgres://user:p@ss@db/app"), "postgres://[redacted]@db/app");
        assert_eq!(scrubbed("postgres://user:pa/ss@db/app"), "postgres://[redacted]@db/app");
        // So an address in a path goes with what precedes it.
        assert_eq!(
            scrubbed("https://example.com/users/buyer@example.com is gone"),
            "https://[redacted]@example.com is gone"
        );
        // A fragment that holds values is one the server was not meant to see.
        assert_eq!(
            scrubbed("back to https://app.example.com/callback#access_token=SECRET&state=1"),
            "back to https://app.example.com/callback#[redacted]"
        );
    }

    #[test]
    fn removes_what_looks_like_a_token_in_a_path() {
        assert_eq!(
            scrubbed("POST https://hooks.slack.com/services/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 failed"),
            "POST https://hooks.slack.com/services/T024BE7LD/B08LX2K4P/[redacted] failed"
        );
        assert_eq!(
            scrubbed("https://api.telegram.org/bot123456789:AAEhBOweik6ad9r_QXMENQjcrGbqCr4K-9w/sendMessage"),
            "https://api.telegram.org/[redacted]/sendMessage"
        );
        // An identifier is not a secret: a UUID, a number, a short name.
        for url in [
            "https://api.example.com/runs/3f2a9c1b-7d8e-4f60-a1b2-c3d4e5f60718/logs",
            "https://shop.myshopify.com/admin/api/2026-01/orders/5123456789012.json",
            "https://api.example.com/customers/cus_NffrFeUfNV2Hib",
        ] {
            assert_eq!(scrubbed(url), url);
        }
    }

    #[test]
    fn leaves_the_rest_of_a_message_as_it_is() {
        for text in [
            "the warehouse refused the orders",
            "Flow \"syncOrders\" failed: why? nobody@example.com knows",
            "see https://example.com/docs/errors#timeouts",
            "calling https://api.example.com/v2/orders (attempt 2)",
            "std::io://not a url",
        ] {
            assert_eq!(scrubbed(text), text);
        }
    }
}
