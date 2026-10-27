//! The integration tests of `openshop`, in one binary: one module per
//! subject, linked once instead of once per file. Every test gets its own
//! database schema.
//!
//! What belongs to the process is shared by every module: a test that reads
//! the process logger does it through [`common::LogCapture`], which takes
//! turns with the others. The tests that bind a Sentry client for the process,
//! set its environment or install a panic hook (`sentry*.rs`), and the
//! `trybuild` tests of the macros (`macros.rs`), are binaries of their own.

mod common;

mod admin_views;
mod api;
mod auth;
mod client_credentials;
mod contexts;
mod custom_admin_pages;
mod declared;
mod engine_dispatch;
mod engine_runner;
mod engine_scheduler;
mod engine_step;
mod engine_worker;
mod functions;
mod graphql_typed;
mod logs_retention;
mod mcp;
mod migrations;
mod models;
mod ops;
mod pages;
mod pool;
mod provider_structs;
mod proxy_routes;
mod records;
mod security;
mod server_limits;
mod server_routes;
mod static_ui;
mod testing_harness;
mod typed_providers;
mod webhooks;
