//! The purge of old log lines: `log_retention`, run by the workers in batches.

use std::time::{Duration, Instant};

use crate::common::{TestApp, create_config};
use chrono::Utc;
use openshop::OpenShopConfig;
use openshop::engine::Worker;
use openshop::engine::retention::{purge_logs, purge_logs_batch};
use sqlx::PgPool;

/// `count` lines created `days` days ago, and the same number now.
async fn seed(db: &PgPool, count: i32, days: i32) {
    sqlx::raw_sql(&format!(
        "INSERT INTO logs (flow_run_id, level, message, created_at) \
         SELECT 'run-' || n, 'info', 'old', now() - make_interval(days => {days}) FROM generate_series(1, {count}) AS n; \
         INSERT INTO logs (flow_run_id, level, message) \
         SELECT 'run-' || n, 'info', 'recent' FROM generate_series(1, {count}) AS n"
    ))
    .execute(db)
    .await
    .unwrap();
}

async fn count(db: &PgPool, message: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM logs WHERE message = $1")
        .bind(message)
        .fetch_one(db)
        .await
        .unwrap()
}

fn config_keeping(retention: Option<Duration>) -> OpenShopConfig {
    OpenShopConfig::builder().log_retention(retention).build().unwrap()
}

#[tokio::test]
async fn a_purge_removes_the_old_lines_in_batches_and_only_them() {
    let test = TestApp::new(create_config([])).await;
    seed(&test.db, 12, 40).await;
    let cutoff = Utc::now() - chrono::Duration::days(30);

    // One batch removes no more than its size, the oldest or not.
    assert_eq!(purge_logs_batch(&test.db, cutoff, 5).await.unwrap(), 5);
    assert_eq!(count(&test.db, "old").await, 7);
    assert_eq!(count(&test.db, "recent").await, 12);

    // The sweep goes on until nothing old is left.
    assert_eq!(purge_logs(&test.db, cutoff).await.unwrap(), 7);
    assert_eq!(count(&test.db, "old").await, 0);
    assert_eq!(count(&test.db, "recent").await, 12);
    assert_eq!(purge_logs(&test.db, cutoff).await.unwrap(), 0);
    test.cleanup().await;
}

#[tokio::test]
async fn a_purge_skips_the_lines_another_purge_holds() {
    let test = TestApp::new(create_config([])).await;
    seed(&test.db, 4, 40).await;
    let cutoff = Utc::now() - chrono::Duration::days(30);

    let mut holder = test.db.begin().await.unwrap();
    sqlx::query("SELECT 1 FROM logs WHERE message = 'old' FOR UPDATE")
        .fetch_all(&mut *holder)
        .await
        .unwrap();
    // It does not wait for the lock, and removes nothing it cannot lock.
    assert_eq!(purge_logs_batch(&test.db, cutoff, 100).await.unwrap(), 0);
    holder.rollback().await.unwrap();
    assert_eq!(purge_logs_batch(&test.db, cutoff, 100).await.unwrap(), 4);
    test.cleanup().await;
}

#[tokio::test]
async fn a_worker_purges_what_the_setting_says() {
    let test = TestApp::new(config_keeping(Some(Duration::from_secs(30 * 24 * 3600)))).await;
    seed(&test.db, 3, 40).await;

    let worker = Worker::new(test.app.clone(), openshop::WorkerConfig::new());
    worker.start().await;
    let deadline = Instant::now() + Duration::from_secs(10);
    while count(&test.db, "old").await > 0 {
        assert!(Instant::now() < deadline, "the old lines are still there");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    worker.stop().await;
    assert_eq!(count(&test.db, "recent").await, 3);
    test.cleanup().await;
}

#[tokio::test]
async fn without_the_setting_the_lines_are_kept() {
    let test = TestApp::new(config_keeping(None)).await;
    seed(&test.db, 3, 4000).await;

    let worker = Worker::new(test.app.clone(), openshop::WorkerConfig::new());
    worker.start().await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    worker.stop().await;
    assert_eq!(count(&test.db, "old").await, 3);
    test.cleanup().await;
}

#[test]
fn a_zero_retention_is_refused() {
    let error = OpenShopConfig::builder()
        .log_retention(Duration::ZERO)
        .build()
        .err()
        .expect("a zero retention is refused");
    assert!(
        error.to_string().contains("logRetention must be a positive number"),
        "{error}"
    );
}
