//! A form that sends files (#90): `multipart/form-data`, its files in the
//! `Upload` fields of the input of the action, within `max_request_body_bytes`.

use maud::html;
use openshop::admin::Upload;
use openshop::prelude::*;
use openshop::server::ServerOptions;
use openshop::testing::{TestContext, TestOptions, toast, toast_error};
use reqwest::StatusCode;

use super::{APP, SHOP, fetch, form, send};

#[input]
pub(super) struct Import {
    label: String,
    file: Upload,
    note: Option<Upload>,
    #[serde(default)]
    images: Vec<Upload>,
}

/// A page that imports a file: a drop zone, and its other fields.
pub(super) struct ImportPage;

#[admin_page("/imports")]
impl ImportPage {
    #[action]
    async fn import(_ctx: AdminContext, input: Import) -> Result<Outcome> {
        let rows = input.file.text()?.lines().skip(1).count();
        let note = input.note.map_or("none".to_string(), |note| note.file_name);
        let images: Vec<String> = input
            .images
            .iter()
            .map(|image| format!("{} {}", image.file_name, image.len()))
            .collect();
        Ok(Outcome::toast(format!(
            "{}: {rows} rows of {} ({}), note {note}, images [{}]",
            input.label,
            input.file.file_name,
            input.file.content_type,
            images.join(", ")
        )))
    }

    #[view]
    fn view(form: FormState) -> Markup {
        html! {
            s-page heading="Import" {
                form method="post" enctype="multipart/form-data" {
                    input type="hidden" name="_action" value=(Self::IMPORT);
                    s-text-field name="label" label="Label" value=(form.value_or("label", "")) error=[form.error("label")] {}
                    s-drop-zone name="file" label="Members" accept=".csv" error=[form.error("file")] {}
                    s-button type="submit" { "Import" }
                }
            }
        }
    }
}

fn members() -> Upload {
    Upload::new(
        "members.csv",
        "text/csv",
        "email,points\na@example.com,10\nb@example.com,4\n",
    )
}

/// A file field left empty, as a browser sends it.
fn none_chosen() -> Upload {
    Upload::new("", "application/octet-stream", Vec::new())
}

#[tokio::test]
async fn an_action_takes_the_files_of_its_form() {
    let ctx = super::context().await;

    let image = Upload::new("logo.png", "image/png", [0x89, b'P', b'N', b'G']);
    let imported = ctx
        .submit_files(
            "/imports",
            &[("_action", ImportPage::IMPORT), ("label", "Spring")],
            &[
                ("file", &members()),
                ("note", &none_chosen()),
                ("images", &image),
                ("images", &image),
            ],
        )
        .send()
        .await
        .unwrap();
    assert_eq!(imported.status(), StatusCode::OK);
    assert_eq!(
        toast(&imported).as_deref(),
        Some("Spring: 2 rows of members.csv (text/csv), note none, images [logo.png 4, logo.png 4]")
    );
    let page = imported.text().await.unwrap();
    assert!(page.contains(r#"enctype="multipart/form-data""#), "{page}");

    // Without `images`: none. A field without a name or content type is a file all the same.
    let note = Upload::new("note.txt", "", "hi");
    let imported = ctx
        .submit_files(
            "/imports",
            &[("_action", ImportPage::IMPORT), ("label", "Bare")],
            &[("file", &members()), ("note", &note)],
        )
        .send()
        .await
        .unwrap();
    assert_eq!(
        toast(&imported).as_deref(),
        Some("Bare: 2 rows of members.csv (text/csv), note note.txt, images []")
    );

    // The JSON of an RPC call: the bytes in base64.
    let outcome: Outcome = ctx
        .call_action(
            "/imports",
            ImportPage::IMPORT,
            json!({ "label": "Rpc", "file": members() }),
        )
        .await
        .unwrap();
    assert_eq!(
        outcome.toast.as_deref(),
        Some("Rpc: 2 rows of members.csv (text/csv), note none, images []")
    );
}

#[tokio::test]
async fn a_file_field_left_empty_is_refused_as_required() {
    let ctx = super::context().await;

    let refused = ctx
        .submit_files(
            "/imports",
            &[("_action", ImportPage::IMPORT), ("label", "Spring")],
            &[("file", &none_chosen())],
        )
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let page = refused.text().await.unwrap();
    // The text fields come back; a file cannot be given back to the browser.
    assert!(
        page.contains(r#"<s-text-field name="label" label="Label" value="Spring"></s-text-field>"#),
        "{page}"
    );
    assert!(
        page.contains(r#"<s-drop-zone name="file" label="Members" accept=".csv" error="Required"></s-drop-zone>"#),
        "{page}"
    );

    // What the script sent before: a file as the text `[object File]`, which the schema refuses.
    let text = send(form(
        fetch(&ctx, reqwest::Method::POST, "/imports"),
        "_action=import&label=Spring&file=%5Bobject+File%5D",
    ))
    .await;
    assert_eq!(text.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        text.body
            .contains(r#"name="file" label="Members" accept=".csv" error="#),
        "{}",
        text.body
    );
}

#[tokio::test]
async fn a_form_larger_than_the_body_limit_is_refused() {
    let mut config = APP.config().unwrap();
    config.max_request_body_bytes = Some(4 * 1024);
    let ctx = TestContext::with_options(
        config,
        TestOptions {
            shop: SHOP.to_string(),
            access_token: Some("offline-token".to_string()),
            server: ServerOptions {
                ui: openshop::UiSource::Server,
                dev: false,
            },
            ..Default::default()
        },
    )
    .await
    .expect("test context");

    let large = Upload::new("large.csv", "text/csv", "x".repeat(8 * 1024));
    let refused = ctx
        .submit_files(
            "/imports",
            &[("_action", ImportPage::IMPORT), ("label", "Large")],
            &[("file", &large)],
        )
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        toast_error(&refused).as_deref(),
        Some("The form is too large to be sent.")
    );

    // Within the limit, larger than what a URL-encoded form may send.
    let fits = Upload::new("fits.csv", "text/csv", format!("email\n{}", "x".repeat(3 * 1024)));
    let imported = ctx
        .submit_files(
            "/imports",
            &[("_action", ImportPage::IMPORT), ("label", "Fits")],
            &[("file", &fits)],
        )
        .send()
        .await
        .unwrap();
    assert_eq!(imported.status(), StatusCode::OK);
    assert_eq!(
        toast(&imported).as_deref(),
        Some("Fits: 1 rows of fits.csv (text/csv), note none, images []")
    );
}

#[tokio::test]
async fn a_multipart_form_posted_to_mcp_is_a_form_of_its_page() {
    let ctx = super::context().await;

    // `/mcp` is the endpoint of MCP clients, which post JSON, and the page of MCP tokens.
    let unknown = ctx
        .submit_files("/mcp", &[("_action", "dropTables")], &[])
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    assert_eq!(toast_error(&unknown).as_deref(), Some("Not found"));
}
