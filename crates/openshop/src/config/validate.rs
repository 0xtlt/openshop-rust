//! Validation of an application definition. Runs when the configuration is built.

use std::collections::HashSet;
use std::sync::LazyLock;
use std::time::Duration;

use anyhow::{Result, bail};
use regex::Regex;

use crate::admin::validate_custom_admin_navigation;
use crate::app::OpenShopConfig;
use crate::engine::scheduler::parse_schedule;
use crate::server::file_routes::resolve_route_paths;
use crate::server::mcp::core::create_core_mcp_capabilities;
use crate::server::mcp::registry::build_mcp_registry;
use crate::server::routes::assert_route_definition;
use crate::types::{
    FieldDef, FieldType, PartialRetryPolicy, PartialWorkerConfig, ShopifyAppConfig, ShopifyConfig, ShopifyFunctionType,
};

macro_rules! fail {
    ($($arg:tt)*) => {
        bail!("[openshop] Invalid config: {}", format!($($arg)*))
    };
}

fn assert_positive_number(value: Option<f64>, path: &str) -> Result<()> {
    if value.is_some_and(|value| !value.is_finite() || value <= 0.0) {
        fail!("{path} must be a positive number");
    }
    Ok(())
}

fn assert_positive_integer(value: Option<u64>, path: &str) -> Result<()> {
    if value == Some(0) {
        fail!("{path} must be a positive integer");
    }
    Ok(())
}

fn assert_positive_duration(value: Option<Duration>, path: &str) -> Result<()> {
    if value.is_some_and(|value| value.is_zero()) {
        fail!("{path} must be a positive number");
    }
    Ok(())
}

fn validate_retry_policy(policy: &PartialRetryPolicy, path: &str) -> Result<()> {
    assert_positive_integer(policy.max_attempts, &format!("{path}.maxAttempts"))?;
    assert_positive_integer(policy.initial_interval_ms, &format!("{path}.initialIntervalMs"))?;
    assert_positive_number(policy.backoff_coefficient, &format!("{path}.backoffCoefficient"))?;
    assert_positive_integer(policy.max_interval_ms, &format!("{path}.maxIntervalMs"))
}

fn validate_worker_config(worker: &PartialWorkerConfig) -> Result<()> {
    assert_positive_integer(worker.concurrency.map(|value| value as u64), "worker.concurrency")?;
    assert_positive_integer(worker.poll_interval_ms, "worker.pollIntervalMs")?;
    assert_positive_integer(worker.poll_max_interval_ms, "worker.pollMaxIntervalMs")?;
    assert_positive_number(worker.poll_backoff_coefficient, "worker.pollBackoffCoefficient")?;
    assert_positive_integer(worker.lease_duration_ms, "worker.leaseDurationMs")
}

static APP_HANDLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-zA-Z0-9_-]+$").expect("valid regex"));

fn validate_shopify_config(config: Option<&ShopifyConfig>) -> Result<()> {
    let Some(config) = config else {
        return Ok(());
    };
    if config.scopes.as_deref().is_some_and(|scopes| scopes.trim().is_empty()) {
        fail!("shopify.scopes must be a non-empty string");
    }

    for (handle, app) in config.apps.iter().flatten() {
        if !APP_HANDLE.is_match(handle) {
            fail!("shopify.apps.{handle} handle may only contain letters, numbers, \"_\" and \"-\"");
        }
        if app.api_secret().trim().is_empty() {
            fail!("shopify.apps.{handle}.apiSecret must be a non-empty string");
        }
        match app {
            ShopifyAppConfig::Toml { toml, .. } if toml.trim().is_empty() => {
                fail!("shopify.apps.{handle}.toml must be a non-empty string");
            }
            ShopifyAppConfig::Keys { api_key, .. } if api_key.trim().is_empty() => {
                fail!("shopify.apps.{handle}.apiKey must be a non-empty string when toml is not set");
            }
            _ => {}
        }
        if app.app_url().is_some_and(|url| url.trim().is_empty()) {
            fail!("shopify.apps.{handle}.appUrl must be a non-empty string");
        }
    }
    Ok(())
}

