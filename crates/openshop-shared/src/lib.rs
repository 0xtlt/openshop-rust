//! Types and pure logic shared by the OpenShop server and its WebAssembly admin UI.
//!
//! Everything here compiles for both native targets and `wasm32-unknown-unknown`.

pub mod custom_pages;
pub mod pages;
pub mod rpc;

pub use custom_pages::{
    CUSTOM_ADMIN_RESERVED_PREFIXES, CustomAdminNavigationItem, CustomAdminPageAccess, CustomAdminPagesResponse,
    custom_admin_route_shape_key, is_reserved_custom_admin_path, is_safe_custom_admin_path, match_custom_admin_path,
};
pub use pages::{
    ADMIN_PAGE_IDS, ADMIN_PAGE_MODES, AdminNavigationResources, AdminPageId, AdminPageMode, AdminPagesConfig,
    ResolvedAdminPages, admin_page_from_api_path, admin_page_from_ui_path, resolve_admin_pages, same_admin_pages,
};
pub use rpc::build_admin_function_rpc_url;
