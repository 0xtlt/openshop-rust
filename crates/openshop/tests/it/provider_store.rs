//! `Mailer::config_of(&ctx)`: the configuration of a provider managed by the
//! code of the application, with the built-in providers page disabled.

use crate::common::shop_connectors as connectors;
use maud::html;
use openshop::prelude::*;
use openshop::providers::{Check, Saved};
use openshop::server::ServerOptions;
use openshop::testing::{TestContext, TestOptions};
use openshop::{AdminPageId, AdminPageMode, AdminPagesConfig, Connectors, OpenShop, OpenShopConfig, UiSource};
use serde::Serialize;

#[derive(Debug, ProviderConfig)]
struct Mailer {
    #[field(label = "API URL", url)]
    api_url: String,
    #[field(label = "API key", password, non_empty)]
    api_key: String,
    #[field(range(min = 1, max = 100), name = "perBatch")]
    batch_size: Option<u32>,
}

#[provider]
impl Mailer {
    #[checker]
    async fn check(&self) -> Result<bool> {
        if self.api_url.contains("panic") {
            panic!("the checker panicked");
        }
        Ok(self.api_key == "ml_live")
    }

    pub async fn sender(&self) -> Result<String> {
        Ok(format!("{} with {}", self.api_url, self.api_key))
    }
}

/// A provider without a checker.
#[derive(Debug, ProviderConfig)]
struct Clock {
    #[field(label = "Zone")]
    zone: String,
}

#[provider]
impl Clock {
    pub async fn zone(&self) -> Result<String> {
        Ok(self.zone.clone())
    }
}

async fn context() -> TestContext {
    // No built-in providers page, nor its API: the application manages its
    // providers itself.
    let mut pages = AdminPagesConfig::default();
    pages.set(AdminPageId::Providers, AdminPageMode::Disabled);
    let config = OpenShopConfig::builder()
        .provider(Mailer::provider())
        .provider(Clock::provider())
        .builtin_pages(pages)
        .build()
        .expect("valid config");
    let options = TestOptions {
        encryption_key: Some("0123456789abcdef".repeat(4)),
        ..Default::default()
    };
    TestContext::with_options(config, options).await.expect("test context")
}

fn mailer(api_url: &str, api_key: &str) -> Mailer {
    Mailer {
        api_url: api_url.to_string(),
        api_key: api_key.to_string(),
        batch_size: Some(10),
    }
}

/// The values of a form, under the names of the fields of `Mailer`.
#[derive(Serialize)]
struct MailerForm {
    api_url: String,
    api_key: String,
}

#[openshop::test(context())]
async fn the_code_saves_what_the_disabled_page_no_longer_can(ctx: &TestContext) {
    let page = ctx.get("/api/providers").send().await.unwrap();
    assert_eq!(page.status(), 404);

    let store = Mailer::config_of(ctx);
    assert_eq!(store.status().await.unwrap(), Default::default());
    assert_eq!(
        store.save(mailer("https://mailer.test", "ml_live")).await.unwrap(),
        Saved::Saved
    );

    let status = store.status().await.unwrap();
    assert!(status.saved && status.configured);
    assert_eq!(status.value("api_url").as_deref(), Some("https://mailer.test"));
    // Under the name of the field, saved as `perBatch`.
    assert_eq!(status.value("batch_size").as_deref(), Some("10"));
    // The password is never given back, only that it is saved.
    assert_eq!(status.value("api_key"), None);
    assert!(status.has_secret("api_key"));
    assert_eq!(status.saved_secrets, ["api_key"]);

    // Encrypted at rest, as the page saves it.
    let stored: (String,) = sqlx::query_as("SELECT config::text FROM provider_configs WHERE provider_name = 'mailer'")
        .fetch_one(&ctx.db)
        .await
        .unwrap();
    assert!(!stored.0.contains("ml_live"), "{}", stored.0);

    let connectors = connectors(ctx).await;
    assert_eq!(
        Mailer::on(&connectors).sender().await.unwrap(),
        "https://mailer.test with ml_live"
    );
}

