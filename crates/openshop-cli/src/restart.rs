//! Serializes restarts of the development process.
//!
//! Changes that arrive while a restart is in progress are coalesced into a
//! single follow-up restart.

use std::future::Future;
use std::sync::{Arc, Mutex};

/// What the coordinator needs from the process it manages.
pub trait Restartable: Send + Sync + 'static {
    /// Stops the current process (if any) and starts a new one.
    fn restart(&self, reason: String) -> impl Future<Output = ()> + Send;
    /// Called when a restart is requested while another one is running.
    fn queued(&self, _reason: &str) {}
}

#[derive(Default)]
struct State {
    restarting: bool,
    pending_reason: Option<String>,
}

pub struct RestartCoordinator<R: Restartable> {
    target: Arc<R>,
    state: Arc<Mutex<State>>,
}

impl<R: Restartable> Clone for RestartCoordinator<R> {
    fn clone(&self) -> Self {
        Self {
            target: self.target.clone(),
            state: self.state.clone(),
        }
    }
}

impl<R: Restartable> RestartCoordinator<R> {
    pub fn new(target: Arc<R>) -> Self {
        Self {
            target,
            state: Arc::new(Mutex::new(State::default())),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Restarts now, or queues one more restart if one is already running.
    /// Returns once no restart is left to do for this request.
    pub async fn request_restart(&self, reason: impl Into<String>) {
        let mut reason = reason.into();
        {
            let mut state = self.state();
            if state.restarting {
                if state.pending_reason.is_none() {
                    self.target.queued(&reason);
                    state.pending_reason = Some(reason);
                }
                return;
            }
            state.restarting = true;
        }

        loop {
            self.target.restart(reason).await;
            let mut state = self.state();
            match state.pending_reason.take() {
                Some(next) => reason = next,
                None => {
                    state.restarting = false;
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[derive(Default)]
    struct Recorder {
        restarts: Mutex<Vec<String>>,
        queued: Mutex<Vec<String>>,
        running: Mutex<u32>,
        max_running: Mutex<u32>,
    }

    impl Restartable for Recorder {
        async fn restart(&self, reason: String) {
            {
                let mut running = self.running.lock().unwrap();
                *running += 1;
                let mut max = self.max_running.lock().unwrap();
                *max = (*max).max(*running);
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
            self.restarts.lock().unwrap().push(reason);
            *self.running.lock().unwrap() -= 1;
        }

        fn queued(&self, reason: &str) {
            self.queued.lock().unwrap().push(reason.to_string());
        }
    }

    #[tokio::test]
    async fn restarts_immediately_when_idle() {
        let recorder = Arc::new(Recorder::default());
        let coordinator = RestartCoordinator::new(recorder.clone());

        coordinator.request_restart("src/main.rs").await;
        coordinator.request_restart("Cargo.toml").await;

        assert_eq!(*recorder.restarts.lock().unwrap(), vec!["src/main.rs", "Cargo.toml"]);
        assert!(recorder.queued.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn coalesces_requests_made_during_a_restart() {
        let recorder = Arc::new(Recorder::default());
        let coordinator = RestartCoordinator::new(recorder.clone());

        let first = tokio::spawn({
            let coordinator = coordinator.clone();
            async move { coordinator.request_restart("first").await }
        });
        tokio::time::sleep(Duration::from_millis(5)).await;
        coordinator.request_restart("second").await;
        coordinator.request_restart("third").await;
        first.await.unwrap();

        assert_eq!(*recorder.restarts.lock().unwrap(), vec!["first", "second"]);
        assert_eq!(*recorder.queued.lock().unwrap(), vec!["second"]);
        assert_eq!(*recorder.max_running.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn accepts_new_restarts_after_draining_the_queue() {
        let recorder = Arc::new(Recorder::default());
        let coordinator = RestartCoordinator::new(recorder.clone());

        let first = tokio::spawn({
            let coordinator = coordinator.clone();
            async move { coordinator.request_restart("first").await }
        });
        tokio::time::sleep(Duration::from_millis(5)).await;
        coordinator.request_restart("queued").await;
        first.await.unwrap();
        coordinator.request_restart("later").await;

        assert_eq!(*recorder.restarts.lock().unwrap(), vec!["first", "queued", "later"]);
    }
}
