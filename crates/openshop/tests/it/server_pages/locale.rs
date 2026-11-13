//! `admin_locale` and `admin_texts`: `<html lang>` and the texts openshop
//! writes on the admin pages, in French, in the application's words, and in
//! English by default.

use maud::html;
use openshop::admin::{AdminText, FieldError};
use openshop::prelude::*;
use openshop::server::ServerOptions;
use openshop::testing::{TestContext, TestOptions};
use openshop::{OpenShopConfig, UiSource};
use reqwest::{Method, StatusCode};

use super::{HomePage, NewReviewPage, PrivatePage, fetch, form, request, send};

const SHOP: &str = "server-locale.myshopify.com";

/// A page whose loader fails, and an action that fails, with errors the
/// merchant may not read.
struct BrokenPage;

#[admin_page("/broken")]
impl BrokenPage {
    #[loader]
    async fn broken(_ctx: AdminContext) -> Result<String> {
        bail!("the database is down")
    }

    #[view]
    fn view(broken: String) -> Markup {
        html! { s-page { (broken) } }
    }
}

struct FailingPage;

#[admin_page("/failing")]
impl FailingPage {
    #[loader]
    async fn failing(_ctx: AdminContext) -> Result<String> {
        Ok("failing".to_string())
    }

    #[action]
    async fn explode(_ctx: AdminContext) -> Result<Outcome> {
        bail!("the database is down")
    }

    #[view]
    fn view(failing: String) -> Markup {
        html! { s-page { (failing) } }
    }
}

static FRENCH: App = app! {
    admin_pages: [HomePage, NewReviewPage, PrivatePage, BrokenPage, FailingPage],
    admin_locale: "fr",
};

/// German: no built-in table, the application's texts, English for the others.
static GERMAN: App = app! {
    admin_pages: [NewReviewPage, BrokenPage],
    admin_locale: "de",
    admin_texts: |text: &AdminText| match text {
        AdminText::Field(FieldError::Required) => Some("Pflichtfeld".to_string()),
        AdminText::Field(FieldError::MinLength(minimum)) => Some(format!("Mindestens {minimum} Zeichen")),
        AdminText::PageUnavailable => Some("Seite nicht verfügbar".to_string()),
        AdminText::RequestFailed => Some("Etwas ist schiefgelaufen.".to_string()),
        _ => None,
    },
};

static ENGLISH: App = app! {
    admin_pages: [NewReviewPage, BrokenPage],
};

async fn context(config: OpenShopConfig) -> TestContext {
    TestContext::with_options(
        config,
        TestOptions {
            shop: SHOP.to_string(),
            access_token: Some("offline-token".to_string()),
            server: ServerOptions {
                ui: UiSource::Server,
                dev: false,
            },
            ..Default::default()
        },
    )
    .await
    .expect("test context")
}

/// The texts `openshop.js` reads from `<html>`, as maud escapes them.
fn script_text(key: &str, text: &str) -> String {
    format!("&quot;{key}&quot;:&quot;{text}&quot;")
}

