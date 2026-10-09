//! The views of custom admin pages (feature `ui`).
//!
//! A view is the function marked `#[view]` in the `#[admin_page]` block of a
//! page, next to the loaders and the actions it calls. It is written with
//! [Leptos](https://leptos.dev) and Shopify's Polaris web components, and
//! compiled to WebAssembly into the admin UI bundle of the application.
//!
//! ```
//! use leptos::prelude::*;
//! use openshop::prelude::*;
//! use openshop::ui::use_loader;
//!
//! pub struct HelloPage;
//!
//! #[admin_page("/hello", nav = "Hello")]
//! impl HelloPage {
//!     #[loader]
//!     async fn greeting(ctx: AdminContext) -> Result<String> {
//!         Ok(format!("Hello {}", ctx.shop))
//!     }
//!
//!     #[view]
//!     fn view() -> impl IntoView {
//!         // `HelloPage::GREETING` names the loader, with the type it returns.
//!         let greeting = use_loader(Self::GREETING, || ());
//!         view! { <s-page heading="Hello">{move || greeting.data.get()}</s-page> }
//!     }
//! }
//! ```
//!
//! `openshop dev` and `openshop build` build the bundle from the modules of
//! the application that declare pages, compiled on their own: outside the
//! functions the server runs, a page names nothing else of the application.

pub use openshop_ui::admin::{
    ActionConcurrency, AdminActionOptions, AdminActionState, AdminFunctionKind, AdminFunctionRef, AdminLoaderState,
    AdminRpcError, use_action, use_loader,
};
pub use openshop_ui::{RouteParams, navigate, shopify};

/// A page of the admin UI bundle: a path and the view rendered for it.
#[doc(hidden)]
pub use openshop_ui::CustomAdminPage as BundlePage;

/// The view of a declared admin page, as `#[admin_page]` implements it.
#[doc(hidden)]
pub trait PageView {
    /// The page of the bundle, for a page that has a view.
    fn page_view() -> Option<BundlePage>;
}

/// The page of the bundle a `#[view]` renders. What `#[admin_page]` calls.
#[doc(hidden)]
pub fn page_view<Marker>(path: &str, view: impl openshop_ui::CustomPageView<Marker>) -> BundlePage {
    BundlePage::new(path, view)
}

/// Mounts the admin UI: the built-in screens, and the views of the pages. The
/// `main` of the bundle `openshop` builds.
#[cfg(target_arch = "wasm32")]
#[doc(hidden)]
pub fn mount(pages: impl IntoIterator<Item = Option<BundlePage>>) {
    pages
        .into_iter()
        .flatten()
        .fold(openshop_ui::App::new(), openshop_ui::App::custom_page)
        .mount();
}
