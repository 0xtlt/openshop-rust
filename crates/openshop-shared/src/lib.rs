//! Types and pure logic of the OpenShop server that need nothing of it: the
//! paths of the admin pages, their modes, and the encoding of a path segment.

pub mod custom_pages;
pub mod json;
pub mod pages;
pub mod rpc;

pub use custom_pages::{
    ADMIN_HOME_PATH, AdminMenuItem, CustomAdminPageAccess, CustomAdminPagesResponse, custom_admin_route_params,
    is_custom_admin_page_pattern, is_reserved_custom_admin_path, is_safe_custom_admin_path, is_safe_path_segment,
    match_custom_admin_path, path_segments, sort_custom_admin_routes,
};
pub use pages::{
    ADMIN_PAGE_IDS, AdminNavigationResources, AdminPageId, AdminPageMode, AdminPagesConfig, ResolvedAdminPages,
    admin_page_from_api_path, admin_page_from_ui_path, resolve_admin_pages,
};
pub use rpc::AdminFunctionKind;
