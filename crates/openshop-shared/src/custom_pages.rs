//! Path rules for application-defined admin pages.

use serde::{Deserialize, Serialize};

/// The path of the home of the admin. A page of the application can take it.
pub const ADMIN_HOME_PATH: &str = "/";

pub const CUSTOM_ADMIN_RESERVED_PREFIXES: [&str; 14] = [
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

fn segments(path: &str) -> impl Iterator<Item = &str> {
    path.split('/').filter(|segment| !segment.is_empty())
}

fn is_plain_segment(segment: &str) -> bool {
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
    match segments(path).next() {
        Some(first) => CUSTOM_ADMIN_RESERVED_PREFIXES.contains(&first),
        None => true,
    }
}

pub fn is_safe_custom_admin_path(path: &str, allow_dynamic: bool) -> bool {
    if !path.starts_with('/') || path == "/" || path.ends_with('/') {
        return false;
    }
    if segments(path).next().is_none() || is_reserved_custom_admin_path(path) {
        return false;
    }
    segments(path).all(|segment| (allow_dynamic && is_dynamic_segment(segment)) || is_plain_segment(segment))
}

/// Whether a page of the application can be served at `pattern`: the home of
/// the admin, or a safe path that may have dynamic segments.
pub fn is_custom_admin_page_pattern(pattern: &str) -> bool {
    pattern == ADMIN_HOME_PATH || is_safe_custom_admin_path(pattern, true)
}

pub fn match_custom_admin_path(pattern: &str, pathname: &str) -> bool {
    let pattern_segments: Vec<&str> = segments(pattern).collect();
    let path_segments: Vec<&str> = segments(pathname).collect();
    pattern_segments.len() == path_segments.len()
        && pattern_segments
            .iter()
            .zip(&path_segments)
            .all(|(pattern, path)| pattern.starts_with(':') || pattern == path)
}

pub fn custom_admin_route_shape_key(pattern: &str) -> String {
    segments(pattern)
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
