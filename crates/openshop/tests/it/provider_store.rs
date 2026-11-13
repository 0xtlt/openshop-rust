//! `Mailer::config_of(&ctx)`: the configuration of a provider managed by the
//! code of the application, with the built-in providers page disabled.

use crate::common::shop_connectors as connectors;
use openshop::providers::{Check, Saved};
use openshop::testing::{TestContext, TestOptions};
use openshop::{
    AdminPageId, AdminPageMode, AdminPagesConfig, Connectors, OpenShop, OpenShopConfig, ProviderConfig, Result,
    ShopScope, json, provider,
};
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
async fn a_refused_configuration_names_the_fields_of_the_struct(ctx: &TestContext) {
    let store = Mailer::config_of(ctx);
    let Saved::Refused { message, fields } = store.save(mailer("not a url", "")).await.unwrap() else {
        panic!("saved");
    };
    assert!(message.starts_with("Field \""), "{message}");
    // The required fields first, then the rules of each field: as the page does.
    let names: Vec<&str> = fields.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["api_key"]);
    let Saved::Refused { fields, .. } = store.save(mailer("not a url", "ml_live")).await.unwrap() else {
        panic!("saved");
    };
    assert_eq!(fields[0].0, "api_url");
    // A key given in clear is never in an error.
    let refused = Mailer {
        batch_size: Some(500),
        ..mailer("https://mailer.test", "ml_live")
    };
    let Saved::Refused { fields, .. } = store.save(refused).await.unwrap() else {
        panic!("saved");
    };
    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0].0, "batch_size");
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
    assert_eq!(fields[0].0, "api_key");

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
    assert_eq!(fields[0].0, "api_key");

    // A name the struct does not have is a mistake of the code.
    let error = store
        .save_form(&json!({ "api_url": "https://a.test", "apiKey": "x" }))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("no field `apiKey`"), "{error}");
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
