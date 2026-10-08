//! Public server routes under `/routes`, each with an explicit authentication contract.

use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{RawPathParams, State};
use axum::http::header::ALLOW;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use futures::FutureExt;
use indexmap::IndexMap;
use serde_json::{Value, json};

use super::file_routes::{resolve_route_paths, to_router_path};
use super::http::json_error;
use crate::app::OpenShop;
use crate::declare::InvalidInput;
use crate::engine::step::error_message;
use crate::runtime::logger;
use crate::types::{Route, RouteAuth, RouteAuthContext, RouteAuthOutcome, RouteContext, RouteMethod, RouteRequest};

pub use super::connectors::resolve_shop_context;

fn internal_error() -> Response {
    json_error(StatusCode::INTERNAL_SERVER_ERROR, "Internal server route error")
}

struct RegisteredRoute {
    path: String,
    definition: Route,
}

async fn handle(
    registered: Arc<RegisteredRoute>,
    app: OpenShop,
    method: Method,
    uri: Uri,
    params: RawPathParams,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let RegisteredRoute {
        path: route_path,
        definition: route,
    } = &*registered;
    let method_name = method.as_str().to_uppercase();
    let Some(RouteMethod {
        auth: authentication,
        handler,
    }) = route.handlers.get(method_name.as_str()).cloned()
    else {
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            [(ALLOW, route.allowed_methods().join(", "))],
            axum::Json(json!({ "error": "Method not allowed" })),
        )
            .into_response();
    };

    let params: IndexMap<String, String> = params
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();
    let request = RouteRequest {
        method,
        uri,
        headers,
        body,
    };

    let mut auth = Value::Null;
    if let RouteAuth::Custom(authenticate) = &authentication {
        let context = RouteAuthContext {
            request: request.clone(),
            params: params.clone(),
            db: app.db().clone(),
            app: app.clone(),
        };
        match AssertUnwindSafe(authenticate(context)).catch_unwind().await {
            Ok(Ok(RouteAuthOutcome::Authenticated(value))) => auth = value,
            Ok(Ok(RouteAuthOutcome::Unauthorized)) => return json_error(StatusCode::UNAUTHORIZED, "Unauthorized"),
            Ok(Ok(RouteAuthOutcome::Response(response))) => return response,
            outcome => {
                let error = match outcome {
                    Ok(Err(error)) => error_message(&error),
                    _ => "authentication panicked".to_string(),
                };
                logger::error_with(
                    format!("[openshop] Server route authentication error for {method_name} {route_path}"),
                    json!({ "error": error }),
                );
                return internal_error();
            }
        }
    }

    let context = RouteContext {
        request,
        params,
        auth,
        db: app.db().clone(),
        app,
    };
    match AssertUnwindSafe(handler(context)).catch_unwind().await {
        Ok(Ok(response)) => response,
        // The handler refused its input.
        Ok(Err(error)) if error.is::<InvalidInput>() => json_error(StatusCode::UNPROCESSABLE_ENTITY, error.to_string()),
        outcome => {
            let error = match outcome {
                Ok(Err(error)) => error_message(&error),
                _ => "handler panicked".to_string(),
            };
            logger::error_with(
                format!("[openshop] Server route error for {method_name} {route_path}"),
                json!({ "error": error }),
            );
            internal_error()
        }
    }
}

/// Fails when a route declares no HTTP method.
pub fn assert_route_definition(route: &Route, route_path: &str) -> anyhow::Result<()> {
    if route.handlers.is_empty() {
        anyhow::bail!("[openshop] Invalid server route {route_path}: define at least one HTTP method");
    }
    Ok(())
}

pub fn create_server_routes(app: &OpenShop, log: bool) -> anyhow::Result<Router<OpenShop>> {
    let mut router = Router::new();
    let definitions = &app.config().routes;
    let routes = resolve_route_paths("server", definitions.keys().map(String::as_str))?;

    for (route_path, key) in &routes {
        let route = definitions[*key].clone();
        assert_route_definition(&route, route_path)?;
        let allowed = route.allowed_methods().join(", ");

        let registered = Arc::new(RegisteredRoute {
            path: route_path.clone(),
            definition: route,
        });
        let handler = any(
            move |State(app): State<OpenShop>,
                  method: Method,
                  uri: Uri,
                  params: RawPathParams,
                  headers: HeaderMap,
                  body: Bytes| { handle(registered.clone(), app, method, uri, params, headers, body) },
        );

        let full_path = format!("/routes{}", to_router_path(route_path));
        router = router.route(full_path.trim_end_matches('/'), handler.clone());
        if route_path == "/" {
            router = router.route("/routes/", handler);
        }
        if log {
            logger::info(format!("[openshop] Server route: {route_path} ({allowed})"));
        }
    }

    if log && !routes.is_empty() {
        logger::info(format!("[openshop] {} server route(s) registered", routes.len()));
    }
    Ok(router)
}
