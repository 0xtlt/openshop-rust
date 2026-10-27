//! Public server routes under `/routes`, each with an explicit authentication contract.

use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{RawPathParams, State};
use axum::http::header::ALLOW;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use indexmap::IndexMap;
use serde_json::{Value, json};

use super::file_routes::mount_routes;
use super::http::{catch_panic, json_error, report_failure};
use crate::app::OpenShop;
use crate::declare::InvalidInput;
use crate::runtime::report;
use crate::types::{
    Route, RouteAuth, RouteAuthContext, RouteAuthOutcome, RouteAuthValue, RouteContext, RouteMethod, RouteRequest,
};

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
    report::describe(|request| {
        request.unit("route", format_args!("{method_name} {route_path}"));
    });

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

    // A public route has no authentication value: its handlers find `Null`.
    let mut auth: RouteAuthValue = Box::new(Value::Null);
    if let RouteAuth::Custom(authenticate) = &authentication {
        let context = RouteAuthContext {
            request: request.clone(),
            params: params.clone(),
            db: app.db().clone(),
            app: app.clone(),
        };
        match catch_panic("authentication", authenticate(context)).await {
            Ok(RouteAuthOutcome::Authenticated(value)) => auth = value,
            Ok(RouteAuthOutcome::Unauthorized) => return json_error(StatusCode::UNAUTHORIZED, "Unauthorized"),
            Ok(RouteAuthOutcome::Response(response)) => return response,
            Err(error) => {
                report_failure(
                    format!("[openshop] Server route authentication error for {method_name} {route_path}"),
                    &error,
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
    match catch_panic("handler", handler(context)).await {
        Ok(response) => response,
        // The handler refused its input.
        Err(error) if error.is::<InvalidInput>() => json_error(StatusCode::UNPROCESSABLE_ENTITY, error.to_string()),
        Err(error) => {
            report_failure(format!("[openshop] Server route error for {method_name} {route_path}"), &error);
            internal_error()
        }
    }
}

/// Fails when a route declares no HTTP method. The configuration is checked
/// with it when the application starts.
pub fn assert_route_definition(route: &Route, route_path: &str) -> anyhow::Result<()> {
    if route.handlers.is_empty() {
        anyhow::bail!("[openshop] Invalid server route {route_path}: define at least one HTTP method");
    }
    Ok(())
}

pub fn create_server_routes(app: &OpenShop, log: bool) -> anyhow::Result<Router<OpenShop>> {
    let definitions = &app.config().routes;
    mount_routes("Server", "/routes", definitions.keys().map(String::as_str), log, |route_path, key| {
        let route = definitions[key].clone();
        let allowed = route.allowed_methods().join(", ");
        let registered = Arc::new(RegisteredRoute {
            path: route_path.to_string(),
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
        (handler, allowed)
    })
}
