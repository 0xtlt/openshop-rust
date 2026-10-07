//! The connection pool: when an idle connection is checked before it is used.

use std::str::FromStr;
use std::time::Duration;

use openshop::db::pool_options;
use openshop::{PgPool, Uuid};
use sqlx::postgres::PgConnectOptions;
use sqlx::{Connection, PgConnection};

const POOLED: usize = 3;

fn connect_options() -> PgConnectOptions {
    let url = std::env::var("TEST_DATABASE_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .unwrap_or_else(|_| openshop::runtime::env::DEFAULT_DATABASE_URL.to_string());
    PgConnectOptions::from_str(&url).unwrap()
}

/// A pool of `pool_options()` holding `POOLED` idle connections that the
/// server has just closed, as a restart or a failover does.
async fn pool_of_closed_connections() -> PgPool {
    let name = format!("pool-test-{}", Uuid::new_v4().simple());
    let mut admin = PgConnection::connect_with(&connect_options()).await.unwrap();
    let pool = pool_options()
        .connect_with(connect_options().application_name(&name))
        .await
        .unwrap();

    let mut connections = Vec::new();
    for _ in 0..POOLED {
        connections.push(pool.acquire().await.unwrap());
    }
    drop(connections);
    while pool.num_idle() < POOLED {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }

    sqlx::query("SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE application_name = $1")
        .bind(&name)
        .execute(&mut admin)
        .await
        .unwrap();
    loop {
        let open: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE application_name = $1")
            .bind(&name)
            .fetch_one(&mut admin)
            .await
            .unwrap();
        if open == 0 {
            break;
        }
    }
    admin.close().await.unwrap();
    pool
}

/// Which of `count` statements run one after the other on the pool succeed.
async fn run_statements(pool: &PgPool, count: usize) -> Vec<bool> {
    let mut succeeded = Vec::new();
    for _ in 0..count {
        succeeded.push(sqlx::query("SELECT 1").execute(pool).await.is_ok());
    }
    succeeded
}

#[tokio::test]
async fn a_connection_closed_while_it_waited_is_replaced_without_an_error() {
    let pool = pool_of_closed_connections().await;

    // Past the second after which an idle connection is pinged before use.
    tokio::time::sleep(Duration::from_millis(1_100)).await;

    assert_eq!(run_statements(&pool, POOLED * 2).await, vec![true; POOLED * 2]);
    pool.close().await;
}

#[tokio::test]
async fn a_connection_closed_right_after_use_fails_one_statement() {
    let pool = pool_of_closed_connections().await;

    // Used again within the second: no ping, so each closed connection fails
    // the statement it is given, then leaves the pool.
    assert_eq!(run_statements(&pool, POOLED).await, vec![false; POOLED]);
    assert_eq!(run_statements(&pool, POOLED * 2).await, vec![true; POOLED * 2]);
    pool.close().await;
}
