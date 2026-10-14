use std::path::Path;

use openshop::db::migrate::check_migrations;
use openshop_template_public::APP;

/// `./migrations` creates the framework schema and the tables of the models.
/// After a change to a model, or an upgrade of OpenShop: `cargo run -- migrate generate`.
#[test]
fn migrations_cover_the_schema() {
    let config = APP.config().unwrap();
    check_migrations(Path::new(env!("CARGO_MANIFEST_DIR")), &config.models).unwrap();
}
