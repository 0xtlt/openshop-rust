//! The files a form of an admin page sends: what a field of type [`Upload`]
//! of the input of an action receives.

use std::fmt;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

use super::view::property_types;

/// A file sent by a form of an admin page: `<input type="file" name="file">`,
/// or Polaris' `<s-drop-zone name="file">`. A field of this type in the
/// `#[input]` of an action receives it, next to the text fields of the form.
///
/// ```
/// use openshop::admin::Upload;
/// use openshop::prelude::*;
///
/// #[input]
/// struct Import {
///     file: Upload,
///     dry_run: bool,
/// }
///
/// fn rows(input: &Import) -> Result<usize> {
///     Ok(input.file.text()?.lines().skip(1).count())
/// }
/// # let input = Import { file: Upload::new("members.csv", "text/csv", "email\na@example.com\n"), dry_run: false };
/// # assert_eq!(rows(&input).unwrap(), 1);
/// ```
///
/// - A field left empty (no file chosen) sends no file: an `Option<Upload>`
///   is `None`, an `Upload` is refused with `Required`.
/// - A `Vec<Upload>` takes every file of its name (`multiple`), `[]` when
///   none was chosen. An `Upload` takes one, the last of its name, as a text
///   field does.
/// - The whole request is at most `max_request_body_bytes` (2 MB unless
///   `app!` sets it): a larger form is answered `413`, with an error toast.
/// - In JSON (an RPC call, `ctx.call_action`), it is
///   `{ "fileName": …, "contentType": …, "bytes": "<base64>" }`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Upload {
    /// The name of the file as the browser sends it, without its directory:
    /// `members.csv`. Chosen by whoever sends the form: never a path to
    /// write to as it is.
    pub file_name: String,
    /// Its media type as the browser says it: `text/csv`;
    /// `application/octet-stream` when it says none.
    pub content_type: String,
    /// Its content.
    #[serde(with = "base64_bytes")]
    #[schemars(with = "String", extend("contentEncoding" = "base64"))]
    pub bytes: Vec<u8>,
}

impl Upload {
    /// A file, as a test sends it: `Upload::new("members.csv", "text/csv", "email\n")`.
    pub fn new(file_name: impl Into<String>, content_type: impl Into<String>, bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            file_name: file_name.into(),
            content_type: content_type.into(),
            bytes: bytes.into(),
        }
    }

    /// The content as UTF-8 text: a CSV, a JSON file.
    pub fn text(&self) -> Result<&str, std::str::Utf8Error> {
        std::str::from_utf8(&self.bytes)
    }

    /// The size of the content, in bytes.
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// A file field left empty: the browser sends it without a name nor content.
    fn is_none_chosen(&self) -> bool {
        self.file_name.is_empty() && self.bytes.is_empty()
    }
}

/// The content is not printed: a log line of the input would carry the file.
impl fmt::Debug for Upload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Upload")
            .field("file_name", &self.file_name)
            .field("content_type", &self.content_type)
            .field("len", &self.bytes.len())
            .finish()
    }
}

mod base64_bytes {
    use super::*;

    pub(super) fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(deserializer)?;
        STANDARD.decode(text).map_err(serde::de::Error::custom)
    }
}

/// Adds the files of a form to `input`, the object
/// [`fields_to_json`](super::fields_to_json) made of its text fields, each
/// one as the JSON of an [`Upload`]. A property that is an array collects
/// every file of its name, and is `[]` when the field was sent without one;
/// another property takes the last file of its name. A field left empty is
/// no file: the property stays missing, for the schema to say whether it is
/// required. Fields that start with `_` are the page's.
pub(crate) fn add_uploads(schema: &Value, input: &mut Value, files: &[(String, Upload)]) {
    let Value::Object(object) = input else { return };
    let properties = schema.get("properties").and_then(Value::as_object);
    for (name, upload) in files.iter().filter(|(name, _)| !name.starts_with('_')) {
        let is_array = properties
            .and_then(|properties| properties.get(name))
            .is_some_and(|property| property_types(property, schema).contains(&"array"));
        add_upload(object, name, upload, is_array);
    }
}

