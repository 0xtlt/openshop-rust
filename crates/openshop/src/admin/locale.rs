//! The language of the texts the admin pages show that openshop writes
//! itself: the errors of the fields an input schema refuses, the page of a
//! loader that failed, the toasts of `openshop.js`. `admin_locale` picks a
//! built-in table, English when it has none; `admin_texts` gives the
//! application's own.

use std::sync::Arc;

/// The texts of the application, set with `admin_texts`: `None` for a text it
/// leaves to the table of its `admin_locale`.
pub type AdminTextsFn = Arc<dyn Fn(&AdminText) -> Option<String> + Send + Sync>;

/// A text of the admin pages that openshop writes itself, which an
/// application translates with `admin_texts`. Each one says its English text.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum AdminText {
    /// The error of a field the input schema of an action refused.
    Field(FieldError),
    /// The heading of a page whose loader or view failed: `Page unavailable`.
    PageUnavailable,
    /// What that page says: `This page could not be displayed.`
    PageUnavailableMessage,
    /// A page whose access rule said no, or that is not there: `This page is
    /// disabled.`
    PageDisabled,
    /// The error toast of an action that failed with an error the merchant
    /// may not read: `Internal server error`.
    ActionFailed,
    /// The error toast of `openshop.js` when a request failed without an
    /// answer it can show: `Something went wrong. Try again.`
    RequestFailed,
    /// The toast of a copy button: `Copied`.
    Copied,
    /// The toast of a copy button the browser refused: `Could not copy`.
    CopyFailed,
}

/// What the input schema of an action says of the value of a field.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum FieldError {
    /// Missing, or empty where text is required: `Required`.
    Required,
    /// Not a whole number: `Must be a whole number`.
    WholeNumber,
    /// Not a number: `Must be a number`.
    Number,
    /// Neither `true` nor `false`: `Must be true or false`.
    Boolean,
    /// Below `minimum`, the bound as the schema writes it: `Must be at least 1`.
    AtLeast(String),
    /// Above `maximum`: `Must be at most 10`.
    AtMost(String),
    /// Not above `exclusiveMinimum`: `Must be more than 0`.
    MoreThan(String),
    /// Not below `exclusiveMaximum`: `Must be less than 2.5`.
    LessThan(String),
    /// Shorter than `minLength`, above 1: `Must be at least 3 characters`.
    MinLength(u64),
    /// Longer than `maxLength`: `Must be at most 1 character`.
    MaxLength(u64),
    /// Not one of the values of `enum`, listed as the schema writes them:
    /// `Must be one of "a", "b"`.
    OneOf(String),
    /// Anything else, as the schema says it: `must be string`.
    Other(String),
}

impl FieldError {
    /// The error the input schema gives a property: `is required`,
    /// `must be >= 1`, `must be integer or null`.
    pub(crate) fn from_schema(error: &str) -> Self {
        let error = error.strip_suffix(" or null").unwrap_or(error);
        let bound = |prefix: &str| {
            error
                .strip_prefix(prefix)
                .filter(|bound| bound.parse::<f64>().is_ok())
                .map(str::to_string)
        };
        let length = |prefix: &str| {
            error
                .strip_prefix(prefix)
                .and_then(|rest| rest.strip_suffix(" characters"))
                .and_then(|length| length.parse::<u64>().ok())
        };
        if let Some(minimum) = bound("must be >= ") {
            return Self::AtLeast(minimum);
        }
        if let Some(maximum) = bound("must be <= ") {
            return Self::AtMost(maximum);
        }
        if let Some(minimum) = bound("must be > ") {
            return Self::MoreThan(minimum);
        }
        if let Some(maximum) = bound("must be < ") {
            return Self::LessThan(maximum);
        }
        if let Some(minimum) = length("must be at least ") {
            return if minimum <= 1 {
                Self::Required
            } else {
                Self::MinLength(minimum)
            };
        }
        if let Some(maximum) = length("must be at most ") {
            return Self::MaxLength(maximum);
        }
        if let Some(values) = error.strip_prefix("must be one of ") {
            return Self::OneOf(values.to_string());
        }
        match error {
            "is required" => Self::Required,
            "must be integer" => Self::WholeNumber,
            "must be number" => Self::Number,
            "must be boolean" => Self::Boolean,
            _ => Self::Other(error.to_string()),
        }
    }
}

