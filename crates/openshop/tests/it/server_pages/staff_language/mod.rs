//! The admin pages in the language of the staff member: `locales/` next to
//! the `Cargo.toml` of this folder (English by default, French, German),
//! `ctx.locale`, `t!`, and what openshop writes itself, `<html lang>` and its
//! texts, in that language.

use maud::html;
use openshop::admin::AdminText;
use openshop::prelude::*;
use openshop::testing::{AdminTestOptions, TestContext, WithLocale, admin_context, assert_translated, toast};
use reqwest::Method;

use super::{NewReviewPage, send};

struct StockPage;

#[admin_page("/")]
impl StockPage {
    /// The language of the request, and a text of a loader.
    #[loader]
    async fn greeting(ctx: AdminContext) -> Result<String> {
        Ok(format!("{}: {}", ctx.locale, t!("home.hello", name = "Ada")))
    }

    #[action]
    async fn save(ctx: AdminContext) -> Result<Outcome> {
        Ok(Outcome::toast(t!("saved", shop = ctx.shop)))
    }

    #[view]
    fn view(greeting: String) -> Markup {
        html! {
            s-page heading=(t!("home.title")) {
                s-paragraph { (greeting) }
                s-paragraph { (t!("home.low", count = 1)) }
                s-paragraph { (t!("home.low", count = 3usize)) }
                // A text French lacks.
                s-paragraph { (t!("home.since", version = "1.2")) }
            }
        }
    }
}

/// German is the language of `admin_locale`, whose texts the application gives.
static APP: App = app! {
    admin_pages: [StockPage, NewReviewPage],
    admin_locale: "de",
    admin_texts: |text: &AdminText| match text {
        AdminText::Copied => Some("Kopiert".to_string()),
        _ => None,
    },
};

/// The page as staff user `sub` opens it, in `locale` when it is given.
async fn page_of(ctx: &TestContext, sub: &str, locale: Option<&str>) -> String {
    let request = ctx
        .request_as(Method::GET, "/", &ctx.shop, sub)
        .header("x-openshop-fetch", "1");
    let request = match locale {
        Some(locale) => request.locale(locale),
        None => request,
    };
    let page = send(request).await;
    assert_eq!(page.status, 200, "{}", page.body);
    page.body
}

/// The texts `openshop.js` reads from `<html>`, as maud escapes them.
fn copied(text: &str) -> String {
    format!("&quot;copied&quot;:&quot;{text}&quot;")
}

#[openshop::test(APP)]
async fn the_pages_are_in_the_language_of_the_staff_member(ctx: &TestContext) {
    for (sub, locale, lang, texts, copy) in [
        (
            "fr",
            Some("fr"),
            "fr",
            [
                "Veille des stocks",
                "fr: Bonjour Ada",
                "1 variante est presque épuisée",
                "3 variantes sont presque épuisées",
                // What French lacks is in the default language.
                "Since version 1.2",
            ],
            "Copié",
        ),
        // The primary subtag; openshop's texts are the application's, German being `admin_locale`.
        (
            "de",
            Some("de-CH"),
            "de",
            [
                "Lagerwache",
                "de: Hallo Ada",
                "1 Variante wird knapp",
                "3 Varianten werden knapp",
                "Seit Version 1.2",
            ],
            "Kopiert",
        ),
        // A language the application does not have, and none: the default file.
        (
            "ja",
            Some("ja-JP"),
            "en",
            [
                "Stock watch",
                "en: Hello Ada",
                "1 variant runs low",
                "3 variants run low",
                "Since version 1.2",
            ],
            "Copied",
        ),
        (
            "none",
            None,
            "en",
            [
                "Stock watch",
                "en: Hello Ada",
                "1 variant runs low",
                "3 variants run low",
                "Since version 1.2",
            ],
            "Copied",
        ),
    ] {
        let page = page_of(ctx, sub, locale).await;
        assert!(
            page.contains(&format!(r#"<html lang="{lang}" data-openshop-texts=""#)),
            "{locale:?}: {page}"
        );
        assert!(page.contains(&copied(copy)), "{locale:?}: {page}");
        assert!(
            page.contains(&format!(r#"<s-page heading="{}">"#, texts[0])),
            "{locale:?}: {page}"
        );
        for text in &texts[1..] {
            assert!(
                page.contains(&format!("<s-paragraph>{text}</s-paragraph>")),
                "{locale:?} {text}: {page}"
            );
        }
    }
}

#[openshop::test(APP)]
async fn the_language_of_the_launch_is_kept_for_the_later_requests(ctx: &TestContext) {
    // Shopify opens the app with the staff member's `locale`.
    let launch = send(ctx.page("/?locale=fr")).await;
    assert!(launch.body.contains(r#"<html lang="fr""#), "{}", launch.body);

    // Then the requests no longer say it: a page, a form, its refused fields, a loader.
    let page = send(ctx.page("/")).await;
    assert!(
        page.body.contains(r#"<s-page heading="Veille des stocks">"#),
        "{}",
        page.body
    );
    let saved = ctx.submit("/", &[("_action", "save")]).send().await.unwrap();
    assert_eq!(toast(&saved).as_deref(), Some("Enregistré pour test.myshopify.com"));
    let refused = send(ctx.submit("/reviews/new", &[("_action", "saveReview"), ("title", "ab")])).await;
    assert!(
        refused
            .body
            .contains(r#"value="ab" error="Doit contenir au moins 3 caractères""#),
        "{}",
        refused.body
    );
    let greeting: String = ctx.call_loader("/", "greeting", ()).await.unwrap();
    assert_eq!(greeting, "fr: Bonjour Ada");

    // The bounce page keeps the query of the page it loads again.
    let bounce = send(
        ctx.http
            .get(ctx.url_for("/auth/session-token?shopify-reload=%2F&locale=fr")),
    )
    .await;
    assert!(bounce.body.contains(r#"<html lang="fr">"#), "{}", bounce.body);

    // A context of a test in a locale.
    let context = admin_context(
        &ctx.app,
        AdminTestOptions {
            locale: Some("de-AT".to_string()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(context.locale, "de");
}

#[test]
fn the_texts_another_file_lacks_are_listed() {
    let failure = std::panic::catch_unwind(|| assert_translated(&APP)).expect_err("French lacks a text");
    let message = failure.downcast_ref::<String>().unwrap();
    assert!(message.contains("\n  fr: home.since"), "{message}");
    assert!(!message.contains("de:"), "{message}");
    let config = APP.config().unwrap();
    let locales = config.locales.unwrap();
    assert_eq!(locales.languages(), ["en", "de", "fr"]);
    assert_eq!(locales.missing(), [("fr", "home.since")]);
}
