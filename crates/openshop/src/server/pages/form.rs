//! The body of a form of a page: `application/x-www-form-urlencoded`, or
//! `multipart/form-data` when it sends files.

use axum::body::to_bytes;
use axum::extract::{FromRequest, Multipart, Request};
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderMap, StatusCode};

use crate::admin::{Upload, parse_fields};

/// What a URL-encoded form may send: its fields. A multipart one, its files
/// with it, may send up to `max_request_body_bytes`.
const MAX_FORM_BYTES: usize = 256 * 1024;
/// The media type of a file whose part says none.
const OCTET_STREAM: &str = "application/octet-stream";

/// A form as it was sent: its fields in order, a name sent several times kept
/// each time, and its files, a field left empty among them (no name, no
/// content).
#[derive(Debug, Default)]
pub(crate) struct Form {
    pub(crate) fields: Vec<(String, String)>,
    pub(crate) files: Vec<(String, Upload)>,
}

/// Whether the body is `multipart/form-data`: a form that sends files.
pub(crate) fn is_multipart(headers: &HeaderMap) -> bool {
    headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|media_type| media_type.trim().eq_ignore_ascii_case("multipart/form-data"))
}

/// Reads the form a page was sent. A multipart body is read within the body
/// limit of the server (`max_request_body_bytes`): `413` above it, `400` when
/// it is not multipart. Any other body is read as URL-encoded, up to
/// [`MAX_FORM_BYTES`].
pub(crate) async fn read(request: Request) -> Result<Form, StatusCode> {
    if !is_multipart(request.headers()) {
        let body = to_bytes(request.into_body(), MAX_FORM_BYTES).await.unwrap_or_default();
        return Ok(Form {
            fields: parse_fields(&body),
            files: Vec::new(),
        });
    }
    let mut multipart = Multipart::from_request(request, &())
        .await
        .map_err(|rejection| rejection.status())?;
    let mut form = Form::default();
    while let Some(field) = multipart.next_field().await.map_err(|error| error.status())? {
        let Some(name) = field.name().map(str::to_string) else {
            continue;
        };
        match field.file_name().map(base_name) {
            Some(file_name) => {
                let content_type = field
                    .content_type()
                    .filter(|value| !value.is_empty())
                    .unwrap_or(OCTET_STREAM)
                    .to_string();
                let bytes = field.bytes().await.map_err(|error| error.status())?;
                form.files.push((name, Upload::new(file_name, content_type, bytes)));
            }
            None => form
                .fields
                .push((name, field.text().await.map_err(|error| error.status())?)),
        }
    }
    Ok(form)
}

/// The name of a file without the directory some browsers put before it:
/// `C:\fakepath\members.csv` is `members.csv`.
fn base_name(file_name: &str) -> String {
    file_name.rsplit(['/', '\\']).next().unwrap_or_default().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use tower::ServiceExt;

    fn request(content_type: &str, body: impl Into<Body>) -> Request {
        Request::builder()
            .method("POST")
            .uri("/import")
            .header(CONTENT_TYPE, content_type)
            .body(body.into())
            .unwrap()
    }

    const BOUNDARY: &str = "----openshop";

    fn multipart(parts: &[&str]) -> String {
        let mut body = String::new();
        for part in parts {
            body.push_str(&format!("--{BOUNDARY}\r\n{part}\r\n"));
        }
        body.push_str(&format!("--{BOUNDARY}--\r\n"));
        body
    }

    #[tokio::test]
    async fn reads_the_fields_and_the_files_of_a_multipart_form() {
        let body = multipart(&[
            "Content-Disposition: form-data; name=\"_action\"\r\n\r\nimport",
            "Content-Disposition: form-data; name=\"note\"\r\n\r\nété",
            "Content-Disposition: form-data; name=\"file\"; filename=\"members.csv\"\r\nContent-Type: text/csv\r\n\r\nemail,points\r\na@example.com,10",
            "Content-Disposition: form-data; name=\"raw\"; filename=\"C:\\\\fakepath\\\\data.bin\"\r\n\r\n\u{1}",
            // A file field left empty, as a browser sends it.
            "Content-Disposition: form-data; name=\"other\"; filename=\"\"\r\nContent-Type: application/octet-stream\r\n\r\n",
            // A part without a name is no field.
            "Content-Disposition: form-data\r\n\r\nlost",
        ]);
        let form = read(request(&format!("multipart/form-data; boundary={BOUNDARY}"), body))
            .await
            .unwrap();
        assert_eq!(
            form.fields,
            [
                ("_action".to_string(), "import".to_string()),
                ("note".to_string(), "été".to_string())
            ]
        );
        assert_eq!(
            form.files,
            [
                (
                    "file".to_string(),
                    Upload::new("members.csv", "text/csv", "email,points\r\na@example.com,10")
                ),
                ("raw".to_string(), Upload::new("data.bin", OCTET_STREAM, [1])),
                ("other".to_string(), Upload::new("", OCTET_STREAM, Vec::new())),
            ]
        );
    }

    #[tokio::test]
    async fn a_form_without_files_is_url_encoded() {
        let form = read(request("application/x-www-form-urlencoded", "_action=save&title=a+b"))
            .await
            .unwrap();
        assert_eq!(
            form.fields,
            [
                ("_action".to_string(), "save".to_string()),
                ("title".to_string(), "a b".to_string())
            ]
        );
        assert!(form.files.is_empty());
        assert!(is_multipart(request("Multipart/Form-Data; boundary=x", "").headers()));
        assert!(!is_multipart(request("multipart/mixed; boundary=x", "").headers()));
    }

    #[tokio::test]
    async fn a_multipart_body_is_read_within_the_body_limit() {
        // Without a boundary, it is not multipart.
        let refused = read(request("multipart/form-data", "x")).await.unwrap_err();
        assert_eq!(refused, StatusCode::BAD_REQUEST);
        // Above the limit of the server, `DefaultBodyLimit`: `413`.
        let body = multipart(&[&format!(
            "Content-Disposition: form-data; name=\"file\"; filename=\"big.bin\"\r\n\r\n{}",
            "x".repeat(64)
        )]);
        let server = |limit: usize| {
            axum::Router::new()
                .route(
                    "/import",
                    axum::routing::post(|request: Request| async move {
                        read(request).await.map_or_else(|status| status, |_| StatusCode::OK)
                    }),
                )
                .layer(axum::extract::DefaultBodyLimit::max(limit))
        };
        let content_type = format!("multipart/form-data; boundary={BOUNDARY}");
        let answer = server(32).oneshot(request(&content_type, body.clone())).await.unwrap();
        assert_eq!(answer.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let answer = server(1024).oneshot(request(&content_type, body)).await.unwrap();
        assert_eq!(answer.status(), StatusCode::OK);
    }
}
