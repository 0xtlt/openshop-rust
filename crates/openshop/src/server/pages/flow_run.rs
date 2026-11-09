//! `/runs/:id`: a run of a flow, its steps, its output and its logs.
//!
//! Not rendered by the server yet: a stub that says so.

use anyhow::Result;
use maud::Markup;

use super::BuiltinPage;
use crate::admin::Outcome;

pub(super) async fn render(_page: &BuiltinPage<'_>) -> Result<Markup> {
    Ok(super::not_ported("Run"))
}

pub(super) async fn submit(_page: &BuiltinPage<'_>, _action: &str, _fields: &[(String, String)]) -> Result<Outcome> {
    Err(super::no_action())
}
