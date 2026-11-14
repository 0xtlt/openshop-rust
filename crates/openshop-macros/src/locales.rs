//! The locale files of a package, in the format of Shopify's: `locales/` next
//! to its `Cargo.toml`, `en.default.json` (the default language), `fr.json`,
//! `de.json`…, with nested keys, `{{ name }}` placeholders and `one`/`other`
//! plurals. Read when the crate compiles, by `t!`, `locales!` and `app!`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use proc_macro2::{Span, TokenStream};
use quote::quote;
use serde_json::{Map, Value};
use syn::parse::{Parse, ParseStream};
use syn::{Expr, Ident, LitStr, Token};

use crate::util::plain_name;

/// The folder of the locale files, next to the `Cargo.toml` of the package.
pub const FOLDER: &str = "locales";
/// What `openshop dev` sets to the names of the files of `locales/`: a crate
/// that reads them is compiled again when a file is added or removed.
const FILES_VARIABLE: &str = "OPENSHOP_LOCALE_FILES";
/// The keys of a plural, chosen by `count`.
const PLURAL_KEYS: [&str; 2] = ["one", "other"];

/// A text of a locale file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Text {
    Plain(String),
    /// `{ "one": "1 review", "other": "{{ count }} reviews" }`.
    Plural {
        one: Option<String>,
        other: String,
    },
}

impl Text {
    fn kind(&self) -> &'static str {
        match self {
            Text::Plain(_) => "a text",
            Text::Plural { .. } => "a plural",
        }
    }

    /// Every form of the text.
    fn forms(&self) -> Vec<&str> {
        match self {
            Text::Plain(text) => vec![text],
            Text::Plural { one, other } => one.iter().map(String::as_str).chain([other.as_str()]).collect(),
        }
    }
}

/// A locale file: its language and its texts, by their dotted key.
#[derive(Debug)]
pub struct File {
    pub language: String,
    /// `fr.json`.
    pub name: String,
    pub path: PathBuf,
    pub texts: Vec<(String, Text)>,
    /// The keys of the groups of texts: `home` of `home.title`.
    pub groups: Vec<String>,
}

impl File {
    fn text(&self, key: &str) -> Option<&Text> {
        self.texts.iter().find(|(name, _)| name == key).map(|(_, text)| text)
    }
}

/// The locale files of a package, the default one first.
#[derive(Debug)]
pub struct Catalog {
    pub files: Vec<File>,
}

impl Catalog {
    pub fn default_file(&self) -> &File {
        &self.files[0]
    }

    /// The keys of the default file another file does not have, by language.
    pub fn missing(&self) -> Vec<(&str, &str)> {
        let default = self.default_file();
        self.files[1..]
            .iter()
            .flat_map(|file| {
                default
                    .texts
                    .iter()
                    .filter(|(key, _)| file.text(key).is_none())
                    .map(|(key, _)| (file.language.as_str(), key.as_str()))
            })
            .collect()
    }

