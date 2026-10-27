//! Built-in admin screens that can be hidden from navigation or disabled entirely.
//! Home is always visible.

use serde::{Deserialize, Serialize};

/// Admin screens that can be hidden from nav or disabled entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AdminPageId {
    Flows,
    Providers,
    Crons,
    Functions,
    Mcp,
}

/// `visible` shows the nav link; `hidden` keeps the URL; `disabled` blocks UI and admin API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AdminPageMode {
    #[default]
    Visible,
    Hidden,
    Disabled,
}

pub const ADMIN_PAGE_IDS: [AdminPageId; 5] = [
    AdminPageId::Flows,
    AdminPageId::Providers,
    AdminPageId::Crons,
    AdminPageId::Functions,
    AdminPageId::Mcp,
];

/// Partial page configuration: unset pages default to `visible`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AdminPagesConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flows: Option<AdminPageMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub providers: Option<AdminPageMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crons: Option<AdminPageMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub functions: Option<AdminPageMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp: Option<AdminPageMode>,
}

impl AdminPagesConfig {
    /// Every built-in page `disabled`: the merchant reaches the pages of the
    /// application and nothing else. A page is opened again by naming it:
    /// `AdminPagesConfig { providers: Some(AdminPageMode::Visible), ..AdminPagesConfig::none() }`.
    pub const fn none() -> Self {
        let disabled = Some(AdminPageMode::Disabled);
        Self {
            flows: disabled,
            providers: disabled,
            crons: disabled,
            functions: disabled,
            mcp: disabled,
        }
    }

    pub fn get(&self, id: AdminPageId) -> Option<AdminPageMode> {
        match id {
            AdminPageId::Flows => self.flows,
            AdminPageId::Providers => self.providers,
            AdminPageId::Crons => self.crons,
            AdminPageId::Functions => self.functions,
            AdminPageId::Mcp => self.mcp,
        }
    }

    pub fn set(&mut self, id: AdminPageId, mode: AdminPageMode) {
        match id {
            AdminPageId::Flows => self.flows = Some(mode),
            AdminPageId::Providers => self.providers = Some(mode),
            AdminPageId::Crons => self.crons = Some(mode),
            AdminPageId::Functions => self.functions = Some(mode),
            AdminPageId::Mcp => self.mcp = Some(mode),
        }
    }

