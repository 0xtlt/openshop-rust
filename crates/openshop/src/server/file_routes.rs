//! Route path conventions shared by app proxies and server routes.
//!
//! Paths use `:name` for dynamic segments (`/orders/:id`); the file-style
//! `[name]` spelling is accepted too.

use std::cmp::Ordering;

use anyhow::{Result, bail};
use axum::Router;
use axum::routing::MethodRouter;

use crate::runtime::logger;

/// Normalizes `orders/[id]/` to `/orders/:id`.
pub fn normalize_route_path(path: &str) -> String {
    let segments: Vec<String> = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(
            |segment| match segment.strip_prefix('[').and_then(|rest| rest.strip_suffix(']')) {
                Some(name) => format!(":{name}"),
                None => segment.to_string(),
            },
        )
        .collect();
    format!("/{}", segments.join("/"))
}

fn segments(path: &str) -> Vec<&str> {
    path.split('/').filter(|segment| !segment.is_empty()).collect()
}

/// Static segments sort before dynamic ones, longer routes before shorter ones.
pub fn compare_route_specificity(left: &str, right: &str) -> Ordering {
    let left_segments = segments(left);
    let right_segments = segments(right);

    for (left_segment, right_segment) in left_segments.iter().zip(&right_segments) {
        let left_dynamic = left_segment.starts_with(':');
        let right_dynamic = right_segment.starts_with(':');
        if left_dynamic != right_dynamic {
            return if left_dynamic {
                Ordering::Greater
            } else {
                Ordering::Less
            };
        }
    }

    right_segments
        .len()
        .cmp(&left_segments.len())
        .then_with(|| left.cmp(right))
}

/// Converts `/orders/:id` to the router syntax `/orders/{id}`.
fn to_router_path(path: &str) -> String {
    let converted: Vec<String> = segments(path)
        .into_iter()
        .map(|segment| match segment.strip_prefix(':') {
            Some(name) => format!("{{{name}}}"),
            None => segment.replace('{', "{{").replace('}', "}}"),
        })
        .collect();
    format!("/{}", converted.join("/"))
}

fn shape_key(path: &str) -> String {
    segments(path)
        .into_iter()
        .map(|segment| if segment.starts_with(':') { ":" } else { segment })
        .collect::<Vec<_>>()
        .join("/")
}

/// Sorted, normalized route paths. Fails when two routes can match the same URLs.
pub fn resolve_route_paths<'a>(kind: &str, paths: impl IntoIterator<Item = &'a str>) -> Result<Vec<(String, &'a str)>> {
    let mut routes: Vec<(String, &'a str)> = paths
        .into_iter()
        .map(|path| (normalize_route_path(path), path))
        .collect();
    routes.sort_by(|left, right| compare_route_specificity(&left.0, &right.0));

    for (index, (path, _)) in routes.iter().enumerate() {
        for segment in segments(path) {
            let name = segment.strip_prefix(':');
            if name.is_some_and(|name| name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
            {
                bail!(
                    "[openshop] Invalid {kind} route \"{path}\": parameter names may only contain letters, numbers and \"_\""
                );
            }
        }
        if let Some((other, _)) = routes[..index]
            .iter()
            .find(|(other, _)| shape_key(other) == shape_key(path))
        {
            bail!("[openshop] {kind} routes \"{other}\" and \"{path}\" have the same route shape");
        }
    }
    Ok(routes)
}

/// Mounts routes under `prefix` (`/routes`, `/proxy`, `/ext`), the most
/// specific first. `route` gives the router of a route from its normalized
/// path and its key, with what its log line says of it. `title` is `Server`
/// or `Proxy`.
pub(crate) fn mount_routes<'a, S: Clone + Send + Sync + 'static>(
    title: &str,
    prefix: &str,
    paths: impl IntoIterator<Item = &'a str>,
    log: bool,
    mut route: impl FnMut(&str, &'a str) -> (MethodRouter<S>, String),
) -> Result<Router<S>> {
    let kind = title.to_lowercase();
    let routes = resolve_route_paths(&kind, paths)?;
    let mut router = Router::new();
    for (route_path, key) in &routes {
        let (method_router, detail) = route(route_path, key);
        let full_path = format!("{prefix}{}", to_router_path(route_path));
        router = router.route(full_path.trim_end_matches('/'), method_router.clone());
        if route_path == "/" {
            router = router.route(&format!("{prefix}/"), method_router);
        }
        if log {
            logger::info(format!("[openshop] {title} route: {route_path} ({detail})"));
        }
    }
    if log && !routes.is_empty() {
        logger::info(format!("[openshop] {} {kind} route(s) registered", routes.len()));
    }
    Ok(router)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_file_style_paths() {
        assert_eq!(normalize_route_path("orders/[id]"), "/orders/:id");
        assert_eq!(normalize_route_path("/orders/:id/"), "/orders/:id");
        assert_eq!(normalize_route_path("/"), "/");
        assert_eq!(normalize_route_path(""), "/");
    }

    #[test]
    fn prioritizes_static_routes_and_longer_paths() {
        let routes = resolve_route_paths("server", ["/", "/orders/[id]", "/orders/new"]).unwrap();
        let paths: Vec<&str> = routes.iter().map(|(path, _)| path.as_str()).collect();
        assert_eq!(paths, vec!["/orders/new", "/orders/:id", "/"]);
        assert_eq!(routes[1].1, "/orders/[id]");
    }

    #[test]
    fn converts_to_router_syntax() {
        assert_eq!(to_router_path("/orders/:id"), "/orders/{id}");
        assert_eq!(to_router_path("/"), "/");
        assert_eq!(to_router_path("/api/reviews"), "/api/reviews");
    }

    #[test]
    fn rejects_ambiguous_routes_and_bad_parameter_names() {
        let error = resolve_route_paths("proxy", ["/orders/:id", "/orders/:slug"]).unwrap_err();
        assert!(error.to_string().contains("have the same route shape"));
        let error = resolve_route_paths("proxy", ["/reviews", "reviews/"]).unwrap_err();
        assert!(error.to_string().contains("have the same route shape"));
        let error = resolve_route_paths("proxy", ["/orders/:bad-name"]).unwrap_err();
        assert!(error.to_string().contains("parameter names"));
    }
}
