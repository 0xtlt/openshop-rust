//! The page `home` of the framework, rendered by the server.

use chrono::{Duration, Utc};
use openshop::builder::{Field, Provider};
use openshop::server::ServerOptions;
use openshop::testing::{TestContext, TestOptions};
use openshop::{AdminPageId, AdminPageMode, Cron, OpenShopConfig, UiSource, json};
use reqwest::{Method, StatusCode};
use uuid::Uuid;

use super::{fetch, form, send};
use crate::common::noop_flow;

const SHOP: &str = "server-home.myshopify.com";

fn crons() -> [Cron; 2] {
    [
        Cron::new("*/5 * * * *", "syncOrders").name("Quick sync"),
        Cron::new("0 * * * *", "syncOrders"),
    ]
}

async fn context(pages: &[(AdminPageId, AdminPageMode)]) -> TestContext {
    let mut builder = OpenShopConfig::builder()
        .flow(noop_flow("syncOrders"))
        .crons(crons())
        .provider(Provider::new("warehouse").field("token", Field::password("Token")));
    for (id, mode) in pages {
        builder = builder.builtin_page(*id, *mode);
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

/// A run of the shop, created `minutes` ago, that took `elapsed_ms` when it has ended.
async fn seed_run(ctx: &TestContext, status: &str, minutes: i64, elapsed_ms: Option<i64>) -> Uuid {
    let created_at = Utc::now() - Duration::minutes(minutes);
    let completed_at = elapsed_ms.map(|elapsed| created_at + Duration::milliseconds(elapsed));
    let (id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO flow_runs (app_handle, shop, flow_name, status, input, available_at, created_at, started_at, completed_at) \
         VALUES ('default', $1, 'syncOrders', $2, '{}'::json, $3, $3, $3, $4) RETURNING id",
    )
    .bind(&ctx.shop)
    .bind(status)
    .bind(created_at)
    .bind(completed_at)
    .fetch_one(&ctx.db)
    .await
    .unwrap();
    id
}

#[tokio::test]
async fn the_home_page_is_a_dashboard_fetched_again_every_ten_seconds() {
    let ctx = context(&[]).await;
    let failed = seed_run(&ctx, "failed", 5, Some(300)).await;
    let completed = seed_run(&ctx, "completed", 90, Some(1_250)).await;
    let (status, _) = crate::common::send(
        ctx.post("/api/crons/toggle")
            .json(&json!({ "key": crons()[1].key(), "enabled": false })),
    )
    .await;
    assert_eq!(status, 200);

    let page = send(fetch(&ctx, Method::GET, "/")).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    let body = &page.body;
    assert!(
        body.contains(r#"<main id="openshop-page"><s-page heading="Dashboard" data-openshop-refresh="10000">"#),
        "{body}"
    );
    // The failed runs, with a link to each.
    assert!(
        body.contains(r#"<s-banner tone="critical" heading="1 failed run">"#),
        "{body}"
    );
    assert!(
        body.contains(&format!(r#"— 5m ago <s-link href="/runs/{failed}">View</s-link>"#)),
        "{body}"
    );
    // The cards: one flow, a cron enabled and one disabled, one provider, the last run.
    assert!(
        body.contains(r#"<s-text color="subdued">Flows</s-text><s-heading>1</s-heading>"#),
        "{body}"
    );
    assert!(
        body.contains(r#"<s-heading>1</s-heading><s-text color="subdued">(1 disabled)</s-text>"#),
        "{body}"
    );
    assert!(
        body.contains(r#"<s-text color="subdued">Providers</s-text><s-heading>1</s-heading>"#),
        "{body}"
    );
    assert!(
        body.contains(r#"<s-text color="subdued">Last activity</s-text><s-heading>5m ago</s-heading>"#),
        "{body}"
    );
    // The latest runs, newest first.
    let newest = body
        .find(&format!(r#"<s-link href="/runs/{failed}" tone="neutral">"#))
        .expect("the failed run");
    let oldest = body
        .find(&format!(r#"<s-link href="/runs/{completed}" tone="neutral">"#))
        .expect("the completed run");
    assert!(newest < oldest, "{body}");
    assert!(
        body.contains(r#"<s-badge tone="success">completed</s-badge>"#),
        "{body}"
    );
    assert!(body.contains(r#"<s-text color="subdued">1h ago</s-text>"#), "{body}");
    assert!(body.contains(r#"<s-text color="subdued">1.3s</s-text>"#), "{body}");
    assert!(body.contains(r#"<s-text color="subdued">300ms</s-text>"#), "{body}");
    assert!(body.contains(r#"href="/flows">View all runs</s-button>"#), "{body}");
    // The providers, each with its status and a link to its form.
    assert!(body.contains(r#"<s-section heading="Providers">"#), "{body}");
    assert!(
        body.contains(r#"<s-badge tone="warning">Not configured</s-badge>"#),
        "{body}"
    );
    assert!(
        body.contains(r#"<s-button variant="secondary" href="/providers?provider=warehouse">Configure</s-button>"#),
        "{body}"
    );

    // The dashboard has no form.
    let posted = send(form(fetch(&ctx, Method::POST, "/"), "_action=refresh")).await;
    assert_eq!(posted.status, StatusCode::NOT_FOUND);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn the_home_page_leaves_out_what_belongs_to_a_disabled_page() {
    let ctx = context(&[
        (AdminPageId::Flows, AdminPageMode::Disabled),
        (AdminPageId::Providers, AdminPageMode::Disabled),
    ])
    .await;
    seed_run(&ctx, "failed", 5, None).await;

    let page = send(fetch(&ctx, Method::GET, "/")).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    let body = &page.body;
    assert!(
        body.contains(r#"<s-text color="subdued">Crons</s-text><s-stack"#),
        "{body}"
    );
    for gone in [
        "Recent runs",
        "failed run",
        "Last activity",
        ">Flows<",
        "Providers",
        "Configure",
    ] {
        assert!(!body.contains(gone), "{gone} in {body}");
    }
    ctx.shutdown().await.unwrap();
}
