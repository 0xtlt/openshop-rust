//! The locale of the staff member behind a request of the admin pages: the
//! `locale` Shopify opens the app with, the `X-OpenShop-Locale` header
//! `openshop.js` sends (`shopify.config.locale`), else the one their last
//! request gave, which the server keeps for the requests that follow.

use std::collections::HashMap;
use std::sync::Mutex;

use axum::http::HeaderMap;
use indexmap::IndexMap;

use super::http::header;
use super::shop::ShopSession;
use crate::admin::is_language_tag;
use crate::app::OpenShop;

/// The header of `openshop.js` with the staff member's locale.
pub(crate) const LOCALE_HEADER: &str = "x-openshop-locale";
/// How many staff members the server keeps the locale of: past that, it
/// starts over, and a request without a locale is in the default language.
const KEPT: usize = 10_000;

/// The locale of each staff member, by app, shop and staff user, as their
/// last request gave it. In the memory of the process: another instance
/// knows it from the header of the next request of `openshop.js`.
#[derive(Default)]
pub(crate) struct StaffLocales(Mutex<HashMap<(String, String, String), String>>);

fn key(session: &ShopSession) -> (String, String, String) {
    (
        session.shopify_app.clone(),
        session.shop.clone(),
        session.actor.id.clone(),
    )
}

impl StaffLocales {
    fn remember(&self, session: &ShopSession, locale: &str) {
        let mut locales = crate::locked(&self.0);
        let key = key(session);
        if locales.len() >= KEPT && !locales.contains_key(&key) {
            locales.clear();
        }
        locales.insert(key, locale.to_string());
    }

    /// The locale the last request of the staff member of `session` gave.
    pub(crate) fn recall(&self, session: &ShopSession) -> Option<String> {
        crate::locked(&self.0).get(&key(session)).cloned()
    }
}

/// The locale of the staff member of `session` for this request: the header
/// of `openshop.js`, then `locale` in the query, which the server keeps,
/// else the one it kept. A value that is not a language tag is ignored.
pub(crate) fn staff_locale(
    app: &OpenShop,
    headers: &HeaderMap,
    query: &IndexMap<String, String>,
    session: &ShopSession,
) -> Option<String> {
    let given = header(headers, LOCALE_HEADER)
        .or(query.get("locale").map(String::as_str))
        .map(str::trim)
        .filter(|locale| is_language_tag(locale));
    match given {
        Some(locale) => {
            app.staff_locales().remember(session, locale);
            Some(locale.to_string())
        }
        None => app.staff_locales().recall(session),
    }
}
