//! What the framework adds to the Sentry client of an application: panics
//! reported without waiting, and events sent without the secrets of a URL.

use std::borrow::Cow;
use std::panic::{self, PanicHookInfo};
use std::sync::{LazyLock, Once};

use regex::Regex;
use sentry::integrations::panic::PanicIntegration;
use sentry::protocol::Event;
use sentry::{ClientOptions, Hub, Integration};

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

/// Whether a panic hook reports the panics of what runs on this hub.
pub(super) fn reports_panics(hub: &Hub) -> bool {
    hub.with_integration(|_: &Reports| true) || hub.with_integration(|_: &PanicIntegration| true)
}

/// Reports a panic where it happens, in the scope of the unit of work that
/// was running.
///
/// The hook of the SDK then waits for the event to be sent. The framework
/// catches the panics of flows and handlers and goes on: a thread of the
/// runtime must not wait on Sentry each time one of them panics.
fn report_panic(info: &PanicHookInfo<'_>) {
    Hub::with_active(|hub| {
        // A client the application gave the hook of the SDK reports with that one.
        if !hub.with_integration(|_: &Reports| true) || hub.with_integration(|_: &PanicIntegration| true) {
            return;
        }
        hub.capture_event(PanicIntegration::default().event_from_panic_info(info));
        // The process stops here: nothing else would send the event.
        if cfg!(panic = "abort")
            && let Some(client) = hub.client()
        {
            client.flush(None);
        }
    });
}

/// `scheme://user:password@` at the start of a URL.
static CREDENTIALS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b([a-z][a-z0-9+.-]*://)[^\s/@?#]+@").expect("valid regex"));
/// The query string of a URL, up to what ends the URL in a sentence.
static QUERY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)\b([a-z][a-z0-9+.-]*://[^\s?#'"<>)]*)\?[^\s#'"<>)]*"#).expect("valid regex"));

/// Removes from a text what a URL carries of secrets: its credentials and
/// its query string. The error of an HTTP client or of a database names the
/// URL it failed on, and that is where an API key or a password travels.
fn scrub(text: &mut String) {
    for (pattern, replacement) in [(&*CREDENTIALS, "${1}[redacted]@"), (&*QUERY, "${1}?[redacted]")] {
        if let Cow::Owned(scrubbed) = pattern.replace_all(text, replacement) {
            *text = scrubbed;
        }
    }
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
            "error sending request for url (https://api.example.com/orders?[redacted])"
        );
        assert_eq!(
            scrubbed("cannot reach postgresql://openshop:hunter2@db.internal:5432/openshop?sslmode=require now"),
            "cannot reach postgresql://[redacted]@db.internal:5432/openshop?[redacted] now"
        );
        assert_eq!(
            scrubbed("GET \"https://user@host/a?token=1\" then HTTPS://HOST/b?token=2"),
            "GET \"https://[redacted]@host/a?[redacted]\" then HTTPS://HOST/b?[redacted]"
        );
    }

    #[test]
    fn leaves_the_rest_of_a_message_as_it_is() {
        for text in [
            "the warehouse refused the orders",
            "Flow \"syncOrders\" failed: why? nobody@example.com knows",
            "see https://example.com/docs/errors#timeouts",
            "https://example.com/users/buyer@example.com is gone",
        ] {
            assert_eq!(scrubbed(text), text);
        }
    }
}