#[tokio::test]
async fn the_admin_pages_speak_the_admin_locale() {
    let ctx = context(FRENCH.config().unwrap()).await;

    // `<html lang>`, and the toasts of the script.
    let page = send(fetch(&ctx, Method::GET, "/reviews/new")).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    assert!(
        page.body.contains(r#"<html lang="fr" data-openshop-texts=""#),
        "{}",
        page.body
    );
    for (key, text) in [
        ("error", "Une erreur s’est produite. Réessayez."),
        ("copied", "Copié"),
        ("copyFailed", "Impossible de copier"),
    ] {
        assert!(page.body.contains(&script_text(key, text)), "{key}: {}", page.body);
    }

    // The errors of the fields the input schema refuses.
    let refused = send(form(
        fetch(&ctx, Method::POST, "/reviews/new"),
        "_action=saveReview&title=ab&rating=",
    ))
    .await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        refused
            .body
            .contains(r#"value="ab" error="Doit contenir au moins 3 caractères""#),
        "{}",
        refused.body
    );
    assert!(
        refused.body.contains(r#"value="" error="Obligatoire""#),
        "{}",
        refused.body
    );
    let wrong_type = send(form(
        fetch(&ctx, Method::POST, "/reviews/new"),
        "_action=saveReview&title=abc&rating=lots",
    ))
    .await;
    assert!(
        wrong_type
            .body
            .contains(r#"value="lots" error="Doit être un nombre entier""#),
        "{}",
        wrong_type.body
    );
    let too_high = send(form(
        fetch(&ctx, Method::POST, "/reviews/new"),
        "_action=saveReview&title=abc&rating=300",
    ))
    .await;
    assert!(
        too_high
            .body
            .contains(r#"value="300" error="Doit être inférieur ou égal à 255""#),
        "{}",
        too_high.body
    );
    // What the action says stays the action's.
    let taken = send(form(
        fetch(&ctx, Method::POST, "/reviews/new"),
        "_action=saveReview&title=taken&rating=4",
    ))
    .await;
    assert!(taken.body.contains(r#"error="Already taken""#), "{}", taken.body);
    // A view that does not read the form: the toast is the first field and its error.
    let home = send(form(fetch(&ctx, Method::POST, "/"), "_action=setHidden&hidden=maybe")).await;
    assert_eq!(home.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        home.header("x-openshop-toast-error"),
        Some("hidden%3A%20Doit%20%C3%AAtre%20vrai%20ou%20faux")
    );

    // A loader that failed.
    let broken = send(fetch(&ctx, Method::GET, "/broken")).await;
    assert_eq!(broken.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        broken.body.contains(r#"<s-page heading="Page indisponible">"#),
        "{}",
        broken.body
    );
    assert!(
        broken.body.contains("Cette page n’a pas pu être affichée."),
        "{}",
        broken.body
    );
    // A page whose access rule says no.
    let private = send(fetch(&ctx, Method::GET, "/private")).await;
    assert_eq!(private.status, StatusCode::NOT_FOUND);
    assert!(private.body.contains("Cette page est désactivée."), "{}", private.body);
    // An action that failed.
    let failed = send(form(fetch(&ctx, Method::POST, "/failing"), "_action=explode")).await;
    assert_eq!(failed.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        failed.header("x-openshop-toast-error"),
        Some("Erreur%20interne%20du%20serveur")
    );

    // The bounce page too.
    let bounce = send(request(&ctx, Method::GET, "/auth/session-token?shopify-reload=%2F")).await;
    assert_eq!(bounce.status, StatusCode::OK);
    assert!(bounce.body.contains(r#"<html lang="fr">"#), "{}", bounce.body);
}

#[tokio::test]
async fn the_texts_of_the_application_come_before_the_tables() {
    let ctx = context(GERMAN.config().unwrap()).await;

    let page = send(fetch(&ctx, Method::GET, "/reviews/new")).await;
    assert!(page.body.contains(r#"<html lang="de""#), "{}", page.body);
    assert!(
        page.body.contains(&script_text("error", "Etwas ist schiefgelaufen.")),
        "{}",
        page.body
    );
    // English for what neither the application nor a table says.
    assert!(page.body.contains(&script_text("copied", "Copied")), "{}", page.body);

    let refused = send(form(
        fetch(&ctx, Method::POST, "/reviews/new"),
        "_action=saveReview&title=ab&rating=",
    ))
    .await;
    assert!(
        refused.body.contains(r#"value="ab" error="Mindestens 3 Zeichen""#),
        "{}",
        refused.body
    );
    assert!(
        refused.body.contains(r#"value="" error="Pflichtfeld""#),
        "{}",
        refused.body
    );
    let wrong_type = send(form(
        fetch(&ctx, Method::POST, "/reviews/new"),
        "_action=saveReview&title=abc&rating=lots",
    ))
    .await;
    assert!(
        wrong_type.body.contains(r#"error="Must be a whole number""#),
        "{}",
        wrong_type.body
    );

    let broken = send(fetch(&ctx, Method::GET, "/broken")).await;
    assert!(
        broken.body.contains(r#"<s-page heading="Seite nicht verfügbar">"#),
        "{}",
        broken.body
    );
    assert!(
        broken.body.contains("This page could not be displayed."),
        "{}",
        broken.body
    );
}

#[tokio::test]
async fn the_admin_is_in_english_by_default() {
    let ctx = context(ENGLISH.config().unwrap()).await;

    let page = send(fetch(&ctx, Method::GET, "/reviews/new")).await;
    assert!(
        page.body.contains(r#"<html lang="en" data-openshop-texts=""#),
        "{}",
        page.body
    );
    for (key, text) in [
        ("error", "Something went wrong. Try again."),
        ("copied", "Copied"),
        ("copyFailed", "Could not copy"),
    ] {
        assert!(page.body.contains(&script_text(key, text)), "{key}: {}", page.body);
    }
    let broken = send(fetch(&ctx, Method::GET, "/broken")).await;
    assert!(
        broken.body.contains(r#"<s-page heading="Page unavailable">"#),
        "{}",
        broken.body
    );
    assert!(
        broken.body.contains("This page could not be displayed."),
        "{}",
        broken.body
    );
}

#[test]
fn an_admin_locale_is_a_language_tag() {
    let error = OpenShopConfig::builder()
        .admin_locale("fr_FR")
        .build()
        .err()
        .expect("refused")
        .to_string();
    assert!(error.contains("admin_locale: `fr_FR` is not a language tag"), "{error}");
    let config = OpenShopConfig::builder().admin_locale(" fr-CA ").build().unwrap();
    assert_eq!(config.admin_lang(), "fr-CA");
    assert_eq!(config.admin_text(&AdminText::Copied), "Copié");
    // An empty value, as an unset variable gives, is English.
    let config = OpenShopConfig::builder().admin_locale("").build().unwrap();
    assert_eq!(config.admin_lang(), "en");
}
