//! The page `providers` of the framework, rendered by the server.

use openshop::builder::{Field, Provider};
use openshop::server::ServerOptions;
use openshop::testing::{TestContext, TestOptions};
use openshop::{OpenShopConfig, PublicError, UiSource, Value, json, validators};
use reqwest::{Method, StatusCode};

use super::{Reply, fetch, form, send, urlencoding};
use crate::common::noop_flow;

const SHOP: &str = "server-providers.myshopify.com";

fn warehouse() -> Provider {
    Provider::new("warehouse")
        .field("endpoint", Field::text("Endpoint").validate(validators::url()))
        .field("apiKey", Field::password("API key"))
        .field("sandbox", Field::checkbox("Sandbox").optional())
        .checker(|config| async move {
            Ok(
                config.get("endpoint").and_then(Value::as_str) == Some("https://warehouse.test")
                    && config.get("apiKey").and_then(Value::as_str) == Some("secret-1"),
            )
        })
}

async fn context(providers: Vec<Provider>) -> TestContext {
    let mut builder = OpenShopConfig::builder().flow(noop_flow("syncOrders"));
    for provider in providers {
        builder = builder.provider(provider);
    }
    TestContext::with_options(
        builder.build().expect("valid config"),
        TestOptions {
            shop: SHOP.to_string(),
            access_token: Some("offline-token".to_string()),
            server: ServerOptions {
                ui: UiSource::Server,
                dev: false,
            },
            ..Default::default()
        },
    )
    .await
    .expect("test context")
}

/// Sends a form of the page, its fields encoded.
async fn post(ctx: &TestContext, path: &str, fields: &[(&str, &str)]) -> Reply {
    let body = fields
        .iter()
        .map(|(name, value)| format!("{}={}", urlencoding(name), urlencoding(value)))
        .collect::<Vec<_>>()
        .join("&");
    send(form(fetch(ctx, Method::POST, path), body)).await
}

async fn stored_config(ctx: &TestContext) -> Value {
    let (config,): (Value,) = sqlx::query_as("SELECT config FROM provider_configs WHERE provider_name = 'warehouse'")
        .fetch_one(&ctx.db)
        .await
        .unwrap();
    config
}