    /// Later values win, like `{ ...app.pages, ...config.pages }`.
    pub fn merge(self, other: Self) -> Self {
        Self {
            flows: other.flows.or(self.flows),
            providers: other.providers.or(self.providers),
            crons: other.crons.or(self.crons),
            functions: other.functions.or(self.functions),
            mcp: other.mcp.or(self.mcp),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedAdminPages {
    pub flows: AdminPageMode,
    pub providers: AdminPageMode,
    pub crons: AdminPageMode,
    pub functions: AdminPageMode,
    pub mcp: AdminPageMode,
}

impl ResolvedAdminPages {
    pub fn get(&self, id: AdminPageId) -> AdminPageMode {
        match id {
            AdminPageId::Flows => self.flows,
            AdminPageId::Providers => self.providers,
            AdminPageId::Crons => self.crons,
            AdminPageId::Functions => self.functions,
            AdminPageId::Mcp => self.mcp,
        }
    }
}

/// Config-backed resources that determine whether a sidebar destination has content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AdminNavigationResources {
    pub flows: usize,
    pub providers: usize,
    pub crons: usize,
    pub functions: usize,
}

pub fn resolve_admin_pages(
    pages: Option<&AdminPagesConfig>,
    resources: Option<&AdminNavigationResources>,
) -> ResolvedAdminPages {
    let mode = |id: AdminPageId| pages.and_then(|pages| pages.get(id)).unwrap_or_default();
    let resolved = ResolvedAdminPages {
        flows: mode(AdminPageId::Flows),
        providers: mode(AdminPageId::Providers),
        crons: mode(AdminPageId::Crons),
        functions: mode(AdminPageId::Functions),
        mcp: mode(AdminPageId::Mcp),
    };

    let Some(resources) = resources else {
        return resolved;
    };

    ResolvedAdminPages {
        flows: hide_empty_page(resolved.flows, resources.flows > 0),
        providers: hide_empty_page(resolved.providers, resources.providers > 0),
        crons: hide_empty_page(resolved.crons, resources.crons > 0),
        functions: hide_empty_page(resolved.functions, resources.functions > 0),
        mcp: resolved.mcp,
    }
}

fn hide_empty_page(mode: AdminPageMode, has_resources: bool) -> AdminPageMode {
    if mode == AdminPageMode::Visible && !has_resources {
        AdminPageMode::Hidden
    } else {
        mode
    }
}

fn page_from_section(section: Option<&str>) -> Option<AdminPageId> {
    match section? {
        "flows" | "runs" => Some(AdminPageId::Flows),
        "providers" => Some(AdminPageId::Providers),
        "crons" => Some(AdminPageId::Crons),
        "functions" => Some(AdminPageId::Functions),
        "mcp" => Some(AdminPageId::Mcp),
        _ => None,
    }
}

/// Maps an Admin API pathname such as `/api/flows` to a gated page, or `None` when ungated.
pub fn admin_page_from_api_path(pathname: &str) -> Option<AdminPageId> {
    let mut segments = pathname.split('/').filter(|segment| !segment.is_empty());
    if segments.next() != Some("api") {
        return None;
    }
    page_from_section(segments.next())
}

/// Maps an embedded UI pathname such as `/runs/:id` to a gated page, or `None` for Home.
pub fn admin_page_from_ui_path(pathname: &str) -> Option<AdminPageId> {
    page_from_section(pathname.split('/').find(|segment| !segment.is_empty()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resources(flows: usize, providers: usize, crons: usize, functions: usize) -> AdminNavigationResources {
        AdminNavigationResources {
            flows,
            providers,
            crons,
            functions,
        }
    }

    #[test]
    fn defaults_every_page_to_visible() {
        let resolved = resolve_admin_pages(None, None);
        for id in ADMIN_PAGE_IDS {
            assert_eq!(resolved.get(id), AdminPageMode::Visible);
        }
    }

    #[test]
    fn keeps_explicit_modes() {
        let pages = AdminPagesConfig {
            flows: Some(AdminPageMode::Hidden),
            mcp: Some(AdminPageMode::Disabled),
            ..Default::default()
        };
        let resolved = resolve_admin_pages(Some(&pages), None);
        assert_eq!(resolved.flows, AdminPageMode::Hidden);
        assert_eq!(resolved.providers, AdminPageMode::Visible);
        assert_eq!(resolved.mcp, AdminPageMode::Disabled);
    }

    #[test]
    fn hides_visible_pages_without_resources() {
        let resolved = resolve_admin_pages(None, Some(&resources(1, 0, 0, 0)));
        assert_eq!(resolved.flows, AdminPageMode::Visible);
        assert_eq!(resolved.providers, AdminPageMode::Hidden);
        assert_eq!(resolved.crons, AdminPageMode::Hidden);
        assert_eq!(resolved.functions, AdminPageMode::Hidden);
        assert_eq!(resolved.mcp, AdminPageMode::Visible);
    }

    #[test]
    fn never_reveals_hidden_or_disabled_pages_with_resources() {
        let pages = AdminPagesConfig {
            flows: Some(AdminPageMode::Disabled),
            providers: Some(AdminPageMode::Hidden),
            ..Default::default()
        };
        let resolved = resolve_admin_pages(Some(&pages), Some(&resources(2, 2, 2, 2)));
        assert_eq!(resolved.flows, AdminPageMode::Disabled);
        assert_eq!(resolved.providers, AdminPageMode::Hidden);
        assert_eq!(resolved.crons, AdminPageMode::Visible);
    }

    #[test]
    fn none_disables_every_page_and_one_can_be_opened_again() {
        let closed = resolve_admin_pages(Some(&AdminPagesConfig::none()), Some(&resources(2, 2, 2, 2)));
        for id in ADMIN_PAGE_IDS {
            assert_eq!(closed.get(id), AdminPageMode::Disabled);
        }

        let pages = AdminPagesConfig {
            providers: Some(AdminPageMode::Visible),
            ..AdminPagesConfig::none()
        };
        let resolved = resolve_admin_pages(Some(&pages), Some(&resources(2, 2, 2, 2)));
        assert_eq!(resolved.providers, AdminPageMode::Visible);
        assert_eq!(resolved.flows, AdminPageMode::Disabled);
    }

    #[test]
    fn keeps_disabled_pages_disabled_without_resources() {
        let pages = AdminPagesConfig {
            crons: Some(AdminPageMode::Disabled),
            ..Default::default()
        };
        let resolved = resolve_admin_pages(Some(&pages), Some(&resources(0, 0, 0, 0)));
        assert_eq!(resolved.crons, AdminPageMode::Disabled);
    }

    #[test]
    fn maps_api_paths_to_gated_pages() {
        assert_eq!(admin_page_from_api_path("/api/flows"), Some(AdminPageId::Flows));
        assert_eq!(
            admin_page_from_api_path("/api/flows/sync/run"),
            Some(AdminPageId::Flows)
        );
        assert_eq!(admin_page_from_api_path("/api/runs/abc/logs"), Some(AdminPageId::Flows));
        assert_eq!(
            admin_page_from_api_path("/api/providers/x"),
            Some(AdminPageId::Providers)
        );
        assert_eq!(admin_page_from_api_path("/api/crons/toggle"), Some(AdminPageId::Crons));
        assert_eq!(admin_page_from_api_path("/api/functions"), Some(AdminPageId::Functions));
        assert_eq!(admin_page_from_api_path("/api/mcp/tokens"), Some(AdminPageId::Mcp));
        assert_eq!(admin_page_from_api_path("/api/pages"), None);
        assert_eq!(admin_page_from_api_path("/api"), None);
        assert_eq!(admin_page_from_api_path("/flows"), None);
    }

    #[test]
    fn maps_ui_paths_to_gated_pages() {
        assert_eq!(admin_page_from_ui_path("/"), None);
        assert_eq!(admin_page_from_ui_path("/flows/sync"), Some(AdminPageId::Flows));
        assert_eq!(admin_page_from_ui_path("/runs/123"), Some(AdminPageId::Flows));
        assert_eq!(admin_page_from_ui_path("/mcp"), Some(AdminPageId::Mcp));
        assert_eq!(admin_page_from_ui_path("/reviews"), None);
    }

    #[test]
    fn merges_like_object_spread() {
        let app = AdminPagesConfig {
            flows: Some(AdminPageMode::Hidden),
            crons: Some(AdminPageMode::Disabled),
            ..Default::default()
        };
        let config = AdminPagesConfig {
            flows: Some(AdminPageMode::Visible),
            ..Default::default()
        };
        let merged = app.merge(config);
        assert_eq!(merged.flows, Some(AdminPageMode::Visible));
        assert_eq!(merged.crons, Some(AdminPageMode::Disabled));
        assert_eq!(merged.mcp, None);
    }
}