    /// What makes the crate compile again when a file changes: each file, and
    /// the list of the files `openshop dev` gives.
    fn tracked(&self) -> TokenStream {
        let paths = self.files.iter().map(|file| file.path.to_string_lossy().into_owned());
        quote! {
            #(const _: &[u8] = ::core::include_bytes!(#paths);)*
            const _: ::core::option::Option<&str> = ::core::option_env!(#FILES_VARIABLE);
        }
    }
}

/// The `locales/` folder of the package of the file a macro is written in:
/// next to the closest `Cargo.toml` above that file, else next to the one of
/// the crate being compiled. `None` when the package has none.
pub fn folder() -> Option<PathBuf> {
    package_root()
        .map(|root| root.join(FOLDER))
        .filter(|folder| folder.is_dir())
}

fn package_root() -> Option<PathBuf> {
    let source = proc_macro::Span::call_site().local_file().and_then(|file| {
        if file.is_absolute() {
            Some(file)
        } else {
            std::env::current_dir().ok().map(|cwd| cwd.join(file))
        }
    });
    let from_source = source.and_then(|file| {
        file.ancestors()
            .skip(1)
            .find(|directory| directory.join("Cargo.toml").is_file())
            .map(Path::to_path_buf)
    });
    from_source.or_else(|| std::env::var_os("CARGO_MANIFEST_DIR").map(PathBuf::from))
}

/// The files of the folder and when each one last changed: a catalog read
/// before is read again when one of them changed.
type Stamp = Vec<(PathBuf, Option<SystemTime>)>;

/// The catalogs read during this compilation: every `t!` of a crate reads the
/// same folder.
static READ: Mutex<Option<Catalogs>> = Mutex::new(None);

/// A catalog by folder, with the stamp of the files it was read from.
type Catalogs = HashMap<PathBuf, (Stamp, Arc<Result<Catalog, String>>)>;

/// The catalog of `folder`, checked: the reason it is refused otherwise.
pub fn catalog(folder: &Path) -> Arc<Result<Catalog, String>> {
    let stamp = stamp(folder);
    let mut read = READ.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let read = read.get_or_insert_with(HashMap::new);
    if let Some((stamped, catalog)) = read.get(folder)
        && *stamped == stamp
    {
        return catalog.clone();
    }
    let catalog = Arc::new(read_catalog(folder));
    read.insert(folder.to_path_buf(), (stamp, catalog.clone()));
    catalog
}

fn stamp(folder: &Path) -> Stamp {
    let mut files: Stamp = std::fs::read_dir(folder)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| {
                    let modified = entry.metadata().and_then(|metadata| metadata.modified()).ok();
                    (entry.path(), modified)
                })
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files
}

/// Whether `tag` is a language tag: `fr`, `fr-CA`, `zh-Hant-TW`. The rule of
/// `admin_locale`.
fn is_language_tag(tag: &str) -> bool {
    let mut subtags = tag.split('-');
    let primary = subtags.next().unwrap_or_default();
    (2..=3).contains(&primary.len())
        && primary.bytes().all(|byte| byte.is_ascii_alphabetic())
        && subtags
            .all(|subtag| (1..=8).contains(&subtag.len()) && subtag.bytes().all(|byte| byte.is_ascii_alphanumeric()))
}

/// Reads and checks the files of `folder`: the JSON of each, one default
/// file, the shape of the plurals, and the same kind of text for a key in
/// every file.
pub fn read_catalog(folder: &Path) -> Result<Catalog, String> {
    let entries = std::fs::read_dir(folder).map_err(|error| format!("{FOLDER}/ cannot be read: {error}"))?;
    let mut names: Vec<(String, PathBuf)> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && path.extension().is_some_and(|extension| extension == "json"))
        .filter_map(|path| Some((path.file_name()?.to_str()?.to_string(), path)))
        .collect();
    names.sort();

    let mut defaults = Vec::new();
    let mut files: Vec<File> = Vec::new();
    for (name, path) in names {
        let stem = name.strip_suffix(".json").unwrap_or(&name);
        let (language, default) = match stem.strip_suffix(".default") {
            Some(language) => (language, true),
            None => (stem, false),
        };
        if !is_language_tag(language) {
            return Err(format!(
                "{FOLDER}/{name}: `{language}` is not a language tag: name a locale file `fr.json`, `fr-CA.json`, or `en.default.json` for the default language"
            ));
        }
        if let Some(same) = files.iter().find(|file| file.language.eq_ignore_ascii_case(language)) {
            return Err(format!(
                "{FOLDER}/{name} and {FOLDER}/{} are both `{language}`: one file per language",
                same.name
            ));
        }
        let content =
            std::fs::read_to_string(&path).map_err(|error| format!("{FOLDER}/{name} cannot be read: {error}"))?;
        let file = parse_file(language, &name, &path, &content)?;
        if default {
            defaults.push(files.len());
        }
        files.push(file);
    }

    let default = match defaults.as_slice() {
        [default] => *default,
        [] => {
            return Err(format!(
                "{FOLDER}/ has no default file: name the file of the default language `<language>.default.json`, `en.default.json`"
            ));
        }
        [first, second, ..] => {
            return Err(format!(
                "{FOLDER}/ has two default files, {} and {}: one language is the default",
                files[*first].name, files[*second].name
            ));
        }
    };
    let default = files.remove(default);
    for file in &files {
        for (key, text) in &file.texts {
            let expected = match default.text(key) {
                Some(expected) => expected.kind(),
                None if default.groups.contains(key) => "a group of texts",
                None => continue,
            };
            if expected != text.kind() {
                return Err(format!(
                    "{FOLDER}/{}: `{key}` is {}, and {expected} in {FOLDER}/{}",
                    file.name,
                    text.kind(),
                    default.name
                ));
            }
        }
        for group in &file.groups {
            if let Some(text) = default.text(group) {
                return Err(format!(
                    "{FOLDER}/{}: `{group}` is a group of texts, and {} in {FOLDER}/{}",
                    file.name,
                    text.kind(),
                    default.name
                ));
            }
        }
    }
    files.insert(0, default);
    Ok(Catalog { files })
}