fn add_upload(object: &mut Map<String, Value>, name: &str, upload: &Upload, is_array: bool) {
    let value = (!upload.is_none_chosen()).then(|| serde_json::to_value(upload).unwrap_or(Value::Null));
    if is_array {
        let entry = object
            .entry(name.to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        if let (Value::Array(entries), Some(value)) = (entry, value) {
            entries.push(value);
        }
    } else if let Some(value) = value {
        object.insert(name.to_string(), value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admin::{FieldError, fields_to_json, parse_fields};
    use crate::types::InputSchema;
    use serde_json::json;

    #[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
    #[serde(rename_all = "camelCase")]
    struct Import {
        file: Upload,
        note: Option<Upload>,
        #[serde(default)]
        images: Vec<Upload>,
        dry_run: bool,
    }

    fn csv() -> Upload {
        Upload::new("members.csv", "text/csv", "email,points\na@example.com,10\n")
    }

    /// No file chosen, as a browser sends an empty `<input type="file">`.
    fn none_chosen() -> Upload {
        Upload::new("", "application/octet-stream", Vec::new())
    }

    fn input(fields: &[u8], files: &[(&str, Upload)]) -> Value {
        let schema = InputSchema::of::<Import>().json_schema;
        let mut input = fields_to_json(&schema, &parse_fields(fields));
        let files: Vec<(String, Upload)> = files
            .iter()
            .map(|(name, upload)| (name.to_string(), upload.clone()))
            .collect();
        add_uploads(&schema, &mut input, &files);
        input
    }

    fn parsed(input: Value) -> Result<Import, String> {
        let value = InputSchema::of::<Import>().parse(input)?;
        serde_json::from_value(value).map_err(|error| error.to_string())
    }

    #[test]
    fn an_upload_is_json_with_its_bytes_in_base64() {
        let json = serde_json::to_value(Upload::new("a.txt", "text/plain", "hi")).unwrap();
        assert_eq!(
            json,
            json!({ "fileName": "a.txt", "contentType": "text/plain", "bytes": "aGk=" })
        );
        let back: Upload = serde_json::from_value(json).unwrap();
        assert_eq!(back.text().unwrap(), "hi");
        assert_eq!(back.len(), 2);
        // The bytes are not printed.
        assert_eq!(
            format!("{back:?}"),
            r#"Upload { file_name: "a.txt", content_type: "text/plain", len: 2 }"#
        );
        assert!(
            serde_json::from_value::<Upload>(json!({ "fileName": "a", "contentType": "b", "bytes": "%%" })).is_err()
        );
    }

    #[test]
    fn a_file_of_the_form_is_a_field_of_the_input() {
        let import = parsed(input(
            b"_action=import&dryRun=on",
            &[
                ("file", csv()),
                ("images", csv()),
                ("images", Upload::new("b.png", "image/png", [1, 2])),
            ],
        ))
        .unwrap();
        assert_eq!(import.file, csv());
        assert!(import.dry_run);
        assert_eq!(import.note, None);
        let names: Vec<&str> = import.images.iter().map(|image| image.file_name.as_str()).collect();
        assert_eq!(names, ["members.csv", "b.png"]);
        assert_eq!(import.images[1].bytes, [1, 2]);
    }

    #[test]
    fn a_file_field_left_empty_sends_no_file() {
        let sent = input(
            b"",
            &[
                ("file", none_chosen()),
                ("note", none_chosen()),
                ("images", none_chosen()),
            ],
        );
        assert_eq!(sent, json!({ "images": [], "dryRun": false }));
        let schema = InputSchema::of::<Import>().json_schema;
        let refused = crate::server::mcp::schema::property_errors(&sent, &schema);
        let shown: Vec<(&str, FieldError)> = refused
            .iter()
            .map(|(field, error)| (field.as_str(), FieldError::from_schema(error)))
            .collect();
        assert_eq!(shown, [("file", FieldError::Required)]);

        // An `Option<Upload>` left empty is `None`.
        let import = parsed(input(b"", &[("file", csv()), ("note", none_chosen())])).unwrap();
        assert_eq!(import.note, None);
        assert!(import.images.is_empty());
        // An empty file that has a name is a file.
        let empty = parsed(input(
            b"",
            &[("file", Upload::new("empty.csv", "text/csv", Vec::new()))],
        ))
        .unwrap();
        assert_eq!(empty.file.file_name, "empty.csv");
        assert!(empty.file.is_empty());
    }

    #[test]
    fn a_single_file_field_takes_the_last_file_and_text_is_refused() {
        let other = Upload::new("other.csv", "text/csv", "x");
        let import = parsed(input(b"", &[("file", csv()), ("file", other.clone())])).unwrap();
        assert_eq!(import.file, other);
        // What a form sent URL-encoded makes of a file: text, which the schema refuses.
        assert!(parsed(input(b"file=%5Bobject+File%5D", &[])).is_err());
        // The page's fields are not the input's.
        assert_eq!(input(b"", &[("_file", csv())]), json!({ "dryRun": false }));
    }
}
