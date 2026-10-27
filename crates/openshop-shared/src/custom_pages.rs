//! Path rules for application-defined admin pages.

use std::cmp::Ordering;
use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::rpc::decode_uri_component;

/// The path of the home of the admin. A page of the application can take it.
pub const ADMIN_HOME_PATH: &str = "/";

const CUSTOM_ADMIN_RESERVED_PREFIXES: [&str; 14] = [
    "api",
    "auth",
    "webhooks",
    "proxy",
    "ext",
    "routes",
    "health",
    "ops",
    "flows",
    "runs",
    "providers",
    "crons",
    "functions",
    "mcp",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminMenuItem {
    pub label: String,
    pub path: String,
}

impl AdminMenuItem {
    pub fn new(label: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            path: path.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomAdminPageAccess {
    pub id: String,
    pub path: String,
    pub allowed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CustomAdminPagesResponse {
    pub navigation: Vec<AdminMenuItem>,
    pub pages: Vec<CustomAdminPageAccess>,
}

/// The segments of a path, empty ones left out: `/reviews//42/` has two.
pub fn path_segments(path: &str) -> impl Iterator<Item = &str> {
    path.split('/').filter(|segment| !segment.is_empty())
}

/// A segment made of ASCII letters, digits, `_` and `-` only.
pub fn is_safe_path_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

fn is_dynamic_segment(segment: &str) -> bool {
    let Some(name) = segment.strip_prefix(':') else {
        return false;
    };
    let mut bytes = name.bytes();
    matches!(bytes.next(), Some(first) if first.is_ascii_alphabetic())
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

pub fn is_reserved_custom_admin_path(path: &str) -> bool {
    match path_segments(path).next() {
        Some(first) => CUSTOM_ADMIN_RESERVED_PREFIXES.contains(&first),
        None => true,
    }
}

pub fn is_safe_custom_admin_path(path: &str, allow_dynamic: bool) -> bool {
    if !path.starts_with('/') || path == "/" || path.ends_with('/') {
        return false;
    }
    if path_segments(path).next().is_none() || is_reserved_custom_admin_path(path) {
        return false;
    }
    path_segments(path).all(|segment| (allow_dynamic && is_dynamic_segment(segment)) || is_safe_path_segment(segment))
}

/// Whether a page of the application can be served at `pattern`: the home of
/// the admin, or a safe path that may have dynamic segments.
pub fn is_custom_admin_page_pattern(pattern: &str) -> bool {
    pattern == ADMIN_HOME_PATH || is_safe_custom_admin_path(pattern, true)
}

pub fn match_custom_admin_path(pattern: &str, pathname: &str) -> bool {
    let pattern_segments: Vec<&str> = path_segments(pattern).collect();
    let pathname_segments: Vec<&str> = path_segments(pathname).collect();
    pattern_segments.len() == pathname_segments.len()
        && pattern_segments
            .iter()
            .zip(&pathname_segments)
            .all(|(pattern, path)| pattern.starts_with(':') || pattern == path)
}

/// The percent-decoded values of the dynamic segments of `pathname`, by name:
/// `id` for `/reviews/:id`. `None` when it does not match `pattern`, or when a
/// value is not a valid escape.
pub fn custom_admin_route_params<C: FromIterator<(String, String)>>(pattern: &str, pathname: &str) -> Option<C> {
    if !match_custom_admin_path(pattern, pathname) {
        return None;
    }
    path_segments(pattern)
        .zip(path_segments(pathname))
        .filter_map(|(pattern, path)| Some((pattern.strip_prefix(':')?, path)))
        .map(|(name, value)| Some((name.to_string(), decode_uri_component(value)?)))
        .collect()
}

fn custom_admin_route_shape_key(pattern: &str) -> String {
    path_segments(pattern)
        .map(|segment| {
            if segment.starts_with(':') {
                ":".to_string()
            } else {
                format!("s:{segment}")
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Static segments sort before dynamic ones, so `/reviews/new` wins over `/reviews/:id`.
fn compare_patterns(left: &str, right: &str) -> Ordering {
    let mut left = left.split('/');
    let mut right = right.split('/');
    loop {
        match (left.next(), right.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(left), Some(right)) if left == right => {}
            (Some(left), Some(right)) => {
                return match (left.starts_with(':'), right.starts_with(':')) {
                    (true, false) => Ordering::Greater,
                    (false, true) => Ordering::Less,
                    _ => left.cmp(right),
                };
            }
        }
    }
}

/// Sorts routes from the most specific, `pattern` giving the route pattern of
/// one. Fails on two routes that could match the same URL.
pub fn sort_custom_admin_routes<T>(routes: &mut [T], pattern: impl Fn(&T) -> &str) -> Result<(), String> {
    routes.sort_by(|left, right| compare_patterns(pattern(left), pattern(right)));
    let mut routes_by_shape: HashMap<String, &str> = HashMap::with_capacity(routes.len());
    for route in routes.iter() {
        let route = pattern(route);
        if let Some(existing) = routes_by_shape.insert(custom_admin_route_shape_key(route), route) {
            return Err(format!(
                "[openshop] Custom admin page routes \"{existing}\" and \"{route}\" have the same route shape"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserves_framework_prefixes() {
        for prefix in CUSTOM_ADMIN_RESERVED_PREFIXES {
            assert!(is_reserved_custom_admin_path(&format!("/{prefix}")));
            assert!(is_reserved_custom_admin_path(&format!("/{prefix}/child")));
        }
        assert!(is_reserved_custom_admin_path("/"));
        assert!(!is_reserved_custom_admin_path("/reviews"));
    }

    #[test]
    fn accepts_only_safe_static_paths_by_default() {
        assert!(is_safe_custom_admin_path("/reviews", false));
        assert!(is_safe_custom_admin_path("/reviews/pending_items-2", false));
        assert!(!is_safe_custom_admin_path("reviews", false));
        assert!(!is_safe_custom_admin_path("/", false));
        assert!(!is_safe_custom_admin_path("/reviews/", false));
        assert!(!is_safe_custom_admin_path("/flows/custom", false));
        assert!(!is_safe_custom_admin_path("/reviews/:id", false));
        assert!(!is_safe_custom_admin_path("/reviews/a b", false));
        assert!(!is_safe_custom_admin_path("/reviews/../x", false));
    }

    #[test]
    fn accepts_dynamic_segments_when_allowed() {
        assert!(is_safe_custom_admin_path("/reviews/:id", true));
        assert!(is_safe_custom_admin_path("/reviews/:review_id/edit", true));
        assert!(!is_safe_custom_admin_path("/reviews/:1id", true));
        assert!(!is_safe_custom_admin_path("/reviews/:", true));
        assert!(!is_safe_custom_admin_path("/reviews/:id-x", true));
    }

    #[test]
    fn a_page_can_take_the_home_but_no_reserved_path() {
        assert!(is_custom_admin_page_pattern("/"));
        assert!(is_custom_admin_page_pattern("/reviews/:id"));
        assert!(!is_custom_admin_page_pattern("/ops"));
        assert!(!is_custom_admin_page_pattern("/flows"));
        assert!(!is_custom_admin_page_pattern(""));
    }

    #[test]
    fn matches_patterns_segment_by_segment() {
        assert!(match_custom_admin_path("/", "/"));
        assert!(!match_custom_admin_path("/", "/reviews"));
        assert!(match_custom_admin_path("/reviews", "/reviews"));
        assert!(match_custom_admin_path("/reviews/:id", "/reviews/42"));
        assert!(match_custom_admin_path("/reviews/:id", "/reviews/42/"));
        assert!(!match_custom_admin_path("/reviews/:id", "/reviews"));
        assert!(!match_custom_admin_path("/reviews/:id", "/reviews/42/edit"));
        assert!(!match_custom_admin_path("/reviews", "/orders"));
    }

    #[test]
    fn extracts_decoded_route_params() {
        let params: Vec<(String, String)> =
            custom_admin_route_params("/reviews/:id/items/:item", "/reviews/gid%3A%2F%2Fx%2F1/items/a%20b/").unwrap();
        assert_eq!(
            params,
            [("id".into(), "gid://x/1".into()), ("item".into(), "a b".into())]
        );
        assert_eq!(
            custom_admin_route_params::<Vec<_>>("/reviews", "/reviews"),
            Some(Vec::new())
        );
        assert_eq!(custom_admin_route_params::<Vec<_>>("/reviews/:id", "/reviews"), None);
        assert_eq!(custom_admin_route_params::<Vec<_>>("/reviews/:id", "/orders/1"), None);
        assert_eq!(
            custom_admin_route_params::<Vec<_>>("/reviews/:id", "/reviews/%zz"),
            None
        );
    }

    fn sorted(patterns: &[&'static str]) -> Result<Vec<&'static str>, String> {
        let mut patterns = patterns.to_vec();
        sort_custom_admin_routes(&mut patterns, |pattern| pattern)?;
        Ok(patterns)
    }

    #[test]
    fn sorts_static_routes_before_dynamic_ones() {
        assert_eq!(
            sorted(&["/reviews/:id", "/reviews/new", "/reviews", "/analytics", "/"]).unwrap(),
            ["/", "/analytics", "/reviews", "/reviews/new", "/reviews/:id"]
        );
    }

    #[test]
    fn refuses_routes_with_the_same_shape() {
        let error = sorted(&["/reviews/:id", "/reviews/:id/edit", "/reviews/:slug"]).unwrap_err();
        assert_eq!(
            error,
            "[openshop] Custom admin page routes \"/reviews/:id\" and \"/reviews/:slug\" have the same route shape"
        );
        assert!(sorted(&["/", "/"]).is_err());
    }

    #[test]
    fn builds_route_shape_keys() {
        assert_eq!(custom_admin_route_shape_key("/reviews/:id"), "s:reviews/:");
        assert_eq!(
            custom_admin_route_shape_key("/reviews/:id"),
            custom_admin_route_shape_key("/reviews/:slug")
        );
        assert_ne!(
            custom_admin_route_shape_key("/reviews/:id"),
            custom_admin_route_shape_key("/reviews/new")
        );
    }
}
