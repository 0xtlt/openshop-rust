//! Cancellation tokens of the runs executing in this process.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex, MutexGuard};

use tokio_util::sync::CancellationToken;

static TOKENS: LazyLock<Mutex<HashMap<String, Registered>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Tells the registrations of one run apart: a run claimed again while its
/// previous execution is still winding down is registered twice.
static GENERATION: AtomicU64 = AtomicU64::new(0);

struct Registered {
    generation: u64,
    token: CancellationToken,
}

/// The cancellation token of one execution of a run, registered by run id.
pub struct Abort {
    pub token: CancellationToken,
    run_id: String,
    generation: u64,
}

impl Abort {
    /// Forgets the token, unless a newer execution of the run replaced it.
    pub fn cleanup(&self) {
        let mut tokens = tokens();
        if tokens
            .get(&self.run_id)
            .is_some_and(|current| current.generation == self.generation)
        {
            tokens.remove(&self.run_id);
        }
    }
}

fn tokens() -> MutexGuard<'static, HashMap<String, Registered>> {
    TOKENS.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Registers an execution of a run. It replaces the token of a previous
/// execution of the same run, which a cancellation then no longer reaches.
pub fn register_abort(run_id: &str) -> Abort {
    let token = CancellationToken::new();
    let generation = GENERATION.fetch_add(1, Ordering::Relaxed);
    tokens().insert(
        run_id.to_string(),
        Registered {
            generation,
            token: token.clone(),
        },
    );
    Abort {
        token,
        run_id: run_id.to_string(),
        generation,
    }
}

/// Cancels a run executing in this process. Returns `false` when it is not running here.
pub fn cancel_run(run_id: &str) -> bool {
    match tokens().remove(run_id) {
        Some(registered) => {
            registered.token.cancel();
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_returns_a_live_token() {
        let abort = register_abort("abort-test-1");
        assert!(!abort.token.is_cancelled());
        abort.cleanup();
    }

    #[test]
    fn cancel_run_cancels_the_token_and_forgets_it() {
        let abort = register_abort("abort-test-2");
        assert!(cancel_run("abort-test-2"));
        assert!(abort.token.is_cancelled());
        assert!(!cancel_run("abort-test-2"));
    }

    #[test]
    fn cancel_unknown_run_returns_false() {
        assert!(!cancel_run("abort-test-unknown"));
    }

    #[test]
    fn cleanup_removes_the_token_without_cancelling() {
        let abort = register_abort("abort-test-3");
        abort.cleanup();
        assert!(!abort.token.is_cancelled());
        assert!(!cancel_run("abort-test-3"));
    }

    #[test]
    fn cleanup_of_a_replaced_execution_keeps_the_token_of_the_new_one() {
        let old = register_abort("abort-test-4");
        let new = register_abort("abort-test-4");
        old.cleanup();
        assert!(cancel_run("abort-test-4"));
        assert!(new.token.is_cancelled());
        assert!(!old.token.is_cancelled());
    }
}
