//! A command of an application that gives a DSN: started under its name, and
//! reporting what stops it.
//!
//! The command binds its client for the whole process and reads the
//! environment: this file has one test, and is a process of its own.

use openshop::sentry::test::TestTransport;
use openshop::{OpenShopConfig, SentryConfig};

/// Sets the environment of the command, before anything else runs.
#[allow(unsafe_code)]
fn environment(variables: &[(&str, Option<&str>)]) {
    for (name, value) in variables {
        // SAFETY: the only test of this process, before it starts a thread.
        unsafe {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }
}

#[test]
fn a_command_reports_under_its_name_what_stops_it() {
    environment(&[
        // Nothing listens there: `start` stops when it connects.
        ("DATABASE_URL", Some("postgresql://openshop@127.0.0.1:1/openshop")),
        ("PGPOOL_CONNECTION_TIMEOUT_MS", Some("300")),
        ("OPENSHOP_ENV", None),
        ("NODE_ENV", None),
    ]);
    let transport = TestTransport::new();
    let kept = transport.clone();
    let config = OpenShopConfig::builder()
        .sentry(
            SentryConfig::new("https://public@sentry.invalid/42".to_string())
                .release("1.2.3")
                .configure(|options| options.transport(kept)),
        )
        .build()
        .unwrap();

    let stopped = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(openshop::cli::run_with_args(config, &["start".to_string()]))
        .unwrap_err();

    let events = transport.fetch_and_clear_events();
    assert_eq!(events.len(), 1, "{events:#?}");
    let event = &events[0];
    let reported = event.exception.last().and_then(|exception| exception.value.clone());
    assert_eq!(reported, Some(stopped.to_string()));
    assert_eq!(event.tags.get("process").map(String::as_str), Some("start"));
    assert_eq!(event.environment.as_deref(), Some("development"));
    assert_eq!(event.release.as_deref(), Some("1.2.3"));
    // The integration of the framework is part of the client the command starts.
    let sdk = event.sdk.as_ref().unwrap();
    assert!(sdk.integrations.iter().any(|name| name == "openshop"), "{sdk:?}");
}
