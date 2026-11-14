//! The admin pages in the language of the staff member: the languages of the
//! `locales/` folder of the application, the language of a request
//! (`ctx.locale`), and the texts of [`t!`](crate::t), in Shopify's locale-file
//! format.
//!
//! Shopify opens the app with the `locale` of the staff member, the language
//! of their admin (`de`, `de-CH`); `openshop.js` sends it again with each
//! request (`shopify.config.locale`), and the server keeps it for the staff
//! member's later requests. It is matched to the languages of `locales/`: the
//! same tag, then its primary subtag (`de-CH` is `de`), else the default file.
//! Without a `locales/` folder, the language is `admin_locale`.

use std::fmt::{Display, Write};
use std::future::Future;

/// The languages of the `locales/` folder of an application, which `app!`
/// reads when it compiles (see [`locales!`](crate::locales)): the default
/// one, the others, and the texts of the default file the others lack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Locales {
    /// The default language first.
    languages: &'static [&'static str],
    missing: &'static [(&'static str, &'static str)],
}

impl Locales {
    #[doc(hidden)]
    pub const fn __new(languages: &'static [&'static str], missing: &'static [(&'static str, &'static str)]) -> Self {
        Self { languages, missing }
    }

    /// The language of the `.default.json` file: `en` of `en.default.json`.
    pub fn default_language(&self) -> &'static str {
        self.languages.first().copied().unwrap_or("en")
    }

    /// Every language, the default one first.
    pub fn languages(&self) -> &'static [&'static str] {
        self.languages
    }

    /// The texts of the default file another file does not have, as
    /// `(language, key)`: `("fr", "home.title")`. [`t!`](crate::t) gives
    /// them in the default language;
    /// [`testing::assert_translated`](crate::testing::assert_translated)
    /// lists them.
    pub fn missing(&self) -> &'static [(&'static str, &'static str)] {
        self.missing
    }

    /// The language of the application for a staff member whose admin is in
    /// `locale`: the same tag, then its primary subtag (`de-CH` is `de`), else
    /// the default language. No locale is the default language.
    pub fn resolve(&self, locale: Option<&str>) -> &'static str {
        locale
            .and_then(|locale| matching(self.languages.iter().copied(), locale))
            .unwrap_or_else(|| self.default_language())
    }
}

/// The language of `languages` that is `wanted`, or its primary subtag. Tags
/// are compared without case.
fn matching<'a>(languages: impl Iterator<Item = &'a str> + Clone, wanted: &str) -> Option<&'a str> {
    let primary = wanted.split('-').next().unwrap_or(wanted);
    languages
        .clone()
        .find(|language| language.eq_ignore_ascii_case(wanted))
        .or_else(|| {
            languages
                .clone()
                .find(|language| language.eq_ignore_ascii_case(primary))
        })
}

tokio::task_local! {
    /// The language of the request a view, a loader or an action runs for.
    static LANGUAGE: String;
}

/// Runs `future` in `language`: what [`t!`](crate::t) reads.
pub(crate) async fn in_language<F: Future>(language: String, future: F) -> F::Output {
    LANGUAGE.scope(language, future).await
}

/// The language of the request being served, `ctx.locale`: `None` outside a
/// view, a loader, an action or an access rule of an admin page (in a flow,
/// or in a task the request spawned).
pub fn current_language() -> Option<String> {
    LANGUAGE.try_with(Clone::clone).ok()
}

/// The count a plural is chosen by: `t!("home.count", count = n)`, an
/// integer. It is the `{{ count }}` of the text, too.
pub trait Count: Display {
    /// Whether the text is the `one` form of a plural.
    fn is_one(&self) -> bool;
}

macro_rules! counts {
    ($($integer:ty),*) => {
        $(impl Count for $integer {
            fn is_one(&self) -> bool {
                *self == 1
            }
        })*
    };
}

counts!(i8, i16, i32, i64, i128, isize, u8, u16, u32, u64, u128, usize);

impl<T: Count + ?Sized> Count for &T {
    fn is_one(&self) -> bool {
        (**self).is_one()
    }
}

