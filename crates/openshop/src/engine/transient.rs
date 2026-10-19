//! Retries of the writes that record where a run stands, after an error that
//! a moment later is gone: a lost connection, a pool that timed out.

use std::future::Future;
use std::time::Duration;

use anyhow::Result;

use crate::runtime::logger;

/// How often a write is made before its error is final.
const TRIES: u32 = 5;

/// Delay before the second try; it doubles at each one after.
const FIRST_DELAY: Duration = Duration::from_millis(100);

/// Whether an error comes from the connection to the database or from its
/// load, not from the statement or the data. The same statement may succeed
/// a moment later.
pub(crate) fn is_transient(error: &anyhow::Error) -> bool {
    error
        .chain()
        .filter_map(|cause| cause.downcast_ref::<sqlx::Error>())
        .any(|error| match error {
            sqlx::Error::Io(_) | sqlx::Error::Tls(_) | sqlx::Error::PoolTimedOut | sqlx::Error::WorkerCrashed => true,
            sqlx::Error::Database(error) => error.code().is_some_and(|code| {
                // Connection exception, insufficient resources, server shutting
                // down or crashed, serialization failure and deadlock.
                code.starts_with("08")
                    || code.starts_with("53")
                    || matches!(&*code, "57P01" | "57P02" | "57P03" | "40001" | "40P01")
            }),
            _ => false,
        })
}

/// Runs `write` until it succeeds, fails with an error that is not transient,
/// or used all its tries. Every failure is logged. The statements it makes
/// must be safe to repeat: those of the engine carry the ownership of the run
/// in their `WHERE` clause, so a try that comes after the run was lost writes
/// nothing.
pub(crate) async fn retry<T, F, Fut>(what: String, mut write: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let mut delay = FIRST_DELAY;
    for attempt in 1.. {
        let error = match write().await {
            Ok(value) => return Ok(value),
            Err(error) => error,
        };
        if !is_transient(&error) {
            return Err(error);
        }
        if attempt >= TRIES {
            logger::warn(format!("[openshop] Gave up {what} after {attempt} tries: {error:#}"));
            return Err(error);
        }
        logger::warn(format!(
            "[openshop] Failed {what} (try {attempt} of {TRIES}), trying again in {}ms: {error:#}",
            delay.as_millis()
        ));
        tokio::time::sleep(delay).await;
        delay *= 2;
    }
    unreachable!("the loop ends with a return")
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use anyhow::Context;

    use super::*;

    #[test]
    fn a_lost_connection_is_transient_a_bad_statement_is_not() {
        let io = sqlx::Error::Io(std::io::Error::from(std::io::ErrorKind::ConnectionReset));
        assert!(is_transient(&anyhow::Error::new(io).context("while saving")));
        assert!(is_transient(&sqlx::Error::PoolTimedOut.into()));
        assert!(!is_transient(&sqlx::Error::RowNotFound.into()));
        assert!(!is_transient(&sqlx::Error::PoolClosed.into()));
        assert!(!is_transient(&anyhow::anyhow!("boom")));
    }

    #[tokio::test]
    async fn tries_again_only_after_a_transient_error() {
        let calls = AtomicU32::new(0);
        let value = retry("saving".into(), || async {
            if calls.fetch_add(1, Ordering::SeqCst) < 2 {
                return Err(sqlx::Error::PoolTimedOut).context("saving");
            }
            Ok(7)
        })
        .await
        .unwrap();
        assert_eq!((value, calls.load(Ordering::SeqCst)), (7, 3));

        let calls = AtomicU32::new(0);
        let result: Result<()> = retry("saving".into(), || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(sqlx::Error::RowNotFound.into())
        })
        .await;
        assert!(result.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn gives_up_after_its_tries() {
        let calls = AtomicU32::new(0);
        let result: Result<()> = retry("saving".into(), || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(sqlx::Error::PoolTimedOut.into())
        })
        .await;
        assert!(result.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), TRIES);
    }
}
