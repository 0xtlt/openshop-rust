//! What the framework adds to the Sentry client of an application: panics
//! reported without waiting, and events sent without what a URL carries of
//! secrets.

use std::cell::{Cell, RefCell};
use std::panic::{self, PanicHookInfo};
use std::sync::{Arc, LazyLock, Once};

use regex::{Captures, Regex};
use sentry::integrations::panic::PanicIntegration;
use sentry::protocol::Event;
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
        let exceptions = event.exception.iter_mut().filter_map(|exception| exception.value.as_mut());
        let breadcrumbs = event.breadcrumbs.iter_mut().filter_map(|breadcrumb| breadcrumb.message.as_mut());
        exceptions.chain(breadcrumbs).chain(event.message.as_mut()).for_each(scrub);
        if let Some(entry) = &mut event.logentry {
            scrub(&mut entry.message);
        }
        Some(event)
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