fn parse_file(language: &str, name: &str, path: &Path, content: &str) -> Result<File, String> {
    let value: Value =
        serde_json::from_str(content).map_err(|error| format!("{FOLDER}/{name} is not JSON: {error}"))?;
    let Value::Object(root) = value else {
        return Err(format!(
            "{FOLDER}/{name}: expected an object of texts, `{{ \"home\": {{ \"title\": \"Home\" }} }}`"
        ));
    };
    let mut file = File {
        language: language.to_string(),
        name: name.to_string(),
        path: path.to_path_buf(),
        texts: Vec::new(),
        groups: Vec::new(),
    };
    collect(&mut file, "", &root)?;
    Ok(file)
}

/// The texts of `group`, under `prefix`.
fn collect(file: &mut File, prefix: &str, group: &Map<String, Value>) -> Result<(), String> {
    let name = file.name.clone();
    for (key, value) in group {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        if key.is_empty() || key.contains('.') || key.trim() != key {
            return Err(format!(
                "{FOLDER}/{name}: `{path}`: a key is a name without `.` nor spaces around it; nest an object for a group of texts"
            ));
        }
        match value {
            Value::String(text) => file.texts.push((path, Text::Plain(text.clone()))),
            Value::Object(forms) if forms.keys().any(|key| PLURAL_KEYS.contains(&key.as_str())) => {
                let plural = plural(&name, &path, forms)?;
                file.texts.push((path, plural));
            }
            Value::Object(nested) => {
                collect(file, &path, nested)?;
                file.groups.push(path);
            }
            other => {
                return Err(format!(
                    "{FOLDER}/{name}: `{path}` is {}: a text is a string, a group of texts an object, a plural `{{ \"one\": …, \"other\": … }}`",
                    json_kind(other)
                ));
            }
        }
    }
    Ok(())
}

fn plural(name: &str, path: &str, forms: &Map<String, Value>) -> Result<Text, String> {
    if let Some(key) = forms.keys().find(|key| !PLURAL_KEYS.contains(&key.as_str())) {
        return Err(format!(
            "{FOLDER}/{name}: `{path}` mixes `{key}` with the keys of a plural: a plural has `one` and `other`, chosen by `count`, and nothing else"
        ));
    }
    let form = |key: &str| match forms.get(key) {
        None => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(other) => Err(format!(
            "{FOLDER}/{name}: `{path}.{key}` is {}: a form of a plural is a string",
            json_kind(other)
        )),
    };
    let one = form("one")?;
    let Some(other) = form("other")? else {
        return Err(format!(
            "{FOLDER}/{name}: the plural `{path}` has no `other`: the form of any count but 1"
        ));
    };
    Ok(Text::Plural { one, other })
}

fn json_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// The names of the `{{ name }}` placeholders of a text.
pub fn placeholders(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            break;
        };
        let name = after[..end].trim().to_string();
        if !name.is_empty() && !names.contains(&name) {
            names.push(name);
        }
        rest = &after[end + 2..];
    }
    names
}

