//! What a request for a page of the admin goes through before it is
//! rendered: a launch Shopify signed checks that the shop has the app
//! installed, and forgets the plan kept for it when it is the welcome link
//! of a plan; a visitor outside Shopify admin is told where the app opens
//! from. A recent launch for a shop without a token first asks Shopify for one
//! with the client credentials grant, since the signed query vouches for the
//! shop: a development store of the organization that owns the app opens with
//! no OAuth redirect inside the admin iframe.

use std::borrow::Cow;

use axum::http::StatusCode;
use axum::http::header::{CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE};
use axum::response::{IntoResponse, Response};

use super::connectors::is_installed_shop;
use super::http::escape;
use super::jwt::is_staff_subject;
use super::shop::ensure_installation;
use super::shop_domain::normalize_shop_domain;
use super::shopify_apps::ResolvedShopifyApp;
use crate::app::OpenShop;
use crate::shopify::client_credentials::Grant;
use crate::shopify::endpoints::{Endpoints, SHOPIFY_POLARIS, ScriptUrl};
use shopify_api::oauth::{SessionToken, SignedQuery};

/// What a launch whose query verified vouches for: its shop, written as a
/// shop domain, for the app that signed it. `shopify_api` verifies only a
/// launch of the last 90 seconds: an older one, replayed, is not one.
fn launch_grant(query: &SignedQuery, shopify_app: &str, shop: &str) -> Option<Grant> {
    let grant = Grant::vouched(shopify_app, query.get("shop")?)?;
    matches!(&grant, Grant::Vouched { shop: vouched, .. } if vouched == shop).then_some(grant)
}

/// The session token Shopify managed installation opens the app with, as
/// `id_token`, when it is valid for the app that signed the launch, for its
/// shop, and for a staff member.
fn launch_session(shopify_app: &ResolvedShopifyApp, query: &SignedQuery, shop: &str) -> Option<SessionToken> {
    let session = shopify_app.oauth.verify_session_token(query.get("id_token")?).ok()?;
    let staff = is_staff_subject(session.user_id().unwrap_or_default());
    (staff && session.shop().as_str() == shop).then_some(session)
}

/// Whether the shop of a verified launch has the app installed, once given a
/// token when it had none: by the token exchange of the launch's session
/// token first, as the first `/api` request would, then by the client
/// credentials grant the launch vouches for. The shop renders without the
/// OAuth redirect, which Shopify does not display inside the admin frame.
pub(crate) async fn is_installed_for_launch(
    app: &OpenShop,
    shopify_app: &ResolvedShopifyApp,
    shop: &str,
    query: &SignedQuery,
) -> anyhow::Result<bool> {
    if is_installed_shop(app, &shopify_app.handle, shop, None).await? {
        return Ok(true);
    }
    if let Some(session) = launch_session(shopify_app, query, shop) {
        // Best-effort, as for an `/api` request: a failure keeps the fallbacks below.
        let _ = ensure_installation(app, shopify_app, shop, &session).await;
    }
    let grant = launch_grant(query, &shopify_app.handle, shop);
    is_installed_shop(app, &shopify_app.handle, shop, grant.as_ref()).await
}

/// A verified launch of the app with `plan_handle` (or `charge_id`) in its
/// query is the welcome link of a plan the merchant just chose: the plan
/// kept for the shop is forgotten, so that the next check reads the new one.
/// Not an answer that has that plan and is younger than ten seconds: the
/// route of the welcome link read it, then sent the launch here.
pub(crate) async fn forget_plan_on_welcome(app: &OpenShop, shopify_app: &str, query: &SignedQuery) {
    if query.get("plan_handle").is_none() && query.get("charge_id").is_none() {
        return;
    }
    if let Some(shop) = query.get("shop").and_then(normalize_shop_domain) {
        app.plans()
            .forget_on_welcome(shopify_app, &shop, query.get("plan_handle"))
            .await;
    }
}

const RESERVED_PREFIXES: [&str; 8] = [
    "/api",
    "/auth",
    "/webhooks",
    "/proxy",
    "/ext",
    "/routes",
    "/health",
    "/ops",
];

/// Whether a path is a page of the admin rather than a file, an API or a route of the server.
pub fn is_ui_shell_path(pathname: &str) -> bool {
    if RESERVED_PREFIXES
        .iter()
        .any(|prefix| pathname == *prefix || pathname.starts_with(&format!("{prefix}/")))
    {
        return false;
    }
    if pathname == "/" || pathname.ends_with('/') || pathname.ends_with(".html") {
        return true;
    }
    !pathname.rsplit('/').next().unwrap_or_default().contains('.')
}

/// Keeps the page readable while Polaris is not loaded, or when Shopify's CDN
/// cannot be reached.
const POLARIS_FALLBACK_STYLE: &str = "
body { margin: 0; font-family: system-ui, -apple-system, 'Segoe UI', sans-serif; }
s-page:not(:defined), s-section:not(:defined), s-stack:not(:defined), s-heading:not(:defined),
s-paragraph:not(:defined) { display: block; margin: 8px 16px; }
s-heading:not(:defined) { font-weight: 600; }
";