/// Whether `tag` is a language tag `<html lang>` takes: `fr`, `fr-CA`,
/// `zh-Hant-TW`.
pub(crate) fn is_language_tag(tag: &str) -> bool {
    let mut subtags = tag.split('-');
    let primary = subtags.next().unwrap_or_default();
    (2..=3).contains(&primary.len())
        && primary.bytes().all(|byte| byte.is_ascii_alphabetic())
        && subtags
            .all(|subtag| (1..=8).contains(&subtag.len()) && subtag.bytes().all(|byte| byte.is_ascii_alphanumeric()))
}

/// The text in `language`, the language of a request: the application's
/// `texts` when it is the language of `admin_locale`, `default`, whose texts
/// they are, then the built-in table of `language`; else the texts of
/// `default`, then English.
pub(crate) fn admin_text(language: &str, default: &str, texts: Option<&AdminTextsFn>, text: &AdminText) -> String {
    let own = || texts.and_then(|texts| texts(text));
    let first = if same_language(language, default) {
        own().or_else(|| builtin(language, text))
    } else {
        builtin(language, text).or_else(own)
    };
    first
        .or_else(|| builtin(default, text))
        .unwrap_or_else(|| english(text))
}

/// Whether two tags are the same language: `fr-CA` and `fr`.
fn same_language(one: &str, other: &str) -> bool {
    let primary = |tag: &str| tag.split('-').next().unwrap_or_default().to_ascii_lowercase();
    primary(one) == primary(other)
}

/// The built-in table of the language of `tag`, by its first subtag:
/// `fr-CA` is French. `None` for a language or a text it does not have.
fn builtin(tag: &str, text: &AdminText) -> Option<String> {
    let language = tag.split('-').next().unwrap_or_default().to_ascii_lowercase();
    match language.as_str() {
        "en" => Some(english(text)),
        "fr" => french(text),
        _ => None,
    }
}

fn english(text: &AdminText) -> String {
    let field = match text {
        AdminText::Field(field) => field,
        AdminText::PageUnavailable => return "Page unavailable".to_string(),
        AdminText::PageUnavailableMessage => return "This page could not be displayed.".to_string(),
        AdminText::PageDisabled => return "This page is disabled.".to_string(),
        AdminText::ActionFailed => return "Internal server error".to_string(),
        AdminText::RequestFailed => return "Something went wrong. Try again.".to_string(),
        AdminText::Copied => return "Copied".to_string(),
        AdminText::CopyFailed => return "Could not copy".to_string(),
    };
    match field {
        FieldError::Required => "Required".to_string(),
        FieldError::WholeNumber => "Must be a whole number".to_string(),
        FieldError::Number => "Must be a number".to_string(),
        FieldError::Boolean => "Must be true or false".to_string(),
        FieldError::AtLeast(minimum) => format!("Must be at least {minimum}"),
        FieldError::AtMost(maximum) => format!("Must be at most {maximum}"),
        FieldError::MoreThan(minimum) => format!("Must be more than {minimum}"),
        FieldError::LessThan(maximum) => format!("Must be less than {maximum}"),
        FieldError::MinLength(minimum) => format!("Must be at least {minimum} characters"),
        FieldError::MaxLength(1) => "Must be at most 1 character".to_string(),
        FieldError::MaxLength(maximum) => format!("Must be at most {maximum} characters"),
        FieldError::OneOf(values) => format!("Must be one of {values}"),
        FieldError::Other(error) => {
            let mut letters = error.chars();
            letters
                .next()
                .map(|first| first.to_uppercase().chain(letters).collect())
                .unwrap_or_default()
        }
    }
}