#[openshop::test(context())]
async fn a_refused_configuration_names_the_fields_as_a_form_does(ctx: &TestContext) {
    let store = Mailer::config_of(ctx);
    let Saved::Refused { message, fields } = store.save(mailer("not a url", "")).await.unwrap() else {
        panic!("saved");
    };
    // By the key they are saved under, the message too.
    assert_eq!(message, "Field \"apiKey\" is required");
    // The required fields first, then the rules of each field: as the page does.
    assert_eq!(fields, [("apiKey".to_string(), "Required".to_string())]);
    let Saved::Refused { message, fields } = store.save(mailer("not a url", "ml_live")).await.unwrap() else {
        panic!("saved");
    };
    assert_eq!(fields[0].0, "apiUrl");
    assert!(message.starts_with("Field \"apiUrl\""), "{message}");
    // A key given in clear is never in an error.
    let refused = Mailer {
        batch_size: Some(500),
        ..mailer("https://mailer.test", "ml_live")
    };
    let Saved::Refused { fields, .. } = store.save(refused).await.unwrap() else {
        panic!("saved");
    };
    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0].0, "perBatch");
    assert!(!store.status().await.unwrap().saved);
}

#[openshop::test(context())]
async fn a_form_keeps_the_saved_password_when_it_is_left_blank(ctx: &TestContext) {
    let store = Mailer::config_of(ctx);
    let form = |api_url: &str, api_key: &str| MailerForm {
        api_url: api_url.to_string(),
        api_key: api_key.to_string(),
    };
    // Nothing saved yet: a blank key is refused.
    let Saved::Refused { fields, .. } = store.save_form(&form("https://a.test", "")).await.unwrap() else {
        panic!("saved");
    };
    assert_eq!(fields[0].0, "apiKey");

    assert_eq!(
        store.save_form(&form("https://a.test", "ml_live")).await.unwrap(),
        Saved::Saved
    );
    assert_eq!(
        store.save_form(&form("https://b.test", "")).await.unwrap(),
        Saved::Saved
    );
    assert_eq!(
        Mailer::on(&connectors(ctx).await).sender().await.unwrap(),
        "https://b.test with ml_live"
    );

    // `save` replaces as a whole: an empty key is no key.
    let Saved::Refused { fields, .. } = store.save(mailer("https://c.test", "")).await.unwrap() else {
        panic!("saved");
    };
    assert_eq!(fields[0].0, "apiKey");

    // A value is under the key of its field or its name in the struct.
    assert_eq!(
        store
            .save_form(&json!({ "api_url": "https://d.test", "apiKey": "ml_live", "perBatch": 5 }))
            .await
            .unwrap(),
        Saved::Saved
    );
    assert_eq!(store.status().await.unwrap().value("batch_size").as_deref(), Some("5"));

    // A name the struct does not have is a mistake of the code.
    let error = store
        .save_form(&json!({ "api_url": "https://a.test", "apiToken": "x" }))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("no field `apiToken`"), "{error}");
    // Nor is a field given under the camel case of its name when it is saved
    // under another key.
    let error = store
        .save_form(&json!({ "api_url": "https://a.test", "batchSize": 5 }))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("no field `batchSize`"), "{error}");
}

