use std::str::FromStr;

use crate::common::noop_flow;
use openshop::testing::{TestContext, TestDb};
use openshop::{OpenShopConfig, json};
use sqlx::postgres::PgConnectOptions;
use sqlx::{Connection, PgConnection};

const OTHER_SHOP: &str = "other-shop.myshopify.com";

async fn schema_exists(schema: &str) -> bool {
    let url = openshop::testing::test_database_url().unwrap();
    let mut connection = PgConnection::connect_with(&PgConnectOptions::from_str(&url).unwrap())
        .await
        .unwrap();
    let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_namespace WHERE nspname = $1)")
        .bind(schema)
        .fetch_one(&mut connection)
        .await
        .unwrap();
    connection.close().await.unwrap();
    exists
}

#[tokio::test]
async fn cleanup_drops_the_schema() {
    let db = TestDb::create().await.unwrap();
    let schema = db.schema.clone();
    assert!(schema_exists(&schema).await);

    db.cleanup().await.unwrap();
    assert!(!schema_exists(&schema).await);
}

#[tokio::test]
async fn a_database_that_was_not_cleaned_up_is_dropped_with_its_value() {
    let db = TestDb::create().await.unwrap();
    let schema = db.schema.clone();

    // What happens when an assertion fails before `cleanup`.
    drop(db);
    assert!(!schema_exists(&schema).await);
}

#[tokio::test]
async fn a_context_that_was_not_shut_down_drops_its_schema() {
    let ctx = TestContext::new(OpenShopConfig::builder().flow(noop_flow("sync")).build().unwrap())
        .await
        .unwrap();
    let schema: String = sqlx::query_scalar("SELECT current_schema()::text")
        .fetch_one(&ctx.db)
        .await
        .unwrap();
    assert!(schema.starts_with("test_"));

    drop(ctx);
    assert!(!schema_exists(&schema).await);
}

#[tokio::test]
async fn flows_can_be_dispatched_for_another_shop() {
    let ctx = TestContext::new(OpenShopConfig::builder().flow(noop_flow("sync")).build().unwrap())
        .await
        .unwrap();

    let own = ctx.dispatch_flow("sync", json!({})).await.unwrap();
    let other = ctx.dispatch_flow_for("sync", json!({}), OTHER_SHOP).await.unwrap();

    let shops: Vec<(uuid::Uuid, String)> = sqlx::query_as("SELECT id, shop FROM flow_runs ORDER BY created_at, shop")
        .fetch_all(&ctx.db)
        .await
        .unwrap();
    assert_eq!(shops.len(), 2);
    assert!(shops.contains(&(own, ctx.shop.clone())));
    assert!(shops.contains(&(other, OTHER_SHOP.to_string())));
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn authorization_headers_can_target_another_shop_and_user() {
    let ctx = TestContext::new(OpenShopConfig::builder().flow(noop_flow("sync")).build().unwrap())
        .await
        .unwrap();
    ctx.dispatch_flow_for("sync", json!({}), OTHER_SHOP).await.unwrap();

    let own: openshop::Value = ctx.get("/api/runs").send().await.unwrap().json().await.unwrap();
    assert_eq!(own.as_array().unwrap().len(), 0);

    let other: openshop::Value = ctx
        .http
        .get(ctx.url_for("/api/runs"))
        .header("Authorization", ctx.authorization_header_for(OTHER_SHOP, "42"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(other.as_array().unwrap().len(), 1);
    ctx.shutdown().await.unwrap();
}
