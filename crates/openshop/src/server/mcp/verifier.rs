//! Checks the secret of an MCP token against its scrypt hash, at a bounded cost.
//!
//! A scrypt hash is slow and takes 16 MiB on purpose. The stored format is
//! shared with the TypeScript runtime, so it cannot change, but who may spend
//! that cost can: `POST /mcp` asks for it before it knows who is calling.
//!
//! - At most a few hashes run at once, each on a thread of its own. A caller
//!   that cannot get a turn in time is told to come back, and the others keep
//!   their share of the CPU and the memory.
//! - A secret that was right is remembered for a few minutes, under a keyed
//!   hash of the secret and of the stored hash (the key is random and lives
//!   in this process). A client that calls often pays scrypt once. Rotating a
//!   token changes the stored hash, so the old secret finds nothing.
//!
//! Only the scrypt step is remembered. Whether a token is active, revoked or
//! expired is read from the database at each request, as ever.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};

use anyhow::Result;
use hmac::{Hmac, Mac};
use rand::Rng;
use sha2::Sha256;
use tokio::sync::Semaphore;

use super::tokens::verify_mcp_token;

/// How long a secret that was right is remembered.
const VERIFIED_FOR: Duration = Duration::from_secs(300);
/// How long a request waits for its turn to run scrypt.
const MAX_WAIT: Duration = Duration::from_secs(5);
/// Above this many remembered secrets, the ones that expired are dropped, then all.
const MAX_REMEMBERED: usize = 1024;
/// Hashes that run at the same time, at most.
const MAX_CONCURRENT: usize = 4;

type Fingerprint = [u8; 32];

/// What came of checking a secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verification {
    Valid,
    Invalid,
    /// No turn came up in time: the caller should try again later.
    Busy,
}

pub(crate) struct TokenVerifier {
    turns: Arc<Semaphore>,
    wait: Duration,
    remember: Duration,
    key: [u8; 32],
    verified: Mutex<HashMap<Fingerprint, Instant>>,
    hash: fn(&str, &str) -> bool,
}

static SHARED: LazyLock<TokenVerifier> = LazyLock::new(|| {
    let cpus = std::thread::available_parallelism().map_or(1, usize::from);
    TokenVerifier::new(cpus.clamp(1, MAX_CONCURRENT), MAX_WAIT, VERIFIED_FOR, verify_mcp_token)
});

