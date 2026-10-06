//! Allow-list for client-side navigation triggered by App Bridge's `shopify:navigate` event.

use crate::url;

const NAVIGATION_SELECTOR: &str = "s-link[href], a[href], s-button[href]";
const RESERVED_PREFIXES: [&str; 7] = ["/api", "/auth", "/webhooks", "/proxy", "/ext", "/routes", "/health"];

/// An entry of an event's composed path.
pub trait NavigateTarget {
    fn matches(&self, selector: &str) -> bool;
    fn attribute(&self, name: &str) -> Option<String>;
}

/// The parts of a `shopify:navigate` event the bridge relies on.
pub trait NavigateEvent {
    type Target: NavigateTarget;

    fn composed_path(&self) -> Vec<Self::Target>;
    fn prevent_default(&self);
}

fn is_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => *byte == b'-',
            14 => matches!(byte, b'1'..=b'5'),
            19 => matches!(byte.to_ascii_lowercase(), b'8' | b'9' | b'a' | b'b'),
            _ => byte.is_ascii_hexdigit(),
        })
}

fn is_safe_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

fn is_reserved_path(pathname: &str) -> bool {
    RESERVED_PREFIXES.iter().any(|prefix| {
        pathname
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    })
}

fn segments(path: &str) -> Vec<&str> {
    path.split('/').filter(|segment| !segment.is_empty()).collect()
}

fn matches_custom_path<P: AsRef<str>>(path_segments: &[&str], custom_patterns: &[P]) -> bool {
    custom_patterns.iter().any(|pattern| {
        let pattern_segments = segments(pattern.as_ref());
        pattern_segments.len() == path_segments.len()
            && pattern_segments.iter().zip(path_segments).all(|(pattern, segment)| {
                if pattern.starts_with(':') {
                    is_safe_segment(segment)
                } else {
                    pattern == segment
                }
            })
    })
}

fn is_allowed_ui_path<P: AsRef<str>>(pathname: &str, custom_patterns: &[P]) -> bool {
    if pathname == "/" {
        return true;
    }
    if is_reserved_path(pathname) {
        return false;
    }

    let segments = segments(pathname);
    match segments.as_slice() {
        ["flows"] | ["crons"] | ["providers"] | ["mcp"] | ["functions"] => true,
        ["flows", name] => is_safe_segment(name),
        ["runs", id] => is_uuid(id),
        ["functions", handle] | ["functions", handle, _] => is_safe_segment(handle),
        ["flows" | "runs" | "crons" | "providers" | "mcp" | "functions", ..] => false,
        [_] | [_, _] | [_, _, _] => matches_custom_path(&segments, custom_patterns),
        _ => false,
    }
}

/// Maps an `href` to a same-origin UI route (`path?search#hash`), or `None` when the browser
/// must handle it.
pub fn href_to_internal_route<P: AsRef<str>>(href: &str, origin: &str, custom_patterns: &[P]) -> Option<String> {
    let url = url::resolve(href, origin)?;
    if url.origin != origin || !is_allowed_ui_path(&url.pathname, custom_patterns) {
        return None;
    }
    Some(format!("{}{}{}", url.pathname, url.search, url.hash))
}

/// The `href` of the first navigation element in the event's composed path.
pub fn find_shopify_navigate_href<E: NavigateEvent>(event: &E) -> Option<String> {
    event
        .composed_path()
        .into_iter()
        .find(|target| target.matches(NAVIGATION_SELECTOR))
        .and_then(|target| target.attribute("href"))
}

