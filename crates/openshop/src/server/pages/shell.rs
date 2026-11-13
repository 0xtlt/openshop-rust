//! The document around every page: App Bridge, Polaris, `openshop.js`, the
//! menu of the admin, and `#openshop-page`, which the script swaps.

use std::sync::LazyLock;

use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, ETAG, IF_NONE_MATCH, X_CONTENT_TYPE_OPTIONS};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use maud::{DOCTYPE, Markup, PreEscaped, html};
use sha2::{Digest, Sha256};

use super::super::admin_pages::page_is_allowed;
use super::super::api::pages::resolved_admin_pages;
use super::super::reload::Served;
use super::super::shop::ShopSession;
use super::super::shopify_apps::DEFAULT_APP_NAME;
use crate::app::OpenShop;
use crate::shopify::endpoints::{Endpoints, SHOPIFY_APP_BRIDGE, SHOPIFY_POLARIS, ScriptUrl};
use crate::types::{AdminMenuItem, AdminPageMode};

/// The script of the pages, compiled into the binary.
const SCRIPT: &str = include_str!("openshop.js");
/// Where it is served, under the paths of the framework.
pub(crate) const SCRIPT_PATH: &str = "/__openshop/openshop.js";

/// The start of the hash of the script: its URL changes with it, so that it
/// can be cached for good.
static SCRIPT_VERSION: LazyLock<String> = LazyLock::new(|| hex::encode(&Sha256::digest(SCRIPT.as_bytes())[..8]));

/// `GET /__openshop/openshop.js`: cached for a year under the URL of its
/// version, revalidated otherwise.
pub(crate) async fn script(headers: HeaderMap) -> Response {
    let etag = format!("\"{}\"", *SCRIPT_VERSION);
    if headers.get(IF_NONE_MATCH).and_then(|value| value.to_str().ok()) == Some(etag.as_str()) {
        return StatusCode::NOT_MODIFIED.into_response();
    }
    (
        [
            (CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (CACHE_CONTROL, "public, max-age=31536000, immutable"),
            (ETAG, etag.as_str()),
            (X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        SCRIPT,
    )
        .into_response()
}

fn app_bridge(endpoints: &Endpoints) -> &str {
    endpoints
        .app_bridge
        .as_ref()
        .map_or(SHOPIFY_APP_BRIDGE, ScriptUrl::as_str)
}

fn polaris(endpoints: &Endpoints) -> &str {
    endpoints.polaris.as_ref().map_or(SHOPIFY_POLARIS, ScriptUrl::as_str)
}

/// The menu of the admin for a staff member: the built-in pages that are
/// visible, then the entries of the application whose page lets them in.
pub(crate) struct Menu {
    items: Vec<AdminMenuItem>,
}

impl Menu {
    pub(crate) async fn of(app: &OpenShop, session: &ShopSession) -> Self {
        let config = app.config();
        let pages = resolved_admin_pages(config);
        let mut items = Vec::new();
        for (mode, label, path) in [
            (pages.flows, "Flows", "/flows"),
            (pages.providers, "Providers", "/providers"),
            (pages.mcp, "MCP", "/mcp"),
            (pages.crons, "Crons", "/crons"),
            (pages.functions, "Functions", "/functions"),
        ] {
            if mode == AdminPageMode::Visible {
                items.push(AdminMenuItem::new(label, path));
            }
        }
        if config.custom_pages_enabled() {
            for item in config.resolved_admin_menu() {
                let page = app
                    .custom_pages()
                    .iter()
                    .find(|page| crate::admin_paths::match_custom_admin_path(&page.route_pattern, &item.path));
                if let Some(page) = page
                    && page_is_allowed(app, session, page, &item.path).await
                {
                    items.push(item);
                }
            }
        }
        Self { items }
    }
}

/// The page: the document the browser loads, whose `#openshop-page` the
/// script also takes from it. `notice` is a failure shown above the page.
pub(crate) fn document(
    app: &OpenShop,
    session: &ShopSession,
    menu: &Menu,
    notice: Option<&str>,
    page: Markup,
    reload: Option<&Served>,
) -> Markup {
    let shopify_app = app.apps().by_handle(Some(&session.shopify_app)).ok();
    let api_key = shopify_app.map_or("", |shopify_app| shopify_app.api_key.as_str());
    let name = shopify_app.map_or(DEFAULT_APP_NAME, |shopify_app| shopify_app.name.as_str());
    let endpoints = app.endpoints();
    let document = html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                meta name="robots" content="noindex,nofollow,noarchive,nosnippet";
                title { (name) }
                meta name="shopify-api-key" content=(api_key);
                // App Bridge first, without `async` nor `defer`, as Shopify requires.
                script data-api-key=(api_key) src=(app_bridge(endpoints)) {}
                script src=(polaris(endpoints)) {}
                script src=(format!("{SCRIPT_PATH}?v={}", *SCRIPT_VERSION)) defer {}
            }
            body {
                ui-nav-menu {
                    a href="/" rel="home" { "Home" }
                    @for item in &menu.items {
                        a href=(item.path) { (item.label) }
                    }
                }
                main id="openshop-page" {
                    @if let Some(notice) = notice {
                        s-banner tone="critical" { (notice) }
                    }
                    (page)
                }
            }
        }
    };
    match reload {
        // Development: the page reloads when the server starts again.
        Some(served) => PreEscaped(served.inject(&document.into_string())),
        None => document,
    }
}

/// The bounce page: App Bridge alone, which loads `shopify-reload` again
/// with a fresh session token.
pub(crate) fn bounce(api_key: &str, endpoints: &Endpoints) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="shopify-api-key" content=(api_key);
                script data-api-key=(api_key) src=(app_bridge(endpoints)) {}
            }
            body {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SCRIPT;

    /// A modal removed while open stays open for the admin around the app,
    /// which can no longer be clicked (#77): the script hides the modals of
    /// the page before it replaces it, and before it leaves for an answer
    /// that is not a page. The repository has no JavaScript runner: this
    /// reads the script.
    #[test]
    fn the_script_hides_the_modals_before_it_replaces_the_page() {
        let hide = SCRIPT
            .split("function hideModals() {")
            .nth(1)
            .and_then(|rest| rest.split("\n  }\n").next())
            .expect("hideModals in openshop.js");
        assert!(hide.contains("querySelectorAll('s-modal')"), "{hide}");
        assert!(hide.contains("modal.hideOverlay()"), "{hide}");

        let present = SCRIPT
            .split("function present(")
            .nth(1)
            .and_then(|rest| rest.split("\n  }\n").next())
            .expect("present in openshop.js");
        let hidden = present.find("hideModals()").expect("present hides the modals");
        for swap in ["location.assign(", ".replaceChildren(", "ready()"] {
            let at = present.find(swap).unwrap_or_else(|| panic!("{swap} in present"));
            assert!(hidden < at, "the modals are hidden before {swap}: {present}");
        }
        // Every page that comes back goes through `present`.
        assert_eq!(SCRIPT.matches(".replaceChildren(").count(), 1, "{SCRIPT}");
    }
}