impl TokenVerifier {
    /// The verifier of the process.
    pub(crate) fn shared() -> &'static Self {
        &SHARED
    }

    pub(crate) fn new(concurrent: usize, wait: Duration, remember: Duration, hash: fn(&str, &str) -> bool) -> Self {
        let mut key = [0u8; 32];
        rand::rng().fill_bytes(&mut key);
        Self {
            turns: Arc::new(Semaphore::new(concurrent)),
            wait,
            remember,
            key,
            verified: Mutex::new(HashMap::new()),
            hash,
        }
    }

    pub(crate) async fn verify(&self, token: &str, stored_hash: &str) -> Result<Verification> {
        let fingerprint = self.fingerprint(token, stored_hash);
        if self.is_remembered(&fingerprint) {
            return Ok(Verification::Valid);
        }

        let Ok(Ok(turn)) = tokio::time::timeout(self.wait, self.turns.clone().acquire_owned()).await else {
            return Ok(Verification::Busy);
        };
        // The request ahead in line may have just checked the same secret.
        if self.is_remembered(&fingerprint) {
            return Ok(Verification::Valid);
        }

        // The turn goes with the thread: a request that is dropped meanwhile
        // (timeout, closed connection) does not free it before scrypt is done.
        let hash = self.hash;
        let (token, stored_hash) = (token.to_string(), stored_hash.to_string());
        let valid = tokio::task::spawn_blocking(move || {
            let _turn = turn;
            hash(&token, &stored_hash)
        })
        .await?;
        if !valid {
            return Ok(Verification::Invalid);
        }
        self.remember(fingerprint);
        Ok(Verification::Valid)
    }

    fn fingerprint(&self, token: &str, stored_hash: &str) -> Fingerprint {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.key).expect("HMAC accepts any key size");
        mac.update(token.as_bytes());
        mac.update(&[0]);
        mac.update(stored_hash.as_bytes());
        mac.finalize().into_bytes().into()
    }

    fn is_remembered(&self, fingerprint: &Fingerprint) -> bool {
        let verified = self.verified.lock().unwrap_or_else(PoisonError::into_inner);
        verified.get(fingerprint).is_some_and(|until| *until > Instant::now())
    }

    fn remember(&self, fingerprint: Fingerprint) {
        let now = Instant::now();
        let mut verified = self.verified.lock().unwrap_or_else(PoisonError::into_inner);
        if verified.len() >= MAX_REMEMBERED {
            verified.retain(|_, until| *until > now);
            if verified.len() >= MAX_REMEMBERED {
                verified.clear();
            }
        }
        verified.insert(fingerprint, now + self.remember);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn quick_check(token: &str, stored_hash: &str) -> bool {
        token == stored_hash
    }

    fn slow_check(token: &str, stored_hash: &str) -> bool {
        std::thread::sleep(Duration::from_millis(30));
        token == stored_hash
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn hashes_never_run_more_than_the_turns_allow() {
        static RUNNING: AtomicUsize = AtomicUsize::new(0);
        static MOST_RUNNING: AtomicUsize = AtomicUsize::new(0);
        static CHECKS: AtomicUsize = AtomicUsize::new(0);
        // Stands for scrypt: slow, and it counts how many run at once.
        fn counting_check(token: &str, stored_hash: &str) -> bool {
            CHECKS.fetch_add(1, Ordering::SeqCst);
            let running = RUNNING.fetch_add(1, Ordering::SeqCst) + 1;
            MOST_RUNNING.fetch_max(running, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(30));
            RUNNING.fetch_sub(1, Ordering::SeqCst);
            token == stored_hash
        }

        let verifier = Arc::new(TokenVerifier::new(
            2,
            Duration::from_secs(30),
            Duration::from_secs(60),
            counting_check,
        ));
        let calls: Vec<_> = (0..16)
            .map(|index| {
                let verifier = verifier.clone();
                // Different secrets: nothing to remember between them.
                tokio::spawn(async move { verifier.verify(&format!("wrong-{index}"), "stored").await.unwrap() })
            })
            .collect();
        for call in calls {
            assert_eq!(call.await.unwrap(), Verification::Invalid);
        }
        assert_eq!(CHECKS.load(Ordering::SeqCst), 16);
        assert_eq!(MOST_RUNNING.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_secret_that_was_right_is_not_hashed_again() {
        static CHECKS: AtomicUsize = AtomicUsize::new(0);
        fn counting_check(token: &str, stored_hash: &str) -> bool {
            CHECKS.fetch_add(1, Ordering::SeqCst);
            token == stored_hash
        }
        let verifier = TokenVerifier::new(1, Duration::from_secs(1), Duration::from_secs(60), counting_check);

        for _ in 0..3 {
            assert_eq!(verifier.verify("secret", "secret").await.unwrap(), Verification::Valid);
        }
        assert_eq!(CHECKS.load(Ordering::SeqCst), 1);

        // A wrong secret is hashed every time and never remembered.
        for _ in 0..2 {
            assert_eq!(verifier.verify("other", "secret").await.unwrap(), Verification::Invalid);
        }
        assert_eq!(CHECKS.load(Ordering::SeqCst), 3);

        // The stored hash changed (rotation): the old secret is checked again.
        assert_eq!(verifier.verify("secret", "rotated").await.unwrap(), Verification::Invalid);
        assert_eq!(CHECKS.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn a_remembered_secret_is_forgotten_after_its_time() {
        let verifier = TokenVerifier::new(1, Duration::from_secs(1), Duration::from_millis(20), quick_check);
        assert_eq!(verifier.verify("secret", "secret").await.unwrap(), Verification::Valid);
        let fingerprint = verifier.fingerprint("secret", "secret");
        assert!(verifier.is_remembered(&fingerprint));
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert!(!verifier.is_remembered(&fingerprint));
    }

    #[tokio::test]
    async fn a_request_that_waits_too_long_is_told_to_come_back() {
        let verifier = Arc::new(TokenVerifier::new(
            1,
            Duration::from_millis(10),
            Duration::from_secs(60),
            slow_check,
        ));
        let holder = {
            let verifier = verifier.clone();
            tokio::spawn(async move { verifier.verify("a", "a").await.unwrap() })
        };
        tokio::time::sleep(Duration::from_millis(5)).await;
        assert_eq!(verifier.verify("b", "b").await.unwrap(), Verification::Busy);
        assert_eq!(holder.await.unwrap(), Verification::Valid);
    }

    #[tokio::test]
    async fn the_memory_of_secrets_is_bounded() {
        let verifier = TokenVerifier::new(1, Duration::from_secs(1), Duration::from_secs(60), quick_check);
        for index in 0..MAX_REMEMBERED + 10 {
            let secret = format!("secret-{index}");
            assert_eq!(verifier.verify(&secret, &secret).await.unwrap(), Verification::Valid);
        }
        let remembered = verifier.verified.lock().unwrap().len();
        assert!(remembered <= MAX_REMEMBERED, "{remembered}");
    }
}
