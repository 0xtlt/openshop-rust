//! The MCP capability registry: core and application permissions, tools and resources.

use std::collections::HashSet;
use std::sync::LazyLock;

use anyhow::{Result, bail};
use indexmap::IndexMap;
use regex::Regex;
use serde::Serialize;

use super::core::CoreCapabilities;
use crate::types::{McpConfig, McpPermission, McpResource, McpRiskLevel, McpTool};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum McpSource {
    Core,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisteredMcpPermission {
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub group: String,
    pub risk_level: McpRiskLevel,
    pub key: String,
    pub source: McpSource,
}

#[derive(Clone)]
pub struct RegisteredMcpTool {
    pub name: String,
    pub source: McpSource,
    pub definition: McpTool,
}

#[derive(Clone)]
pub struct RegisteredMcpResource {
    pub uri: String,
    pub source: McpSource,
    pub definition: McpResource,
}

#[derive(Clone)]
pub struct McpRegistry {
    pub enabled: bool,
    pub permissions: IndexMap<String, RegisteredMcpPermission>,
    pub tools: IndexMap<String, RegisteredMcpTool>,
    pub resources: IndexMap<String, RegisteredMcpResource>,
}

fn core_permission_definitions() -> [(&'static str, &'static str, &'static str, &'static str, McpRiskLevel); 6] {
    use McpRiskLevel::{High, Low, Medium};
    [
        (
            "read_logs",
            "Read logs",
            "Read OpenShop flow run logs.",
            "Observability",
            Low,
        ),
        (
            "read_flows",
            "Read flows",
            "List OpenShop flows and their schemas.",
            "Flows",
            Low,
        ),
        ("run_flows", "Run flows", "Trigger OpenShop flow runs.", "Flows", High),
        (
            "read_crons",
            "Read crons",
            "List configured OpenShop crons.",
            "Automation",
            Low,
        ),
        (
            "manage_crons",
            "Manage crons",
            "Enable or disable OpenShop crons.",
            "Automation",
            Medium,
        ),
        (
            "shopify_admin_graphql",
            "Shopify Admin GraphQL",
            "Run arbitrary Shopify Admin GraphQL queries and mutations for installed shops.",
            "Shopify Admin",
            High,
        ),
    ]
}

static CUSTOM_PERMISSION_KEY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z][a-z0-9-]*:[a-z][a-z0-9]*(?:_[a-z0-9]+)*$").expect("valid regex"));
static TOOL_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z][a-z0-9]*(?:[._-][a-z0-9]+)*$").expect("valid regex"));
static RESOURCE_URI: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z][a-z0-9+.-]*://.+").expect("valid regex"));
const FORBIDDEN_PERMISSION_KEYS: [&str; 6] = ["*", "all", "admin", "root", "full_access", "full-access"];

macro_rules! fail {
    ($($arg:tt)*) => {
        bail!("[openshop] Invalid MCP config: {}", format!($($arg)*))
    };
}

pub fn core_mcp_permissions() -> IndexMap<String, RegisteredMcpPermission> {
    core_permission_definitions()
        .into_iter()
        .map(|(key, label, description, group, risk_level)| {
            (
                key.to_string(),
                RegisteredMcpPermission {
                    label: label.to_string(),
                    description: Some(description.to_string()),
                    group: group.to_string(),
                    risk_level,
                    key: key.to_string(),
                    source: McpSource::Core,
                },
            )
        })
        .collect()
}

fn normalize_permissions(
    custom: &IndexMap<String, McpPermission>,
) -> Result<IndexMap<String, RegisteredMcpPermission>> {
    let mut permissions = core_mcp_permissions();

    for (key, permission) in custom {
        if FORBIDDEN_PERMISSION_KEYS.contains(&key.as_str()) {
            fail!("permission \"{key}\" is not allowed");
        }
        if !CUSTOM_PERMISSION_KEY.is_match(key) {
            fail!("custom permission \"{key}\" must use namespace:action_resource");
        }
        if permissions.contains_key(key) {
            fail!("custom permission \"{key}\" conflicts with a core permission");
        }
        if permission.label.trim().is_empty() {
            fail!("permissions.custom.{key}.label must be a non-empty string");
        }
        permissions.insert(
            key.clone(),
            RegisteredMcpPermission {
                label: permission.label.to_string(),
                description: permission.description.map(str::to_string),
                group: permission
                    .group
                    .map(str::to_string)
                    .unwrap_or_else(|| key.split(':').next().unwrap_or("Custom").to_string()),
                risk_level: permission.risk_level.unwrap_or(McpRiskLevel::Medium),
                key: key.clone(),
                source: McpSource::Custom,
            },
        );
    }

    Ok(permissions)
}