#[tokio::test]
async fn a_provider_is_configured_through_its_form_and_its_connection_tested() {
    let ctx = context(vec![warehouse()]).await;

    let page = send(fetch(&ctx, Method::GET, "/providers")).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    let body = &page.body;
    assert!(body.contains(r#"<s-page heading="Providers">"#), "{body}");
    assert!(body.contains(r#"<s-section heading="warehouse">"#), "{body}");
    assert!(
        body.contains(r#"<s-badge tone="warning">Not configured</s-badge>"#),
        "{body}"
    );
    assert!(
        body.contains(r#"<s-text-field name="config.endpoint" label="Endpoint" value=""></s-text-field>"#),
        "{body}"
    );
    assert!(
        body.contains(r#"<s-password-field name="config.apiKey" label="API key"></s-password-field>"#),
        "{body}"
    );
    // One provider: nothing to choose, and nothing checked yet.
    assert!(!body.contains(r#"<form method="get">"#), "{body}");
    assert!(!body.contains("Last checked"), "{body}");

    // Refused, field by field: the page again, with what was sent and the errors.
    let refused = post(
        &ctx,
        "/providers",
        &[
            ("_action", "save"),
            ("provider", "warehouse"),
            ("config.endpoint", "nope"),
            ("config.apiKey", ""),
        ],
    )
    .await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", refused.body);
    assert_eq!(refused.header("x-openshop-toast-error"), None);
    assert!(
        refused
            .body
            .contains(r#"<s-text-field name="config.endpoint" label="Endpoint" value="nope"></s-text-field>"#),
        "{}",
        refused.body
    );
    assert!(
        refused
            .body
            .contains(r#"name="config.apiKey" label="API key" error="Required">"#),
        "{}",
        refused.body
    );
    // The values are checked once every required field has one; a password sent is never shown again.
    let invalid = post(
        &ctx,
        "/providers",
        &[
            ("_action", "save"),
            ("provider", "warehouse"),
            ("config.endpoint", "nope"),
            ("config.apiKey", "secret-0"),
        ],
    )
    .await;
    assert_eq!(invalid.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", invalid.body);
    assert!(
        invalid.body.contains(r#"name="config.endpoint" label="Endpoint" value="nope" error="Must be a URL string (was &quot;nope&quot;)">"#),
        "{}",
        invalid.body
    );
    assert!(!invalid.body.contains("secret-0"), "{}", invalid.body);
    let (rows,): (i64,) = sqlx::query_as("SELECT count(*) FROM provider_configs")
        .fetch_one(&ctx.db)
        .await
        .unwrap();
    assert_eq!(rows, 0);

    // Saved: a toast, and the page again with what is saved; never the password.
    let saved = post(
        &ctx,
        "/providers",
        &[
            ("_action", "save"),
            ("provider", "warehouse"),
            ("config.endpoint", "https://warehouse.test"),
            ("config.apiKey", "secret-1"),
            ("config.sandbox", "true"),
        ],
    )
    .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.body);
    assert_eq!(saved.header("x-openshop-toast"), Some("Saved."));
    assert!(
        saved.body.contains(r#"<s-badge tone="info">Configured</s-badge>"#),
        "{}",
        saved.body
    );
    assert!(
        saved.body.contains(r#"value="https://warehouse.test"></s-text-field>"#),
        "{}",
        saved.body
    );
    assert!(
        saved.body.contains(r#"placeholder="Saved; leave blank to keep">"#),
        "{}",
        saved.body
    );
    assert!(saved.body.contains(r#"label="Sandbox" checked>"#), "{}", saved.body);
    assert!(!saved.body.contains("secret-1"), "{}", saved.body);
    assert_eq!(
        stored_config(&ctx).await,
        json!({ "endpoint": "https://warehouse.test", "apiKey": "secret-1", "sandbox": true })
    );

    // A blank password keeps the saved one; a checkbox left unchecked is false.
    let again = post(
        &ctx,
        "/providers",
        &[
            ("_action", "save"),
            ("provider", "warehouse"),
            ("config.endpoint", "https://warehouse.test"),
            ("config.apiKey", ""),
        ],
    )
    .await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.body);
    assert_eq!(
        stored_config(&ctx).await,
        json!({ "endpoint": "https://warehouse.test", "apiKey": "secret-1", "sandbox": false })
    );

    // The check: a toast, then the status and the time of the check on the page.
    let checked = post(&ctx, "/providers", &[("_action", "check"), ("provider", "warehouse")]).await;
    assert_eq!(checked.status, StatusCode::OK, "{}", checked.body);
    assert_eq!(checked.header("x-openshop-toast"), Some("Connection%20OK!"));
    assert!(
        checked.body.contains(r#"<s-badge tone="success">Connected</s-badge>"#),
        "{}",
        checked.body
    );
    assert!(
        checked.body.contains("Last checked: <time datetime=\""),
        "{}",
        checked.body
    );

    // A check that fails: an error toast, and the status says so.
    post(
        &ctx,
        "/providers",
        &[
            ("_action", "save"),
            ("provider", "warehouse"),
            ("config.endpoint", "https://warehouse.test"),
            ("config.apiKey", "wrong"),
        ],
    )
    .await;
    let failed = post(&ctx, "/providers", &[("_action", "check"), ("provider", "warehouse")]).await;
    assert_eq!(failed.status, StatusCode::OK, "{}", failed.body);
    assert_eq!(failed.header("x-openshop-toast-error"), Some("Failed"));
    assert!(
        failed.body.contains(r#"<s-badge tone="critical">Error</s-badge>"#),
        "{}",
        failed.body
    );

    // The JSON API reads what the page saved.
    let (_, list) = crate::common::send(ctx.get("/api/providers")).await;
    assert_eq!(
        list[0]["config"],
        json!({ "endpoint": "https://warehouse.test", "sandbox": false })
    );
    assert_eq!(list[0]["lastCheckOk"], false);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn the_provider_shown_is_the_one_of_the_url() {
    let plain = Provider::new("plain").field("token", Field::password("Token"));
    let flaky = Provider::new("flaky & co")
        .field("token", Field::password("Token"))
        .checker(|_config| async move { Err(PublicError::new("connection refused").into()) });
    let ctx = context(vec![warehouse(), plain, flaky]).await;

    // The first one, with a form that chooses another.
    let first = send(fetch(&ctx, Method::GET, "/providers")).await;
    assert!(
        first.body.contains(r#"<form method="get" data-openshop-autosubmit>"#),
        "{}",
        first.body
    );
    assert!(
        first
            .body
            .contains(r#"<s-select name="provider" label="Provider" value="warehouse">"#),
        "{}",
        first.body
    );
    assert!(
        first.body.contains(r#"<s-option value="plain">plain</s-option>"#),
        "{}",
        first.body
    );
    assert!(
        first.body.contains(r#"<s-section heading="warehouse">"#),
        "{}",
        first.body
    );

    let chosen = send(fetch(&ctx, Method::GET, "/providers?provider=flaky%20%26%20co")).await;
    assert_eq!(chosen.status, StatusCode::OK);
    assert!(
        chosen
            .body
            .contains(r#"<s-option value="flaky &amp; co" selected>flaky &amp; co</s-option>"#),
        "{}",
        chosen.body
    );
    assert!(
        chosen.body.contains(r#"<s-section heading="flaky &amp; co">"#),
        "{}",
        chosen.body
    );
    assert!(
        chosen
            .body
            .contains(r#"<input type="hidden" name="provider" value="flaky &amp; co">"#),
        "{}",
        chosen.body
    );

    // A provider that does not exist.
    let missing = send(fetch(&ctx, Method::GET, "/providers?provider=nope")).await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    assert!(
        missing
            .body
            .contains(r#"<s-banner tone="critical" heading="Provider not found">"#),
        "{}",
        missing.body
    );

    // A check that cannot run, and one that fails: the message of the API in an error toast.
    let none = post(
        &ctx,
        "/providers?provider=plain",
        &[("_action", "check"), ("provider", "plain")],
    )
    .await;
    assert_eq!(none.status, StatusCode::BAD_REQUEST);
    assert_eq!(none.header("x-openshop-toast-error"), Some("No%20checker%20defined"));
    let saved = post(
        &ctx,
        "/providers?provider=flaky%20%26%20co",
        &[("_action", "save"), ("provider", "flaky & co"), ("config.token", "t")],
    )
    .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.body);
    assert!(
        saved.body.contains(r#"<s-section heading="flaky &amp; co">"#),
        "{}",
        saved.body
    );
    let refused = post(
        &ctx,
        "/providers?provider=flaky%20%26%20co",
        &[("_action", "check"), ("provider", "flaky & co")],
    )
    .await;
    assert_eq!(refused.status, StatusCode::OK);
    assert_eq!(
        refused.header("x-openshop-toast-error"),
        Some("Error%3A%20connection%20refused")
    );
    // The stored result is untouched by a check that gave no answer.
    assert!(
        refused.body.contains(r#"<s-badge tone="info">Configured</s-badge>"#),
        "{}",
        refused.body
    );

    // A provider or an action the page does not have.
    for (fields, status) in [
        (vec![("_action", "save"), ("provider", "nope")], StatusCode::NOT_FOUND),
        (vec![("_action", "check"), ("provider", "nope")], StatusCode::NOT_FOUND),
        (vec![("_action", "check")], StatusCode::NOT_FOUND),
        (vec![("_action", "drop"), ("provider", "plain")], StatusCode::NOT_FOUND),
    ] {
        let reply = post(&ctx, "/providers", &fields).await;
        assert_eq!(reply.status, status, "{fields:?}");
    }
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn the_page_says_when_there_is_no_provider() {
    let ctx = context(Vec::new()).await;
    let page = send(fetch(&ctx, Method::GET, "/providers")).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("No providers registered."), "{}", page.body);
    ctx.shutdown().await.unwrap();
}