fn validate_field(field: &FieldDef, path: &str) -> Result<()> {
    if field.label.trim().is_empty() {
        fail!("{path}.label must be a non-empty string");
    }

    if field.field_type == FieldType::Select {
        let options = field.options.as_deref().unwrap_or_default();
        if options.is_empty() {
            fail!("{path}.options must be a non-empty array for select fields");
        }
        let mut values = HashSet::new();
        for option in options {
            if !values.insert(option.value.as_str()) {
                fail!("{path}.options contains duplicate value \"{}\"", option.value);
            }
        }
    }
    Ok(())
}

pub fn validate_openshop_config(config: &OpenShopConfig) -> Result<()> {
    validate_shopify_config(config.shopify.as_ref())?;

    for (key, provider) in &config.providers {
        if provider.name.trim().is_empty() {
            fail!("providers.{key}.name must be a non-empty string");
        }
        for (field_name, field) in &provider.fields {
            validate_field(field, &format!("providers.{key}.ui.fields.{field_name}"))?;
        }
    }

    for (key, flow) in &config.flows {
        if flow.name.trim().is_empty() {
            fail!("flows.{key}.name must be a non-empty string");
        }
        assert_positive_duration(flow.timeout, &format!("flows.{key}.timeout"))?;
        assert_positive_duration(flow.step_timeout, &format!("flows.{key}.stepTimeout"))?;
        validate_retry_policy(&flow.retry_policy, &format!("flows.{key}.retryPolicy"))?;
    }

    for (index, cron) in config.crons.iter().enumerate() {
        if cron.schedule.trim().is_empty() {
            fail!("crons[{index}].schedule must be a non-empty string");
        }
        if !config.flows.contains_key(&cron.flow) {
            fail!("crons[{index}].flow references unknown flow \"{}\"", cron.flow);
        }
        if let Err(error) = parse_schedule(&cron.schedule, &cron.flow) {
            let message = error.to_string();
            fail!(
                "crons[{index}].schedule is invalid: {}",
                message.trim_start_matches("[openshop] ")
            );
        }
    }

    let mut function_handles = HashSet::new();
    for (key, function) in &config.functions {
        if function.handle.trim().is_empty() {
            fail!("functions.{key}.handle must be a non-empty string");
        }
        if function.label.as_deref().is_some_and(|label| label.trim().is_empty()) {
            fail!("functions.{key}.label must be a non-empty string");
        }
        if !function_handles.insert(function.handle.as_str()) {
            fail!("functions.{key}.handle duplicates \"{}\"", function.handle);
        }
        if function.function_type != ShopifyFunctionType::Discount && function.modes.is_some() {
            fail!("functions.{key}.modes is only supported for discount functions");
        }
        for (field_name, field) in &function.config {
            validate_field(field, &format!("functions.{key}.config.{field_name}"))?;
        }
    }

    for (index, item) in config.custom_navigation().iter().enumerate() {
        if item.label.trim().is_empty() {
            fail!("experimental.customPages.navigation[{index}].label must be a non-empty string");
        }
        if item.path.trim().is_empty() {
            fail!("experimental.customPages.navigation[{index}].path must be a non-empty string");
        }
    }
    let admin_pages = config.resolved_admin_pages()?;
    if config.custom_pages_enabled() {
        validate_custom_admin_navigation(config.custom_navigation(), &admin_pages)?;
    }

    for (path, route) in resolve_route_paths("server", config.routes.keys().map(String::as_str))? {
        assert_route_definition(&config.routes[route], &path)?;
    }
    resolve_route_paths("proxy", config.proxies.keys().map(String::as_str))?;

    validate_worker_config(&config.worker)?;
    validate_retry_policy(&config.retry_policy, "retryPolicy")?;
    build_mcp_registry(config.mcp.as_ref(), &create_core_mcp_capabilities())?;
    Ok(())
}