#[openshop::test(context())]
async fn the_check_runs_the_checker_and_keeps_its_answer(ctx: &TestContext) {
    let store = Mailer::config_of(ctx);
    assert_eq!(
        store.save(mailer("https://mailer.test", "ml_test")).await.unwrap(),
        Saved::Saved
    );
    assert_eq!(store.check().await.unwrap(), Check::Refused);
    assert_eq!(store.status().await.unwrap().last_check_ok, Some(false));

    assert_eq!(
        store.save(mailer("https://mailer.test", "ml_live")).await.unwrap(),
        Saved::Saved
    );
    assert_eq!(store.check().await.unwrap(), Check::Ok);
    let status = store.status().await.unwrap();
    assert_eq!(status.last_check_ok, Some(true));
    assert!(status.last_checked_at.is_some());

    // A panic is a failure, and the last answer is kept.
    assert_eq!(
        store.save(mailer("https://panic.test", "ml_live")).await.unwrap(),
        Saved::Saved
    );
    let Check::Failed(message) = store.check().await.unwrap() else {
        panic!("answered");
    };
    assert_eq!(message, "checker panicked");
    assert_eq!(store.status().await.unwrap().last_check_ok, Some(true));

    // A provider without a checker cannot be checked.
    let clock = Clock::config_of(ctx);
    assert_eq!(clock.save(Clock { zone: "UTC".into() }).await.unwrap(), Saved::Saved);
    let error = clock.check().await.unwrap_err();
    assert!(error.to_string().contains("has no `#[checker]`"), "{error}");
}

#[openshop::test(context())]
async fn removing_forgets_the_configuration(ctx: &TestContext) {
    let store = Mailer::config_of(ctx);
    assert!(!store.remove().await.unwrap());
    assert_eq!(
        store.save(mailer("https://mailer.test", "ml_live")).await.unwrap(),
        Saved::Saved
    );
    assert!(store.remove().await.unwrap());
    assert!(!store.status().await.unwrap().saved);
    let error = Mailer::on(&connectors(ctx).await).sender().await.unwrap_err();
    assert!(format!("{error:#}").contains("apiUrl"), "{error:#}");
}

/// A context of a shop whose connectors already loaded a configuration.
struct Scope {
    app: OpenShop,
    shop: String,
    connectors: Connectors,
}

impl ShopScope for Scope {
    fn shop(&self) -> &str {
        &self.shop
    }

    fn shopify_app(&self) -> &str {
        "default"
    }

    fn app(&self) -> &OpenShop {
        &self.app
    }

    fn scope_connectors(&self) -> Option<&Connectors> {
        Some(&self.connectors)
    }
}

#[openshop::test(context())]
async fn the_connectors_of_the_context_read_the_configuration_saved_since(ctx: &TestContext) {
    let scope = Scope {
        app: ctx.app.clone(),
        shop: ctx.shop.clone(),
        connectors: connectors(ctx).await,
    };
    let store = Mailer::config_of(&scope);
    assert_eq!(
        store.save(mailer("https://a.test", "ml_live")).await.unwrap(),
        Saved::Saved
    );
    assert_eq!(
        Mailer::on(&scope.connectors).sender().await.unwrap(),
        "https://a.test with ml_live"
    );

    assert_eq!(
        store.save(mailer("https://b.test", "ml_live")).await.unwrap(),
        Saved::Saved
    );
    assert_eq!(
        Mailer::on(&scope.connectors).sender().await.unwrap(),
        "https://b.test with ml_live"
    );

    store.remove().await.unwrap();
    assert!(Mailer::on(&scope.connectors).sender().await.is_err());
}

#[tokio::test]
async fn a_provider_the_application_does_not_list_is_refused() {
    let config = OpenShopConfig::builder().build().expect("valid config");
    let ctx = TestContext::new(config).await.expect("test context");
    let error = Mailer::config_of(&ctx).status().await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("\"mailer\" is not in the `providers` of `app!`"),
        "{error}"
    );
    ctx.shutdown().await.unwrap();
}

// The page of an application that saves a provider from the `#[input]` of its
// action, as the guide shows it (#75).

#[derive(Debug, ProviderConfig)]
pub struct Warehouse {
    #[field(label = "API key", password, non_empty)]
    pub api_key: String,
    #[field(label = "API URL", url)]
    pub api_url: Option<String>,
}

#[provider]
impl Warehouse {
    pub async fn key(&self) -> Result<String> {
        Ok(self.api_key.clone())
    }
}

#[input]
#[derive(Debug)]
pub struct SaveWarehouse {
    pub api_key: Option<String>,
    pub api_url: Option<String>,
}