fn french(text: &AdminText) -> Option<String> {
    // A decimal number is written with a comma: `2,5`.
    let number = |bound: &str| bound.replace('.', ",");
    let field = match text {
        AdminText::Field(field) => field,
        AdminText::PageUnavailable => return Some("Page indisponible".to_string()),
        AdminText::PageUnavailableMessage => return Some("Cette page n’a pas pu être affichée.".to_string()),
        AdminText::PageDisabled => return Some("Cette page est désactivée.".to_string()),
        AdminText::ActionFailed => return Some("Erreur interne du serveur".to_string()),
        AdminText::RequestFailed => return Some("Une erreur s’est produite. Réessayez.".to_string()),
        AdminText::Copied => return Some("Copié".to_string()),
        AdminText::CopyFailed => return Some("Impossible de copier".to_string()),
    };
    Some(match field {
        FieldError::Required => "Obligatoire".to_string(),
        FieldError::WholeNumber => "Doit être un nombre entier".to_string(),
        FieldError::Number => "Doit être un nombre".to_string(),
        FieldError::Boolean => "Doit être vrai ou faux".to_string(),
        FieldError::AtLeast(minimum) => format!("Doit être supérieur ou égal à {}", number(minimum)),
        FieldError::AtMost(maximum) => format!("Doit être inférieur ou égal à {}", number(maximum)),
        FieldError::MoreThan(minimum) => format!("Doit être supérieur à {}", number(minimum)),
        FieldError::LessThan(maximum) => format!("Doit être inférieur à {}", number(maximum)),
        FieldError::MinLength(minimum) => format!("Doit contenir au moins {minimum} caractères"),
        FieldError::MaxLength(1) => "Doit contenir au plus 1 caractère".to_string(),
        FieldError::MaxLength(maximum) => format!("Doit contenir au plus {maximum} caractères"),
        FieldError::OneOf(values) => format!("Doit être l’une de ces valeurs\u{a0}: {values}"),
        FieldError::Other(_) => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(error: &str) -> AdminText {
        AdminText::Field(FieldError::from_schema(error))
    }

    #[test]
    fn reads_what_the_input_schema_says_of_a_field() {
        for (error, expected) in [
            ("is required", FieldError::Required),
            ("must be at least 1 characters", FieldError::Required),
            ("must be at least 3 characters", FieldError::MinLength(3)),
            ("must be at most 1 characters", FieldError::MaxLength(1)),
            ("must be integer or null", FieldError::WholeNumber),
            ("must be number", FieldError::Number),
            ("must be boolean", FieldError::Boolean),
            ("must be >= 1", FieldError::AtLeast("1".into())),
            ("must be <= 10", FieldError::AtMost("10".into())),
            ("must be > 0", FieldError::MoreThan("0".into())),
            ("must be < 2.5", FieldError::LessThan("2.5".into())),
            ("must be one of \"a\", \"b\"", FieldError::OneOf("\"a\", \"b\"".into())),
            ("must be string", FieldError::Other("must be string".into())),
            ("must be >= x", FieldError::Other("must be >= x".into())),
        ] {
            assert_eq!(FieldError::from_schema(error), expected, "{error}");
        }
    }

    #[test]
    fn every_text_in_english_and_in_french() {
        for (text, en, fr) in [
            (field("is required"), "Required", "Obligatoire"),
            (
                field("must be at least 3 characters"),
                "Must be at least 3 characters",
                "Doit contenir au moins 3 caractères",
            ),
            (
                field("must be at most 1 characters"),
                "Must be at most 1 character",
                "Doit contenir au plus 1 caractère",
            ),
            (
                field("must be at most 80 characters"),
                "Must be at most 80 characters",
                "Doit contenir au plus 80 caractères",
            ),
            (
                field("must be integer"),
                "Must be a whole number",
                "Doit être un nombre entier",
            ),
            (field("must be number"), "Must be a number", "Doit être un nombre"),
            (
                field("must be boolean"),
                "Must be true or false",
                "Doit être vrai ou faux",
            ),
            (
                field("must be >= 1"),
                "Must be at least 1",
                "Doit être supérieur ou égal à 1",
            ),
            (
                field("must be <= 1.5"),
                "Must be at most 1.5",
                "Doit être inférieur ou égal à 1,5",
            ),
            (field("must be > 0"), "Must be more than 0", "Doit être supérieur à 0"),
            (
                field("must be < 2.5"),
                "Must be less than 2.5",
                "Doit être inférieur à 2,5",
            ),
            (
                field("must be one of \"a\", \"b\""),
                "Must be one of \"a\", \"b\"",
                "Doit être l’une de ces valeurs\u{a0}: \"a\", \"b\"",
            ),
            // What no table has stays as the schema says it.
            (field("must be string"), "Must be string", "Must be string"),
            (AdminText::PageUnavailable, "Page unavailable", "Page indisponible"),
            (
                AdminText::PageUnavailableMessage,
                "This page could not be displayed.",
                "Cette page n’a pas pu être affichée.",
            ),
            (
                AdminText::PageDisabled,
                "This page is disabled.",
                "Cette page est désactivée.",
            ),
            (
                AdminText::ActionFailed,
                "Internal server error",
                "Erreur interne du serveur",
            ),
            (
                AdminText::RequestFailed,
                "Something went wrong. Try again.",
                "Une erreur s’est produite. Réessayez.",
            ),
            (AdminText::Copied, "Copied", "Copié"),
            (AdminText::CopyFailed, "Could not copy", "Impossible de copier"),
        ] {
            assert_eq!(admin_text("en", "en", None, &text), en, "{text:?}");
            assert_eq!(admin_text("fr", "fr", None, &text), fr, "{text:?}");
            assert_eq!(admin_text("fr-CA", "fr-CA", None, &text), fr, "{text:?}");
            // A language without a table is English.
            assert_eq!(admin_text("de", "de", None, &text), en, "{text:?}");
        }
    }

    #[test]
    fn the_texts_of_the_application_come_first() {
        let texts: AdminTextsFn = Arc::new(|text| match text {
            AdminText::Field(FieldError::Required) => Some("Pflichtfeld".to_string()),
            AdminText::Field(FieldError::AtLeast(minimum)) => Some(format!("Mindestens {minimum}")),
            _ => None,
        });
        assert_eq!(
            admin_text("de", "de", Some(&texts), &field("is required")),
            "Pflichtfeld"
        );
        assert_eq!(
            admin_text("de", "de", Some(&texts), &field("must be >= 2")),
            "Mindestens 2"
        );
        // The others: the table of the language, then English.
        assert_eq!(admin_text("de", "de", Some(&texts), &AdminText::Copied), "Copied");
        assert_eq!(admin_text("fr", "fr", Some(&texts), &AdminText::Copied), "Copié");
        assert_eq!(
            admin_text("fr", "fr", Some(&texts), &field("is required")),
            "Pflichtfeld"
        );
    }

    #[test]
    fn a_request_in_another_language_has_the_table_of_its_language() {
        // The texts of the application are the ones of `admin_locale`, German.
        let texts: AdminTextsFn = Arc::new(|text| match text {
            AdminText::Copied => Some("Kopiert".to_string()),
            _ => None,
        });
        assert_eq!(admin_text("fr", "de", Some(&texts), &AdminText::Copied), "Copié");
        assert_eq!(admin_text("en", "de", Some(&texts), &AdminText::Copied), "Copied");
        assert_eq!(admin_text("de-CH", "de", Some(&texts), &AdminText::Copied), "Kopiert");
        // A language without a table: the texts of `admin_locale`.
        assert_eq!(admin_text("ja", "de", Some(&texts), &AdminText::Copied), "Kopiert");
        assert_eq!(admin_text("ja", "fr", None, &AdminText::Copied), "Copié");
        assert_eq!(admin_text("ja", "en", None, &AdminText::Copied), "Copied");
    }

    #[test]
    fn a_language_tag() {
        for tag in ["en", "fr", "fr-CA", "pt-BR", "zh-Hant-TW", "es-419", "fil"] {
            assert!(is_language_tag(tag), "{tag}");
        }
        for tag in [
            "",
            "f",
            "fr_FR",
            "french",
            "fr-",
            "-fr",
            "fr CA",
            "fr-toolongsubtag",
            "1a",
            "fr-ÇA",
        ] {
            assert!(!is_language_tag(tag), "{tag}");
        }
    }
}