fn assert_known_permissions(
    owner_path: &str,
    required: &[String],
    permissions: &IndexMap<String, RegisteredMcpPermission>,
) -> Result<Vec<String>> {
    let mut keys: Vec<String> = Vec::new();
    for permission in required {
        if permission.trim().is_empty() {
            fail!("{owner_path}.requiredPermissions must contain strings");
        }
        if !permissions.contains_key(permission) {
            fail!("{owner_path} references unknown permission \"{permission}\"");
        }
        if !keys.contains(permission) {
            keys.push(permission.clone());
        }
    }
    Ok(keys)
}

fn normalize_tools(
    custom: &IndexMap<String, McpTool>,
    core: &IndexMap<String, McpTool>,
    permissions: &IndexMap<String, RegisteredMcpPermission>,
) -> Result<IndexMap<String, RegisteredMcpTool>> {
    let mut tools = IndexMap::new();

    for (name, tool) in core {
        let mut definition = tool.clone();
        definition.required_permissions = assert_known_permissions(
            &format!("mcp.coreTools.{name}"),
            &tool.required_permissions,
            permissions,
        )?;
        tools.insert(
            name.clone(),
            RegisteredMcpTool {
                name: name.clone(),
                source: McpSource::Core,
                definition,
            },
        );
    }

    for (name, tool) in custom {
        if !TOOL_NAME.is_match(name) {
            fail!("mcp.tools.{name} has an invalid tool name");
        }
        if tools.contains_key(name) {
            fail!("mcp.tools.{name} conflicts with a core tool");
        }
        if tool.description.trim().is_empty() {
            fail!("mcp.tools.{name}.description must be a non-empty string");
        }
        let mut definition = tool.clone();
        definition.required_permissions =
            assert_known_permissions(&format!("mcp.tools.{name}"), &tool.required_permissions, permissions)?;
        tools.insert(
            name.clone(),
            RegisteredMcpTool {
                name: name.clone(),
                source: McpSource::Custom,
                definition,
            },
        );
    }

    Ok(tools)
}

fn normalize_resources(
    custom: &IndexMap<String, McpResource>,
    core: &IndexMap<String, McpResource>,
    permissions: &IndexMap<String, RegisteredMcpPermission>,
) -> Result<IndexMap<String, RegisteredMcpResource>> {
    let mut resources = IndexMap::new();

    for (uri, resource) in core {
        let mut definition = resource.clone();
        definition.required_permissions = assert_known_permissions(
            &format!("mcp.coreResources.{uri}"),
            &resource.required_permissions,
            permissions,
        )?;
        resources.insert(
            uri.clone(),
            RegisteredMcpResource {
                uri: uri.clone(),
                source: McpSource::Core,
                definition,
            },
        );
    }

    for (uri, resource) in custom {
        if !RESOURCE_URI.is_match(uri) {
            fail!("mcp.resources.{uri} must be a URI");
        }
        if resources.contains_key(uri) {
            fail!("mcp.resources.{uri} conflicts with a core resource");
        }
        if resource.name.trim().is_empty() {
            fail!("mcp.resources.{uri}.name must be a non-empty string");
        }
        let mut definition = resource.clone();
        definition.required_permissions = assert_known_permissions(
            &format!("mcp.resources.{uri}"),
            &resource.required_permissions,
            permissions,
        )?;
        resources.insert(
            uri.clone(),
            RegisteredMcpResource {
                uri: uri.clone(),
                source: McpSource::Custom,
                definition,
            },
        );
    }

    Ok(resources)
}

pub fn build_mcp_registry(mcp: Option<&McpConfig>, core: &CoreCapabilities) -> Result<McpRegistry> {
    let empty = McpConfig::default();
    let mcp = mcp.unwrap_or(&empty);
    // The permissions tools and resources came with are registered with them.
    let custom = mcp.permissions();
    let declared = mcp.tools.values().flat_map(|tool| &tool.declared_permissions).chain(
        mcp.resources
            .values()
            .flat_map(|resource| &resource.declared_permissions),
    );
    for permission in declared {
        if custom.get(permission.key) != Some(permission) {
            fail!("permission \"{}\" is declared with two definitions", permission.key);
        }
    }
    let permissions = normalize_permissions(&custom)?;

    Ok(McpRegistry {
        enabled: mcp.enabled != Some(false),
        tools: normalize_tools(&mcp.tools, &core.tools, &permissions)?,
        resources: normalize_resources(&mcp.resources, &core.resources, &permissions)?,
        permissions,
    })
}

pub fn has_all_permissions(granted: &[String], required: &[String]) -> bool {
    let granted: HashSet<&str> = granted.iter().map(String::as_str).collect();
    required.iter().all(|permission| granted.contains(permission.as_str()))
}
