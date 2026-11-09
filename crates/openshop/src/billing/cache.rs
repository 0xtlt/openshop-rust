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
//! `plan_handle` (the welcome link of a plan) forgets the answer, unless it
//! is younger than [`MIN_REFRESH`] and has that plan already: the route of
//! the welcome link just read it. An uninstall forgets the answer. What the cache can miss is a plan that ends outside the app (a
//! cancellation, a freeze): it is seen within `plan_ttl`.
//!
//! Concurrent checks of a shop wait for one request rather than each sending
//! its own.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
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
    /// Whether a read of the Admin API's subscriptions, with the Partner API
    /// set, was warned about.
    admin_api_warned: AtomicBool,
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

    /// Whether this answer was read after the plan `plan_handle` was chosen,
    /// so that the welcome link of that plan keeps it: it is younger than
    /// [`MIN_REFRESH`] and has that plan. Its age alone is not enough: the
    /// check that sent the merchant to the plans kept a "no plan" answer
    /// that can be a few seconds old too.
    pub(crate) fn has_the_plan_just_chosen(&self, now: Instant, plan_handle: &str) -> bool {
        let handle = self
            .subscription
            .as_ref()
            .and_then(|subscription| subscription.plan_item())
            .and_then(|item| item.handle.as_deref());
        now.saturating_duration_since(self.asked_at) < MIN_REFRESH && handle == Some(plan_handle)
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

    /// `true` the first time it is called, `false` after: the warning about
    /// a read of the Admin API's subscriptions is logged once per process.
    pub(crate) fn first_admin_api_read(&self) -> bool {
        !self.admin_api_warned.swap(true, Ordering::Relaxed)
    }

    /// Forgets the plan of a shop, not its GIDs: the next check asks the
    /// Partner API.
    pub(crate) async fn forget(&self, shopify_app: &str, shop: &str) {
        self.forget_unless(shopify_app, shop, |_| false).await;
    }

    /// Forgets the plan of a shop on the welcome link of a plan, a verified
    /// launch with `plan_handle` (or only `charge_id`), unless the kept
    /// answer has that plan just chosen ([`Answer::has_the_plan_just_chosen`]):
    /// the route of the welcome link read it, and the home of the app it
    /// sends to would ask the Partner API a second time.
    pub(crate) async fn forget_on_welcome(&self, shopify_app: &str, shop: &str, plan_handle: Option<&str>) {
        let now = Instant::now();
        self.forget_unless(shopify_app, shop, |answer| {
            plan_handle.is_some_and(|handle| answer.has_the_plan_just_chosen(now, handle))
        })
        .await;
    }

    /// Forgets the kept answer of a shop, unless `keep` says to keep it.
    async fn forget_unless(&self, shopify_app: &str, shop: &str, keep: impl FnOnce(&Answer) -> bool) {
        let entry = locked(&self.shops)
            .get(&(shopify_app.to_string(), shop.to_string()))
            .cloned();
        if let Some(entry) = entry {
            let mut entry = entry.lock().await;
            if !entry.answer.as_ref().is_some_and(keep) {
                entry.answer = None;
            }
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

    #[test]
    fn the_welcome_of_a_plan_keeps_an_answer_with_that_plan_read_just_now() {
        let now = Instant::now() + Duration::from_secs(3600);
        let pro = |age: Duration| Answer {
            asked_at: now - age,
            subscription: Some(crate::testing::app_pricing_subscription("pro")),
        };
        assert!(pro(Duration::from_secs(9)).has_the_plan_just_chosen(now, "pro"));
        // Older: it may be from before the plan was chosen.
        assert!(!pro(Duration::from_secs(10)).has_the_plan_just_chosen(now, "pro"));
        // Another plan, or none: the check that sent the merchant to the plans.
        assert!(!pro(Duration::ZERO).has_the_plan_just_chosen(now, "plus"));
        assert!(!answer(Duration::ZERO, now).has_the_plan_just_chosen(now, "pro"));
    }

    #[tokio::test]
    async fn the_welcome_of_a_plan_forgets_any_other_answer() {
        let cache = PlanCache::default();
        let cache = &cache;
        let kept = |subscription| async move {
            let entry = cache.entry("default", "a.myshopify.com");
            entry.lock().await.answer = Some(Answer {
                asked_at: Instant::now(),
                subscription,
            });
        };
        let is_kept = || async move { cache.entry("default", "a.myshopify.com").lock().await.answer.is_some() };
        let pro = || Some(crate::testing::app_pricing_subscription("pro"));

        kept(pro()).await;
        cache.forget_on_welcome("default", "a.myshopify.com", Some("pro")).await;
        assert!(is_kept().await);
        cache
            .forget_on_welcome("default", "a.myshopify.com", Some("plus"))
            .await;
        assert!(!is_kept().await);
        kept(None).await;
        cache.forget_on_welcome("default", "a.myshopify.com", Some("pro")).await;
        assert!(!is_kept().await);
        // `charge_id` alone names no plan: the answer is forgotten.
        kept(pro()).await;
        cache.forget_on_welcome("default", "a.myshopify.com", None).await;
        assert!(!is_kept().await);
        // `refresh_plans()` and an uninstall forget it whatever it has.
        kept(pro()).await;
        cache.forget("default", "a.myshopify.com").await;
        assert!(!is_kept().await);
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