/// A text of a locale file, as [`t!`](crate::t) gives it.
#[doc(hidden)]
#[derive(Clone, Copy, Debug)]
pub enum __Text {
    Plain(&'static str),
    Plural {
        one: Option<&'static str>,
        other: &'static str,
    },
}

/// What [`t!`](crate::t) expands to: the text in the language of the
/// request, the one of its primary subtag, else the default one, the first of
/// `texts`; its form for `count`; its placeholders replaced by the values.
#[doc(hidden)]
pub fn __translate(
    texts: &[(&'static str, __Text)],
    count: Option<&dyn Count>,
    values: &[(&str, &dyn Display)],
) -> String {
    let language = current_language();
    let text = language
        .as_deref()
        .and_then(|language| matching(texts.iter().map(|(language, _)| *language), language))
        .and_then(|language| texts.iter().find(|(candidate, _)| *candidate == language))
        .or_else(|| texts.first())
        .map(|(_, text)| *text);
    let form = match text {
        None => return String::new(),
        Some(__Text::Plain(text)) => text,
        Some(__Text::Plural { one, other }) => match one {
            Some(one) if count.is_some_and(|count| count.is_one()) => one,
            _ => other,
        },
    };
    interpolate(form, count, values)
}

/// `text` with each `{{ name }}` replaced by its value; one without a value is
/// left as it is.
fn interpolate(text: &str, count: Option<&dyn Count>, values: &[(&str, &dyn Display)]) -> String {
    let mut written = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        written.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            written.push_str(&rest[start..]);
            return written;
        };
        let name = after[..end].trim();
        let value = values
            .iter()
            .find(|(given, _)| *given == name)
            .map(|(_, value)| *value)
            .or_else(|| count.filter(|_| name == "count").map(|count| count as &dyn Display));
        match value {
            Some(value) => {
                let _ = write!(written, "{value}");
            }
            None => written.push_str(&rest[start..start + 2 + end + 2]),
        }
        rest = &after[end + 2..];
    }
    written.push_str(rest);
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCALES: Locales = Locales::__new(&["en", "fr", "de", "pt-BR"], &[]);

    #[test]
    fn a_staff_locale_is_matched_to_the_languages_of_the_application() {
        for (locale, expected) in [
            (Some("fr"), "fr"),
            (Some("FR"), "fr"),
            (Some("pt-BR"), "pt-BR"),
            // The primary subtag.
            (Some("de-CH"), "de"),
            (Some("fr-CA"), "fr"),
            // A language the application does not have, and none at all: the default file.
            (Some("ja"), "en"),
            (Some("pt-PT"), "en"),
            (None, "en"),
        ] {
            assert_eq!(LOCALES.resolve(locale), expected, "{locale:?}");
        }
    }

    const TITLE: &[(&str, __Text)] = &[
        ("en", __Text::Plain("Stock watch")),
        ("fr", __Text::Plain("Veille des stocks")),
    ];

    async fn translated(language: &str, texts: &[(&'static str, __Text)], count: Option<&dyn Count>) -> String {
        in_language(language.to_string(), async { __translate(texts, count, &[]) }).await
    }

    #[tokio::test]
    async fn a_text_is_in_the_language_of_the_request_else_in_the_default_one() {
        assert_eq!(translated("fr", TITLE, None).await, "Veille des stocks");
        assert_eq!(translated("fr-CA", TITLE, None).await, "Veille des stocks");
        // A file without the text: the default file.
        assert_eq!(translated("de", TITLE, None).await, "Stock watch");
        // Outside a request: the default file.
        assert_eq!(__translate(TITLE, None, &[]), "Stock watch");
    }

    #[tokio::test]
    async fn a_plural_is_chosen_by_its_count() {
        const VARIANTS: &[(&str, __Text)] = &[
            (
                "en",
                __Text::Plural {
                    one: Some("1 variant runs low"),
                    other: "{{ count }} variants run low",
                },
            ),
            (
                "ja",
                __Text::Plural {
                    one: None,
                    other: "{{count}} 件",
                },
            ),
        ];
        assert_eq!(translated("en", VARIANTS, Some(&1)).await, "1 variant runs low");
        assert_eq!(translated("en", VARIANTS, Some(&0usize)).await, "0 variants run low");
        assert_eq!(translated("en", VARIANTS, Some(&12i64)).await, "12 variants run low");
        // A language without `one`.
        assert_eq!(translated("ja", VARIANTS, Some(&1)).await, "1 件");
    }

    #[test]
    fn placeholders_are_replaced_by_their_values() {
        let name = "Ada";
        assert_eq!(
            interpolate(
                "Hello {{ name }}, {{count}} left, {{ unknown }} {{ name",
                Some(&3),
                &[("name", &name)]
            ),
            "Hello Ada, 3 left, {{ unknown }} {{ name"
        );
    }
}
