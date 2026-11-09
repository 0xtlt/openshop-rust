//! The plans read from the Partner API, kept per shop for a while.
//!
//! The Partner API takes four requests a second per Partner API client, for
//! every shop and every process of the organization together: a plan read at
//! each request of each merchant would go over it. So each process keeps, per
//! Shopify app and shop:
//!
//! - the GIDs of the app and of the shop, which never change: read once from
//!   the Admin API;
//! - the last answer of `activeSubscription`, for the `plan_ttl` of the
//!   [Partner API](crate::PartnerApi) (five minutes by default).
//!
//! A check that the answer does not pass (no plan, or not the one asked)
//! asks again, unless the answer is younger than [`MIN_REFRESH`]: a merchant
//! who just chose a plan gets it at once, and a merchant without one costs at
//! most a request every ten seconds. A verified launch of the app with
//! `plan_handle` (the welcome link of a plan) and an uninstall forget the
//! answer. What the cache can miss is a plan that ends outside the app (a
//! cancellation, a freeze): it is seen within `plan_ttl`.
//!
//! Concurrent checks of a shop wait for one request rather than each sending
//! its own.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::partner::{AppPricingSubscription, PartnerIds};
use crate::locked;

/// An answer younger than this is not asked again, even when a check does
/// not pass with it.
pub const MIN_REFRESH: Duration = Duration::from_secs(10);

type Key = (String, String);

/// What a process keeps of the shops, per Shopify app and shop.
#[derive(Default)]
pub(crate) struct PlanCache {
    shops: Mutex<HashMap<Key, Arc<tokio::sync::Mutex<Entry>>>>,
}

/// What is kept of a shop. Its lock is held while Shopify is asked, so that
/// concurrent checks send one request.
#[derive(Default)]
pub(crate) struct Entry {
    pub(crate) ids: Option<PartnerIds>,
    pub(crate) answer: Option<Answer>,
}

/// An answer of `activeSubscription`, and when it was asked.
#[derive(Debug, Clone)]
pub(crate) struct Answer {
    pub(crate) asked_at: Instant,
    pub(crate) subscription: Option<AppPricingSubscription>,
}

impl Answer {
    /// Whether this answer serves a check at `now` without asking Shopify:
    /// it is younger than `ttl`, and the check `passes` with it or it is
    /// younger than [`MIN_REFRESH`].
    pub(crate) fn serves(&self, now: Instant, ttl: Duration, passes: bool) -> bool {
        let age = now.saturating_duration_since(self.asked_at);
        age < ttl && (passes || age < MIN_REFRESH)
    }
}

impl PlanCache {
    /// The entry of a shop, created empty.
    pub(crate) fn entry(&self, shopify_app: &str, shop: &str) -> Arc<tokio::sync::Mutex<Entry>> {
        locked(&self.shops)
            .entry((shopify_app.to_string(), shop.to_string()))
            .or_default()
            .clone()
    }

    /// Forgets the plan of a shop, not its GIDs: the next check asks the
    /// Partner API.
    pub(crate) async fn forget(&self, shopify_app: &str, shop: &str) {
        let entry = locked(&self.shops)
            .get(&(shopify_app.to_string(), shop.to_string()))
            .cloned();
        if let Some(entry) = entry {
            entry.lock().await.answer = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TTL: Duration = Duration::from_secs(300);

    fn answer(age: Duration, now: Instant) -> Answer {
        Answer {
            asked_at: now - age,
            subscription: None,
        }
    }

    #[test]
    fn an_answer_serves_the_checks_it_passes_until_its_ttl() {
        let now = Instant::now() + Duration::from_secs(3600);
        assert!(answer(Duration::from_secs(299), now).serves(now, TTL, true));
        assert!(!answer(Duration::from_secs(300), now).serves(now, TTL, true));
    }

    #[test]
    fn a_check_it_fails_asks_again_after_ten_seconds() {
        let now = Instant::now() + Duration::from_secs(3600);
        assert!(answer(Duration::from_secs(9), now).serves(now, TTL, false));
        assert!(!answer(Duration::from_secs(10), now).serves(now, TTL, false));
        // A ttl under ten seconds is the ttl.
        assert!(!answer(Duration::from_secs(2), now).serves(now, Duration::from_secs(2), false));
        // Nothing is kept with no ttl.
        assert!(!answer(Duration::ZERO, now).serves(now, Duration::ZERO, true));
    }

    #[tokio::test]
    async fn forgetting_keeps_the_ids() {
        let cache = PlanCache::default();
        let entry = cache.entry("default", "a.myshopify.com");
        {
            let mut entry = entry.lock().await;
            entry.ids = Some(PartnerIds {
                app: "gid://shopify/App/1".into(),
                shop: "gid://shopify/Shop/1".into(),
            });
            entry.answer = Some(answer(Duration::ZERO, Instant::now()));
        }
        cache.forget("default", "a.myshopify.com").await;
        // Another shop, or one never asked, is nothing to forget.
        cache.forget("default", "b.myshopify.com").await;
        let entry = cache.entry("default", "a.myshopify.com");
        let entry = entry.lock().await;
        assert!(entry.answer.is_none());
        assert_eq!(entry.ids.as_ref().unwrap().shop, "gid://shopify/Shop/1");
    }
}