/// The `Locales` of the catalog, for `app!` and `locales!`: the languages of
/// the files, the default one first, and the keys the others lack.
pub fn locales(catalog: &Catalog) -> TokenStream {
    let tracked = catalog.tracked();
    let languages = catalog.files.iter().map(|file| &file.language);
    let missing = catalog
        .missing()
        .into_iter()
        .map(|(language, key)| quote!((#language, #key)));
    quote! {
        {
            #tracked
            ::openshop::i18n::Locales::__new(&[#(#languages),*], &[#(#missing),*])
        }
    }
}

/// `locales!()`: the `Locales` of the package, or why there are none.
pub fn locales_macro(input: TokenStream) -> syn::Result<TokenStream> {
    if !input.is_empty() {
        return Err(syn::Error::new_spanned(
            input,
            format!("`locales!` takes nothing: it reads `{FOLDER}/` next to the Cargo.toml of the package"),
        ));
    }
    let Some(folder) = folder() else {
        return Err(syn::Error::new(Span::call_site(), no_folder()));
    };
    match &*catalog(&folder) {
        Ok(catalog) => Ok(locales(catalog)),
        Err(reason) => Err(syn::Error::new(Span::call_site(), reason)),
    }
}

/// `app!`: the `.locales(…)` of the builder when the package has a
/// `locales/` folder, or why its files are refused.
pub fn app_locales() -> syn::Result<Option<TokenStream>> {
    let Some(folder) = folder() else {
        return Ok(None);
    };
    match &*catalog(&folder) {
        Ok(catalog) => {
            let locales = locales(catalog);
            Ok(Some(quote!(.locales(#locales))))
        }
        Err(reason) => Err(syn::Error::new(Span::call_site(), reason)),
    }
}

fn no_folder() -> String {
    format!(
        "no `{FOLDER}/` folder next to the Cargo.toml of the package: its texts are in `{FOLDER}/en.default.json`, `{FOLDER}/fr.json`…"
    )
}

/// `t!("home.title")`, `t!("home.count", count = n)`, `t!("home.hello", name = value)`.
pub struct Translation {
    key: LitStr,
    count: Option<Expr>,
    arguments: Vec<(String, Expr)>,
}

impl Parse for Translation {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let key: LitStr = input
            .parse()
            .map_err(|error| syn::Error::new(error.span(), "expected the key of a text: `t!(\"home.title\")`"))?;
        let mut count = None;
        let mut arguments: Vec<(String, Expr)> = Vec::new();
        while !input.is_empty() {
            input.parse::<Token![,]>()?;
            if input.is_empty() {
                break;
            }
            let name: Ident = input.parse().map_err(|error| {
                syn::Error::new(
                    error.span(),
                    "expected a value of the text: `name = value`, or `count = n` for a plural",
                )
            })?;
            input.parse::<Token![=]>()?;
            let value: Expr = input.parse()?;
            let plain = plain_name(&name);
            if plain == "count" && count.is_none() {
                count = Some(value);
            } else if plain == "count" || arguments.iter().any(|(given, _)| *given == plain) {
                return Err(syn::Error::new(name.span(), format!("`{plain}` is given twice")));
            } else {
                arguments.push((plain, value));
            }
        }
        Ok(Self { key, count, arguments })
    }
}

/// `t!`: the text of `key` in the language of the request. The key, its
/// plural and its placeholders are checked against the default file.
pub fn translate(translation: Translation) -> syn::Result<TokenStream> {
    let span = translation.key.span();
    let Some(folder) = folder() else {
        return Err(syn::Error::new(span, no_folder()));
    };
    let catalog = catalog(&folder);
    let catalog = catalog
        .as_ref()
        .as_ref()
        .map_err(|reason| syn::Error::new(span, reason))?;
    expand_translation(catalog, &translation)
}

fn expand_translation(catalog: &Catalog, translation: &Translation) -> syn::Result<TokenStream> {
    let key = translation.key.value();
    let span = translation.key.span();
    let default = catalog.default_file();
    let at = format!("{FOLDER}/{}", default.name);
    let Some(text) = default.text(&key) else {
        if default.groups.contains(&key) {
            let first = default
                .texts
                .iter()
                .find(|(name, _)| name.starts_with(&format!("{key}.")))
                .map_or(String::new(), |(name, _)| format!(": name one of its texts, `{name}`"));
            return Err(syn::Error::new(
                span,
                format!("`{key}` is a group of texts in {at}, not a text{first}"),
            ));
        }
        return Err(syn::Error::new(span, format!("no text `{key}` in {at}")));
    };
    if matches!(text, Text::Plural { .. }) && translation.count.is_none() {
        return Err(syn::Error::new(
            span,
            format!("`{key}` is a plural in {at}: give the count it is chosen by, `t!(\"{key}\", count = n)`"),
        ));
    }
    for form in text.forms() {
        for name in placeholders(form) {
            let given = name == "count" && translation.count.is_some()
                || translation.arguments.iter().any(|(argument, _)| *argument == name);
            if !given {
                return Err(syn::Error::new(
                    span,
                    format!("`{key}` writes `{{{{ {name} }}}}` in {at}: give its value, `t!(\"{key}\", {name} = …)`"),
                ));
            }
        }
    }

    let texts = catalog.files.iter().filter_map(|file| {
        let language = &file.language;
        let text = match file.text(&key)? {
            Text::Plain(text) => quote!(::openshop::i18n::__Text::Plain(#text)),
            Text::Plural { one, other } => {
                let one = match one {
                    Some(one) => quote!(::core::option::Option::Some(#one)),
                    None => quote!(::core::option::Option::None),
                };
                quote!(::openshop::i18n::__Text::Plural { one: #one, other: #other })
            }
        };
        Some(quote!((#language, #text)))
    });
    let count = match &translation.count {
        Some(count) => quote!(::core::option::Option::Some(&(#count) as &dyn ::openshop::i18n::Count)),
        None => quote!(::core::option::Option::None),
    };
    let arguments = translation
        .arguments
        .iter()
        .map(|(name, value)| quote!((#name, &(#value) as &dyn ::core::fmt::Display)));
    let tracked = catalog.tracked();
    Ok(quote! {
        {
            #tracked
            ::openshop::i18n::__translate(&[#(#texts),*], #count, &[#(#arguments),*])
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `locales/` folder with these files, in a directory of its own.
    fn folder(files: &[(&str, &str)]) -> PathBuf {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let next = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let folder = std::env::temp_dir()
            .join(format!("openshop-locales-{}-{next}", std::process::id()))
            .join(FOLDER);
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        for (name, content) in files {
            std::fs::write(folder.join(name), content).unwrap();
        }
        folder
    }

    fn refusal(files: &[(&str, &str)]) -> String {
        read_catalog(&folder(files)).expect_err("refused")
    }

    const DEFAULT: &str = r#"{
        "home": {
            "title": "Stock watch",
            "hello": "Hello {{ name }}",
            "count": { "one": "1 variant", "other": "{{ count }} variants" }
        }
    }"#;

    #[test]
    fn reads_the_texts_by_their_key_and_what_the_other_files_lack() {
        let catalog = read_catalog(&folder(&[
            ("en.default.json", DEFAULT),
            (
                "fr.json",
                r#"{ "home": { "title": "Veille des stocks", "count": { "other": "{{ count }} variantes" } } }"#,
            ),
            ("README.md", "not a locale file"),
        ]))
        .unwrap();
        let languages: Vec<&str> = catalog.files.iter().map(|file| file.language.as_str()).collect();
        assert_eq!(languages, ["en", "fr"]);
        let default = catalog.default_file();
        assert_eq!(default.text("home.title"), Some(&Text::Plain("Stock watch".into())));
        assert_eq!(
            default.text("home.count"),
            Some(&Text::Plural {
                one: Some("1 variant".into()),
                other: "{{ count }} variants".into()
            })
        );
        assert_eq!(default.groups, ["home"]);
        assert_eq!(catalog.missing(), [("fr", "home.hello")]);
    }

    #[test]
    fn refuses_files_that_are_not_locale_files() {
        for (files, expected) in [
            (
                &[("en.default.json", "{ \"title\": ")][..],
                "locales/en.default.json is not JSON: EOF while parsing a value at line 1 column 11",
            ),
            (
                &[("en.default.json", "[]")],
                "locales/en.default.json: expected an object of texts",
            ),
            (
                &[("en.default.json", "{ \"count\": 3 }")],
                "locales/en.default.json: `count` is a number: a text is a string",
            ),
            (
                &[("en.default.json", "{ \"home.title\": \"Home\" }")],
                "locales/en.default.json: `home.title`: a key is a name without `.`",
            ),
            (&[("fr.json", "{}")], "locales/ has no default file"),
            (
                &[("en.default.json", "{}"), ("fr.default.json", "{}")],
                "locales/ has two default files, en.default.json and fr.default.json",
            ),
            (
                &[("en.default.json", "{}"), ("EN.json", "{}")],
                "locales/en.default.json and locales/EN.json are both `en`",
            ),
            (
                &[("english.default.json", "{}")],
                "locales/english.default.json: `english` is not a language tag",
            ),
        ] {
            let refused = refusal(files);
            assert!(refused.starts_with(expected), "{refused}");
        }
    }

    #[test]
    fn refuses_plurals_of_another_shape() {
        for (count, expected) in [
            (
                r#"{ "one": "1 variant", "other": "{{ count }} variants", "zero": "None" }"#,
                "`count` mixes `zero` with the keys of a plural: a plural has `one` and `other`",
            ),
            (r#"{ "one": "1 variant" }"#, "the plural `count` has no `other`"),
            (
                r#"{ "other": 2 }"#,
                "`count.other` is a number: a form of a plural is a string",
            ),
        ] {
            let refused = refusal(&[("en.default.json", &format!("{{ \"count\": {count} }}"))]);
            assert!(refused.contains(expected), "{refused}");
        }
        // A file whose key is another kind of text than in the default file.
        let mismatch = refusal(&[
            ("en.default.json", DEFAULT),
            ("fr.json", r#"{ "home": { "count": "variantes" } }"#),
        ]);
        assert_eq!(
            mismatch,
            "locales/fr.json: `home.count` is a text, and a plural in locales/en.default.json"
        );
        let mismatch = refusal(&[("en.default.json", DEFAULT), ("fr.json", r#"{ "home": "Accueil" }"#)]);
        assert_eq!(
            mismatch,
            "locales/fr.json: `home` is a text, and a group of texts in locales/en.default.json"
        );
    }

    fn translation(tokens: TokenStream) -> Result<String, String> {
        let catalog = read_catalog(&folder(&[
            ("en.default.json", DEFAULT),
            ("fr.json", r#"{ "home": { "title": "Veille" } }"#),
        ]))
        .unwrap();
        let translation: Translation = syn::parse2(tokens).map_err(|error| error.to_string())?;
        expand_translation(&catalog, &translation)
            .map(|tokens| tokens.to_string())
            .map_err(|error| error.to_string())
    }

    #[test]
    fn a_translation_names_a_text_of_the_default_file_with_its_values() {
        let expanded = translation(quote!("home.title")).unwrap();
        assert!(
            expanded.contains(
                &quote!(::openshop::i18n::__translate(
                    &[
                        ("en", ::openshop::i18n::__Text::Plain("Stock watch")),
                        ("fr", ::openshop::i18n::__Text::Plain("Veille"))
                    ],
                    ::core::option::Option::None,
                    &[]
                ))
                .to_string()
            ),
            "{expanded}"
        );
        // A language without the text falls back on the default file, at runtime.
        let expanded = translation(quote!("home.hello", name = user.name,)).unwrap();
        assert!(expanded.contains(&quote!(&[("en", ::openshop::i18n::__Text::Plain("Hello {{ name }}"))]).to_string()));
        assert!(expanded.contains(&quote!(&[("name", &(user.name) as &dyn ::core::fmt::Display)]).to_string()));
        assert!(translation(quote!("home.count", count = variants.len())).is_ok());

        for (tokens, expected) in [
            (quote!("home.titel"), "no text `home.titel` in locales/en.default.json"),
            (
                quote!("home"),
                "`home` is a group of texts in locales/en.default.json, not a text: name one of its texts, `home.title`",
            ),
            (
                quote!("home.count"),
                "`home.count` is a plural in locales/en.default.json: give the count it is chosen by, `t!(\"home.count\", count = n)`",
            ),
            (
                quote!("home.hello"),
                "`home.hello` writes `{{ name }}` in locales/en.default.json: give its value, `t!(\"home.hello\", name = …)`",
            ),
            (quote!("home.hello", name = a, name = b), "`name` is given twice"),
            (quote!(home.title), "expected the key of a text: `t!(\"home.title\")`"),
        ] {
            assert_eq!(translation(tokens).unwrap_err(), expected);
        }
    }

    #[test]
    fn finds_the_placeholders_of_a_text() {
        assert_eq!(
            placeholders("{{ count }} of {{total}} for {{ count }}"),
            ["count", "total"]
        );
        assert!(placeholders("{{ unclosed").is_empty());
    }
}
