//! What the process remembers of the token exchange of the session token.
//!
//! A shop without an access token asks Shopify for one in the middleware of
//! `/api`, before the request is served. So that a shop that cannot get one
//! does not make every request wait for Shopify:
//!
//! - one request at a time asks for an app and shop: the others wait for its
//!   turn, then use the token it stored, or fall back at once when it failed.
//!   The turn of a shop never waits for the turn of another.
//! - a failure (a refusal, an error, a timeout) holds back the next requests
//!   of the app and shop for [`HOLD_BACK`].
//! - Shopify is asked for [`TIMEOUT`] at most.

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex, MutexGuard};
use std::time::Duration;

use tokio::sync::{Mutex, OwnedMutexGuard};
use tokio::time::Instant;

/// How long the request of the exchange may take, whatever the transport
/// allows: a request waits for it, and the requests of the shop wait for it.
pub(crate) const TIMEOUT: Duration = Duration::from_secs(5);

/// How long a failed exchange holds back another one for the same app and shop.
pub(crate) const HOLD_BACK: Duration = Duration::from_secs(30);

type Key = (String, String);

#[derive(Default)]
pub(crate) struct TokenExchanges {
    /// The shops whose exchange is out, or waited for: the lock of a shop is
    /// dropped from the map once nobody holds or waits for it.
    turns: StdMutex<HashMap<Key, Arc<Mutex<()>>>>,
    /// Until when each app and shop that failed is held back.
    held: StdMutex<HashMap<Key, Instant>>,
}

/// The turn of an app and shop to ask Shopify; given up when dropped.
pub(crate) struct Turn<'a> {
    exchanges: &'a TokenExchanges,
    key: Key,
    guard: Option<OwnedMutexGuard<()>>,
}

fn key(shopify_app: &str, shop: &str) -> Key {
    (shopify_app.to_string(), shop.to_string())
}

fn locked<T>(mutex: &StdMutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl TokenExchanges {
    /// Whether a failure still holds back the exchange of this app and shop.
    pub fn held_back(&self, shopify_app: &str, shop: &str) -> bool {
        locked(&self.held)
            .get(&key(shopify_app, shop))
            .is_some_and(|held_until| Instant::now() < *held_until)
    }

    /// Remembers that the exchange of this app and shop failed. What no
    /// longer holds anything back is forgotten.
    pub fn hold_back(&self, shopify_app: &str, shop: &str) {
        let now = Instant::now();
        let mut held = locked(&self.held);
        held.retain(|_, held_until| now < *held_until);
        held.insert(key(shopify_app, shop), now + HOLD_BACK);
    }

    /// Waits for the turn of this app and shop.
    pub async fn turn(&self, shopify_app: &str, shop: &str) -> Turn<'_> {
        let key = key(shopify_app, shop);
        let lock = locked(&self.turns).entry(key.clone()).or_default().clone();
        Turn {
            exchanges: self,
            key,
            guard: Some(lock.lock_owned().await),
        }
    }
}

impl Drop for Turn<'_> {
    fn drop(&mut self) {
        // Released before the count: the map and the waiters hold the others.
        drop(self.guard.take());
        let mut turns = locked(&self.exchanges.turns);
        if turns.get(&self.key).is_some_and(|lock| Arc::strong_count(lock) == 1) {
            turns.remove(&self.key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_turn_is_forgotten_once_nobody_waits_for_it() {
        let exchanges = TokenExchanges::default();
        let first = exchanges.turn("default", "a.myshopify.com").await;
        let other = exchanges.turn("default", "b.myshopify.com").await;
        assert_eq!(locked(&exchanges.turns).len(), 2);
        drop(other);
        assert_eq!(locked(&exchanges.turns).len(), 1);
        drop(first);
        assert!(locked(&exchanges.turns).is_empty());
    }

    #[tokio::test]
    async fn a_turn_waits_for_the_turn_of_the_same_shop_only() {
        let exchanges = Arc::new(TokenExchanges::default());
        let first = exchanges.turn("default", "a.myshopify.com").await;
        // Another shop is served at once.
        drop(exchanges.turn("default", "b.myshopify.com").await);

        let waiting = tokio::spawn({
            let exchanges = exchanges.clone();
            async move { drop(exchanges.turn("default", "a.myshopify.com").await) }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiting.is_finished());
        drop(first);
        waiting.await.unwrap();
        assert!(locked(&exchanges.turns).is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_failure_holds_back_for_thirty_seconds_then_is_forgotten() {
        let exchanges = TokenExchanges::default();
        assert!(!exchanges.held_back("default", "a.myshopify.com"));
        exchanges.hold_back("default", "a.myshopify.com");
        assert!(exchanges.held_back("default", "a.myshopify.com"));
        assert!(!exchanges.held_back("default", "b.myshopify.com"));
        assert!(!exchanges.held_back("other", "a.myshopify.com"));

        tokio::time::advance(HOLD_BACK + Duration::from_secs(1)).await;
        assert!(!exchanges.held_back("default", "a.myshopify.com"));
        exchanges.hold_back("default", "b.myshopify.com");
        assert_eq!(locked(&exchanges.held).len(), 1);
    }
}
