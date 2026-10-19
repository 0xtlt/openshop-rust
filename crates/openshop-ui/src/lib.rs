#![doc = include_str!("../README.md")]

mod admin_pages;
mod app;
mod components;
mod custom_admin_pages;
mod dom;
mod events;
mod fetch;
mod format;
mod http;
mod json;
mod navigation;
mod pages;
mod provider_status;
mod query;
mod reactive;
mod report;
mod router;
mod types;
mod url;

pub mod admin;
pub mod shopify;

pub use app::App;
pub use custom_admin_pages::{CustomAdminPage, CustomPageView, RouteParams};
pub use router::route as navigate;