/// The Polaris script, what it loads from Shopify's CDN, each under its own
/// path (the styles and the files of its font, its icons), no form, and
/// nothing else. The page can be framed: a shop whose launch is refused reads
/// it inside Shopify admin.
const WELCOME_POLICY: &str = "default-src 'none'; \
    script-src https://cdn.shopify.com/shopifycloud/polaris.js; \
    style-src 'unsafe-inline' https://cdn.shopify.com/static/fonts/; \
    font-src https://cdn.shopify.com/static/fonts/; \
    connect-src https://cdn.shopify.com/shopifycloud/; \
    form-action 'none'; base-uri 'none'";

/// [`WELCOME_POLICY`] for a Polaris that is not Shopify's: its origin in
/// `script-src`, and in the directives of what it loads, its styles, its font
/// and its icons, which a copy of Polaris serves from where it is. Shopify's
/// CDN stays: a stand-in may redirect there.
fn welcome_policy(polaris: Option<&ScriptUrl>) -> Cow<'static, str> {
    let Some(polaris) = polaris else {
        return Cow::Borrowed(WELCOME_POLICY);
    };
    let directives: Vec<String> = WELCOME_POLICY
        .split("; ")
        .map(|directive| match directive.split_once(' ') {
            Some(("script-src" | "style-src" | "font-src" | "connect-src", _)) => {
                format!("{directive} {}", polaris.origin())
            }
            _ => directive.to_string(),
        })
        .collect();
    Cow::Owned(directives.join("; "))
}

/// What a visitor is told when the admin is not opened for a shop: where the
/// app opens from. It shows nothing of the application. Its Polaris is the
/// one of the Shopify endpoints.
pub(crate) fn open_from_shopify_admin(status: StatusCode, endpoints: &Endpoints) -> Response {
    let polaris = match &endpoints.polaris {
        Some(polaris) => Cow::Owned(escape(polaris.as_str())),
        None => Cow::Borrowed(SHOPIFY_POLARIS),
    };
    let html = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <meta name=\"robots\" content=\"noindex,nofollow\">\
         <title>Open this app from Shopify admin</title><style>{}</style><script src=\"{}\"></script>\
         </head><body><s-page inlineSize=\"small\"><s-section><s-stack gap=\"base\">\
         <s-heading>Open this app from Shopify admin</s-heading>\
         <s-paragraph>This app works inside Shopify admin. Open it from the Apps section of your store.</s-paragraph>\
         </s-stack></s-section></s-page></body></html>",
        POLARIS_FALLBACK_STYLE, polaris,
    );
    let policy = welcome_policy(endpoints.polaris.as_ref());
    (
        status,
        [
            (CONTENT_TYPE.as_str(), "text/html; charset=utf-8"),
            (CACHE_CONTROL.as_str(), "no-store"),
            (CONTENT_SECURITY_POLICY.as_str(), policy.as_ref()),
        ],
        html,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shopify::ShopifyEndpoints;

    const APP_BRIDGE: &str = "http://localhost:9393/admin/app-bridge.js";
    const POLARIS: &str = "http://localhost:9393/cdn/polaris.js";

    fn endpoints(endpoints: ShopifyEndpoints) -> Endpoints {
        Endpoints::resolve(&endpoints, &ShopifyEndpoints::new()).unwrap()
    }

    fn simulator() -> Endpoints {
        endpoints(
            ShopifyEndpoints::new()
                .admin_origin("http://localhost:9393")
                .app_bridge(APP_BRIDGE)
                .polaris(POLARIS),
        )
    }

    #[test]
    fn recognizes_the_paths_of_pages() {
        for path in [
            "/",
            "/flows",
            "/flows/",
            "/runs/123",
            "/index.html",
            "/reviews/42",
            "/apis",
        ] {
            assert!(is_ui_shell_path(path), "{path}");
        }
        for path in [
            "/api",
            "/api/flows",
            "/auth/callback",
            "/webhooks",
            "/proxy/reviews",
            "/ext/reviews",
            "/routes/ping",
            "/health",
            "/assets/app.js",
            "/favicon.ico",
        ] {
            assert!(!is_ui_shell_path(path), "{path}");
        }
    }

    #[test]
    fn the_welcome_page_allows_its_polaris_where_polaris_needs_it() {
        assert_eq!(welcome_policy(None), WELCOME_POLICY);
        // The App Bridge and the admin origin change nothing of it: the page loads neither.
        let no_polaris = endpoints(
            ShopifyEndpoints::new()
                .admin_origin("http://localhost:9393")
                .app_bridge(APP_BRIDGE),
        );
        assert_eq!(welcome_policy(no_polaris.polaris.as_ref()), WELCOME_POLICY);

        let simulator = simulator();
        assert_eq!(
            welcome_policy(simulator.polaris.as_ref()),
            "default-src 'none'; script-src https://cdn.shopify.com/shopifycloud/polaris.js http://localhost:9393; \
             style-src 'unsafe-inline' https://cdn.shopify.com/static/fonts/ http://localhost:9393; \
             font-src https://cdn.shopify.com/static/fonts/ http://localhost:9393; \
             connect-src https://cdn.shopify.com/shopifycloud/ http://localhost:9393; \
             form-action 'none'; base-uri 'none'"
        );
    }
}
