//! Liquid templates kept in files, for the app proxy answers Shopify renders.

use std::borrow::Cow;
use std::fmt;

use serde::{Serialize, Serializer};
use serde_json::Value;

/// A Liquid template, and the values the server gives it.
///
/// Shopify renders the answer of a `liquid` app proxy with the objects of the
/// storefront: `shop`, `customer`, `cart`… The template is a file next to its
/// handler, embedded by [`liquid!`](crate::liquid!), and what only the server
/// knows reaches it as variables:
///
/// ```
/// use openshop::Liquid;
///
/// let template = Liquid::new("<p>{{ review_count }} reviews of {{ shop.name }}</p>").assign("review_count", 12);
/// assert_eq!(
///     template.render().unwrap(),
///     "{%- assign review_count = 12 -%}\n<p>{{ review_count }} reviews of {{ shop.name }}</p>"
/// );
/// ```
///
/// A value is never pasted into the template: it is written as a Liquid
/// literal that the text of the value cannot close, so that what a customer
/// typed is displayed and never executed.
#[derive(Debug, Clone, PartialEq)]
pub struct Liquid {
    source: Cow<'static, str>,
    /// Each variable with its value, or why the value cannot be written.
    assigns: Vec<(String, Result<Value, String>)>,
}

impl Liquid {
    pub fn new(source: impl Into<Cow<'static, str>>) -> Self {
        Self {
            source: source.into(),
            assigns: Vec::new(),
        }
    }

    /// Gives the template a variable, `{{ name }}`.
    ///
    /// A string, a number, a boolean and `None` are the same value in Liquid.
    /// Liquid has no literal for a list or an object: those are assigned as
    /// their JSON text, to print in a `<script type="application/json">` for
    /// the scripts of the page.
    pub fn assign(mut self, name: impl Into<String>, value: impl Serialize) -> Self {
        let value = serde_json::to_value(value).map_err(|error| error.to_string());
        self.assigns.push((name.into(), value));
        self
    }

    /// The template as it is sent to Shopify: the variables, then the file.
    /// Fails on a name Liquid does not accept for a variable, or on a value
    /// that does not serialize.
    pub fn render(&self) -> Result<String, String> {
        let mut rendered = String::new();
        for (name, value) in &self.assigns {
            if !is_variable_name(name) {
                return Err(format!("`{name}` is not a name for a Liquid variable"));
            }
            let value = value
                .as_ref()
                .map_err(|error| format!("the value of `{name}` does not serialize: {error}"))?;
            rendered.push_str(&format!("{{%- assign {name} = {} -%}}\n", literal(value)));
        }
        rendered.push_str(&self.source);
        Ok(rendered)
    }
}

/// Letters, digits, `_` and `-`, not starting with a digit or a `-`.
fn is_variable_name(name: &str) -> bool {
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && characters.all(|character| character.is_ascii_alphanumeric() || character == '_' || character == '-')
}

fn literal(value: &Value) -> String {
    match value {
        Value::Null => "nil".to_string(),
        Value::Bool(boolean) => boolean.to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => string_literal(text),
        structured => string_literal(&structured.to_string()),
    }
}

/// A Liquid expression whose value is `text`.
///
/// A Liquid string has no escape sequence, and a tag ends at the first `%}`
/// wherever it is. So `"`, `%`, `{` and `}` each go in a literal of their own,
/// joined with `append`: the text can neither close its literal, nor the tag,
/// nor open another one.
fn string_literal(text: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut plain = String::new();
    for character in text.chars() {
        if matches!(character, '"' | '%' | '{' | '}') {
            if !plain.is_empty() {
                parts.push(format!("\"{plain}\""));
                plain.clear();
            }
            parts.push(format!("'{character}'"));
        } else {
            plain.push(character);
        }
    }
    if !plain.is_empty() || parts.is_empty() {
        parts.push(format!("\"{plain}\""));
    }
    parts.join(" | append: ")
}

/// A template is the body of the answer: the handler of a `liquid` proxy
/// returns it as it would a string.
impl Serialize for Liquid {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let rendered = self.render().map_err(serde::ser::Error::custom)?;
        serializer.serialize_str(&rendered)
    }
}

