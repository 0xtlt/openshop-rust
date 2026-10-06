//! Cancellation tokens of the runs executing in this process.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, MutexGuard};

use tokio_util::sync::CancellationToken;

static TOKENS: LazyLock<Mutex<HashMap<String, CancellationToken>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn tokens() -> MutexGuard<'static, HashMap<String, CancellationToken>> {
    TOKENS.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub fn register_abort(run_id: &str) -> CancellationToken {
    let token = CancellationToken::new();
    tokens().insert(run_id.to_string(), token.clone());
    token
}

/// Cancels a run executing in this process. Returns `false` when it is not running here.
pub fn cancel_run(run_id: &str) -> bool {
    match tokens().remove(run_id) {
        Some(token) => {
            token.cancel();
            true
        }
        None => false,
    }
}

pub fn cleanup_abort(run_id: &str) {
    tokens().remove(run_id);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_returns_a_live_token() {
        let token = register_abort("abort-test-1");
        assert!(!token.is_cancelled());
        cleanup_abort("abort-test-1");
    }

    #[test]
    fn cancel_run_cancels_the_token_and_forgets_it() {
        let token = register_abort("abort-test-2");
        assert!(cancel_run("abort-test-2"));
        assert!(token.is_cancelled());
        assert!(!cancel_run("abort-test-2"));
    }

    #[test]
    fn cancel_unknown_run_returns_false() {
        assert!(!cancel_run("abort-test-unknown"));
    }

    #[test]
    fn cleanup_removes_the_token_without_cancelling() {
        let token = register_abort("abort-test-3");
        cleanup_abort("abort-test-3");
        assert!(!token.is_cancelled());
        assert!(!cancel_run("abort-test-3"));
    }
}
