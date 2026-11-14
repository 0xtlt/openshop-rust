//! The admin pages of a test in a language: a request in the locale of a
//! staff member, and the texts the locale files lack.

use reqwest::RequestBuilder;

use crate::declare::App;
use crate::server::locale::LOCALE_HEADER;

/// A request of the admin pages in the locale of the staff member, as
/// `openshop.js` sends it (`shopify.config.locale`):
/// `ctx.page("/").locale("de").send()`, `ctx.submit(…).locale("de-CH")`.
/// The server keeps it for the staff member's later requests, as it does for
/// the `locale` Shopify opens the app with, `ctx.page("/?locale=de")`.
///
/// ```no_run
/// # async fn example(ctx: &openshop::testing::TestContext) -> openshop::Result<()> {
/// use openshop::testing::WithLocale;
///
/// let page = ctx.page("/").locale("fr").send().await?.text().await?;
/// assert!(page.contains(r#"<html lang="fr""#));
/// # Ok(())
/// # }
/// ```
pub trait WithLocale {
    fn locale(self, locale: &str) -> Self;
}

impl WithLocale for RequestBuilder {
    fn locale(self, locale: &str) -> Self {
        self.header(LOCALE_HEADER, locale)
    }
}

/// Panics with the texts of the default file of `locales/` another file
/// lacks, which [`t!`](crate::t) gives in the default language: a test of
/// the application, `assert_translated(&APP)`, keeps its translations whole.
/// An application without `locales/` has none.
pub fn assert_translated(app: &App) {
    let config = app
        .config()
        .unwrap_or_else(|error| panic!("[openshop] The application: {error:#}"));
    let Some(locales) = config.locales else {
        return;
    };
    let missing: Vec<String> = locales
        .missing()
        .iter()
        .map(|(language, key)| format!("{language}: {key}"))
        .collect();
    assert!(
        missing.is_empty(),
        "[openshop] Texts of locales/{}.default.json missing from another file, written in the default language:\n  {}",
        locales.default_language(),
        missing.join("\n  ")
    );
}