/// Routes internal links client-side and leaves everything else to native navigation.
pub fn handle_shopify_navigate<E: NavigateEvent, P: AsRef<str>>(
    event: &E,
    route: impl FnOnce(&str),
    origin: &str,
    custom_patterns: &[P],
) {
    let Some(href) = find_shopify_navigate_href(event).filter(|href| !href.is_empty()) else {
        return;
    };
    let Some(internal_route) = href_to_internal_route(&href, origin, custom_patterns) else {
        return;
    };
    event.prevent_default();
    route(&internal_route);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    const ORIGIN: &str = "https://example.test";
    const VALID_RUN_ID: &str = "550e8400-e29b-41d4-a716-446655440000";
    const NO_PATTERNS: [&str; 0] = [];

    fn route(href: &str) -> Option<String> {
        href_to_internal_route(href, ORIGIN, &NO_PATTERNS)
    }

    #[derive(Clone)]
    struct FakeTarget {
        tag: &'static str,
        href: Option<String>,
    }

    fn nav_element(tag: &'static str, href: &str) -> FakeTarget {
        FakeTarget {
            tag,
            href: Some(href.to_string()),
        }
    }

    fn plain_object() -> FakeTarget {
        FakeTarget { tag: "", href: None }
    }

    impl NavigateTarget for FakeTarget {
        fn matches(&self, selector: &str) -> bool {
            self.href.is_some()
                && selector
                    .split(',')
                    .any(|item| item.trim() == format!("{}[href]", self.tag))
        }

        fn attribute(&self, name: &str) -> Option<String> {
            self.href.clone().filter(|_| name == "href")
        }
    }

    struct FakeNavigateEvent {
        path: Vec<FakeTarget>,
        default_prevented: Cell<bool>,
    }

    impl FakeNavigateEvent {
        fn new(path: Vec<FakeTarget>) -> Self {
            Self {
                path,
                default_prevented: Cell::new(false),
            }
        }
    }

    impl NavigateEvent for FakeNavigateEvent {
        type Target = FakeTarget;

        fn composed_path(&self) -> Vec<FakeTarget> {
            self.path.clone()
        }

        fn prevent_default(&self) {
            self.default_prevented.set(true);
        }
    }

    fn dispatch(event: &FakeNavigateEvent) -> Vec<String> {
        let routed = RefCell::new(Vec::new());
        handle_shopify_navigate(
            event,
            |path| routed.borrow_mut().push(path.to_string()),
            ORIGIN,
            &NO_PATTERNS,
        );
        routed.into_inner()
    }

    #[test]
    fn accepts_allowed_app_routes() {
        assert_eq!(route("/").as_deref(), Some("/"));
        assert_eq!(route("/flows").as_deref(), Some("/flows"));
        assert_eq!(
            route("/flows/syncPrescriptions?q=failed#latest").as_deref(),
            Some("/flows/syncPrescriptions?q=failed#latest")
        );
        assert_eq!(
            route(&format!("/runs/{VALID_RUN_ID}")),
            Some(format!("/runs/{VALID_RUN_ID}"))
        );
        assert_eq!(route("/mcp").as_deref(), Some("/mcp"));
        assert_eq!(
            route("/functions/discounts/new").as_deref(),
            Some("/functions/discounts/new")
        );
        assert_eq!(
            route("/functions/discounts/gid%3A%2F%2Fshopify%2FDiscountNode%2F1").as_deref(),
            Some("/functions/discounts/gid%3A%2F%2Fshopify%2FDiscountNode%2F1")
        );
    }

    #[test]
    fn accepts_only_registered_custom_page_patterns() {
        let patterns = ["/reviews/:id"];
        assert_eq!(
            href_to_internal_route("/reviews/123", ORIGIN, &patterns).as_deref(),
            Some("/reviews/123")
        );
        assert_eq!(href_to_internal_route("/reviews/123/edit", ORIGIN, &patterns), None);
        assert_eq!(href_to_internal_route("/unknown", ORIGIN, &patterns), None);
    }

    #[test]
    fn rejects_external_and_special_scheme_urls() {
        for href in [
            "https://evil.test/runs/550e8400-e29b-41d4-a716-446655440000",
            "//evil.test/runs/550e8400-e29b-41d4-a716-446655440000",
            "javascript:alert(1)",
            "data:text/html,x",
            "https://admin.shopify.com/store/x",
            "app:bar",
            "shopify:admin/api/graphql.json",
            "mailto:test@example.test",
            "tel:+33123456789",
        ] {
            assert_eq!(route(href), None, "{href}");
        }
    }

    #[test]
    fn rejects_server_reserved_and_unknown_paths() {
        for href in [
            "/api/runs",
            "/auth?shop=x",
            "/webhooks/orders/create",
            "/proxy/dashboard",
            "/ext/dashboard",
            "/routes/callback",
            "/health",
            "/admin",
            "/%2F%2Fevil.test",
        ] {
            assert_eq!(route(href), None, "{href}");
        }
    }

    #[test]
    fn rejects_traversal_and_invalid_route_params() {
        for href in [
            "/flows/../auth",
            "/runs/not-a-uuid",
            "/flows/sync/orders",
            "/functions/discounts/new/extra",
        ] {
            assert_eq!(route(href), None, "{href}");
        }
    }

    #[test]
    fn finds_href_from_a_navigation_element_in_the_composed_path() {
        let event = FakeNavigateEvent::new(vec![
            plain_object(),
            nav_element("s-link", &format!("/runs/{VALID_RUN_ID}")),
        ]);
        assert_eq!(
            find_shopify_navigate_href(&event),
            Some(format!("/runs/{VALID_RUN_ID}"))
        );
    }

    #[test]
    fn routes_internal_links_and_prevents_default_navigation() {
        let event = FakeNavigateEvent::new(vec![
            plain_object(),
            nav_element("s-link", &format!("/runs/{VALID_RUN_ID}")),
        ]);
        assert_eq!(dispatch(&event), vec![format!("/runs/{VALID_RUN_ID}")]);
        assert!(event.default_prevented.get());
    }

    #[test]
    fn ignores_non_navigation_elements_with_href_like_attributes() {
        let event = FakeNavigateEvent::new(vec![nav_element("div", &format!("/runs/{VALID_RUN_ID}"))]);
        assert!(dispatch(&event).is_empty());
        assert!(!event.default_prevented.get());
    }

    #[test]
    fn leaves_external_links_to_native_navigation() {
        let event = FakeNavigateEvent::new(vec![nav_element("s-link", "https://admin.shopify.com/store/x")]);
        assert!(dispatch(&event).is_empty());
        assert!(!event.default_prevented.get());
    }

    #[test]
    fn validates_run_ids_as_uuids() {
        assert!(is_uuid(VALID_RUN_ID));
        assert!(is_uuid("550E8400-E29B-41D4-A716-446655440000"));
        assert!(!is_uuid("550e8400-e29b-61d4-a716-446655440000"));
        assert!(!is_uuid("550e8400-e29b-41d4-c716-446655440000"));
        assert!(!is_uuid("550e8400e29b41d4a716446655440000"));
    }
}