struct WarehousePage;

#[admin_page("/warehouse")]
impl WarehousePage {
    #[loader]
    async fn saved(ctx: AdminContext) -> Result<bool> {
        Ok(Warehouse::config_of(&ctx).status().await?.saved)
    }

    #[action]
    async fn save(ctx: AdminContext, input: SaveWarehouse) -> Result<Outcome> {
        Ok(match Warehouse::config_of(&ctx).save_form(&input).await? {
            Saved::Saved => Outcome::toast("Warehouse connected"),
            Saved::Refused { message, fields } => Outcome::invalid(fields).with_toast(message),
        })
    }

    #[view]
    fn view(saved: bool, form: FormState) -> Markup {
        let saving = form.of(Self::SAVE);
        html! {
            s-page heading=(if saved { "Connected" } else { "Not connected" }) {
                form method="post" {
                    input type="hidden" name="_action" value=(Self::SAVE);
                    s-password-field name="apiKey" label="API key" error=[saving.error("apiKey")] {}
                    s-url-field name="apiUrl" label="API URL" error=[saving.error("apiUrl")] {}
                    s-button type="submit" { "Save" }
                }
            }
        }
    }
}

static WAREHOUSE_APP: App = app! {
    providers: [Warehouse],
    admin_pages: [WarehousePage],
};

async fn warehouse_context() -> TestContext {
    let mut config = WAREHOUSE_APP.config().unwrap();
    let mut pages = AdminPagesConfig::default();
    pages.set(AdminPageId::Providers, AdminPageMode::Disabled);
    config.builtin_pages = Some(pages);
    let options = TestOptions {
        encryption_key: Some("0123456789abcdef".repeat(4)),
        server: ServerOptions {
            ui: UiSource::Server,
            dev: false,
        },
        ..Default::default()
    };
    TestContext::with_options(config, options).await.expect("test context")
}

#[openshop::test(warehouse_context())]
async fn the_input_of_an_action_saves_and_its_form_shows_the_refusals(ctx: &TestContext) {
    let input = SaveWarehouse {
        api_key: None,
        api_url: Some("https://warehouse.test".to_string()),
    };
    // Under the keys an `#[input]` gives its fields, `apiKey` and `apiUrl`.
    let Saved::Refused { message, fields } = Warehouse::config_of(ctx).save_form(&input).await.unwrap() else {
        panic!("saved");
    };
    assert_eq!(message, "Field \"apiKey\" is required");
    assert_eq!(fields, [("apiKey".to_string(), "Required".to_string())]);

    // The form of the page shows the refusal under the control it names.
    let refused = ctx
        .submit(
            "/warehouse",
            &[("_action", "save"), ("apiKey", ""), ("apiUrl", "not a url")],
        )
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), 422);
    let body = refused.text().await.unwrap();
    assert!(
        body.contains(r#"<s-password-field name="apiKey" label="API key" error="Required">"#),
        "{body}"
    );

    let saved = ctx
        .submit(
            "/warehouse",
            &[("_action", "save"), ("apiKey", "wh_live"), ("apiUrl", "")],
        )
        .send()
        .await
        .unwrap();
    assert_eq!(saved.status(), 200);
    assert_eq!(openshop::testing::toast(&saved).as_deref(), Some("Warehouse connected"));
    assert_eq!(Warehouse::on(&connectors(ctx).await).key().await.unwrap(), "wh_live");

    // A blank key keeps the one saved; a URL that is not one is refused under
    // its control.
    let refused = ctx
        .submit(
            "/warehouse",
            &[("_action", "save"), ("apiKey", ""), ("apiUrl", "not a url")],
        )
        .send()
        .await
        .unwrap();
    let body = refused.text().await.unwrap();
    assert!(body.contains(r#"name="apiUrl" label="API URL" error="#), "{body}");
    assert!(!body.contains(r#"name="apiKey" label="API key" error="#), "{body}");
}