impl fmt::Display for Liquid {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.render() {
            Ok(rendered) => formatter.write_str(&rendered),
            Err(_) => Err(fmt::Error),
        }
    }
}

/// The Liquid template in a file next to the source file that names it:
/// `liquid!("reviews.liquid")`.
///
/// ```ignore
/// // src/proxy/reviews.rs, next to src/proxy/reviews.liquid
/// use openshop::{Liquid, ProxyContext, Result, liquid, proxy};
///
/// #[proxy(GET, "/reviews", liquid)]
/// pub async fn reviews(ctx: ProxyContext) -> Result<Liquid> {
///     Ok(liquid!("reviews.liquid").assign("review_count", 12))
/// }
/// ```
///
/// The file is embedded when the crate compiles, like `include_str!`: a
/// missing file is a compile error, the binary carries its templates, and
/// `openshop dev` restarts when one changes. The value is a
/// [`Liquid`](crate::Liquid), to give variables with
/// [`assign`](crate::Liquid::assign).
#[macro_export]
macro_rules! liquid {
    ($path:literal) => {
        $crate::Liquid::new(::core::include_str!($path))
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_template_without_variables_is_its_file() {
        let template = Liquid::new("<h2>{{ shop.name }}</h2>");
        assert_eq!(template.render().unwrap(), "<h2>{{ shop.name }}</h2>");
        assert_eq!(
            serde_json::to_value(&template).unwrap(),
            json!("<h2>{{ shop.name }}</h2>")
        );
        assert_eq!(template.to_string(), "<h2>{{ shop.name }}</h2>");
    }

    #[test]
    fn variables_are_assigned_before_the_file_as_liquid_values() {
        let template = Liquid::new("{{ title }}")
            .assign("title", "Reviews")
            .assign("count", 12)
            .assign("average", 4.5)
            .assign("verified", true)
            .assign("note", None::<String>)
            .assign("tags", ["new", "sale"]);
        assert_eq!(
            template.render().unwrap(),
            "{%- assign title = \"Reviews\" -%}\n\
             {%- assign count = 12 -%}\n\
             {%- assign average = 4.5 -%}\n\
             {%- assign verified = true -%}\n\
             {%- assign note = nil -%}\n\
             {%- assign tags = \"[\" | append: '\"' | append: \"new\" | append: '\"' | append: \",\" | append: '\"' | append: \"sale\" | append: '\"' | append: \"]\" -%}\n\
             {{ title }}"
        );
    }

    #[test]
    fn a_value_cannot_close_its_literal_or_its_tag() {
        let hostile = r#"" %}{% include 'secrets' %}{{ shop.secret }}"#;
        let rendered = Liquid::new("{{ text }}").assign("text", hostile).render().unwrap();
        let (assignment, _) = rendered.split_once('\n').unwrap();
        let tag = assignment
            .strip_prefix("{%- assign text = ")
            .and_then(|rest| rest.strip_suffix(" -%}"))
            .unwrap();

        // Nothing inside the tag opens or closes a tag or an output.
        for delimiter in ["%}", "{%", "{{", "}}"] {
            assert!(!tag.contains(delimiter), "`{delimiter}` in `{tag}`");
        }
        // Read back the way Liquid does, the literals give the text.
        let read: String = tag.split(" | append: ").map(|part| &part[1..part.len() - 1]).collect();
        assert_eq!(read, hostile);
        assert_eq!(string_literal(""), "\"\"");
        assert_eq!(string_literal("it's 100%"), "\"it's 100\" | append: '%'");
    }

    #[test]
    fn refuses_a_name_that_is_not_a_variable() {
        for name in ["", "1st", "a b", "x %}", "-dash"] {
            let error = Liquid::new("").assign(name, 1).render().unwrap_err();
            assert_eq!(error, format!("`{name}` is not a name for a Liquid variable"));
        }
        assert!(Liquid::new("").assign("review_count-2", 1).render().is_ok());
        assert!(serde_json::to_value(Liquid::new("").assign("a b", 1)).is_err());
    }
}
