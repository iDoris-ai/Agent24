//! L8-S02: `documents.fill` writes text and sourced identity, and says what
//! it left for the user.

use super::*;
use crate::form::{FieldKind, field_kind};
use crate::office::DOCX;

const TPL: &str = "doc_01K74Z3QJ8V5N2W9RTX6Y00091";
const SRC: &str = "doc_01K74Z3QJ8V5N2W9RTX6Y00092";

#[test]
fn field_kind_marks_identity_manual_and_text() {
    for name in ["姓名", "身份证号", "联系电话", "家庭住址", "银行卡号"] {
        assert_eq!(field_kind(name), FieldKind::Identity, "{name}");
    }
    for name in ["本人签名", "照片", "附件", "是否已婚☐"] {
        assert_eq!(field_kind(name), FieldKind::Manual, "{name}");
    }
    for name in ["部门", "入职日期"] {
        assert_eq!(field_kind(name), FieldKind::Text, "{name}");
    }
    // 「签名」 is manual even though it sits next to a name.
    assert_eq!(field_kind("签名"), FieldKind::Manual);
}

fn docx(paragraphs: &[&str]) -> Vec<u8> {
    let w = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main""#;
    let body: String = paragraphs
        .iter()
        .map(|p| format!(r#"<w:p><w:r><w:t xml:space="preserve">{p}</w:t></w:r></w:p>"#))
        .collect();
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document {w}><w:body>{body}</w:body></w:document>"#
    );
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    zip.start_file(
        "word/document.xml",
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    use std::io::Write;
    zip.write_all(xml.as_bytes()).unwrap();
    zip.finish().unwrap().into_inner()
}

async fn put_docx(state: &AppState, id: &str, title: &str, bytes: &[u8]) {
    let storage = state.storage().await.unwrap();
    let sha = storage.blobs.put_bytes(bytes).unwrap().sha256;
    exec(
        state,
        &format!(
            "INSERT INTO documents (id, title, media_type, head_revision)
                    VALUES ('{id}', '{title}', '{DOCX}', 1);
             INSERT INTO revisions (document_id, revision, content_sha256, size, media_type, origin)
                    VALUES ('{id}', 1, '{sha}', {n}, '{DOCX}', 'import');",
            n = bytes.len()
        ),
    )
    .await;
}

async fn uploads(state: &AppState) -> i64 {
    let storage = state.storage().await.unwrap();
    sqlx::query_scalar("SELECT COUNT(*) FROM uploads")
        .fetch_one(storage.db.pool())
        .await
        .unwrap()
}

async fn head_text(state: &AppState, id: &str) -> String {
    let storage = state.storage().await.unwrap();
    let sha: String = sqlx::query_scalar(
        "SELECT r.content_sha256 FROM documents d
         JOIN revisions r ON r.document_id = d.id AND r.revision = d.head_revision
         WHERE d.id = ?",
    )
    .bind(id)
    .fetch_one(storage.db.pool())
    .await
    .unwrap();
    let bytes = storage.blobs.read_verified(&sha).unwrap();
    crate::docx_patch::list_paragraphs(&bytes)
        .unwrap()
        .into_iter()
        .map(|p| p.text)
        .collect::<Vec<_>>()
        .join("\n")
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .into_iter()
        .flatten()
        .map(|s| s.as_str().unwrap().to_owned())
        .collect()
}

fn values() -> Value {
    json!({
        "姓名": "张三",
        "电话": "13800000000",
        "部门": "研发",
        "本人签名": "张三",
    })
}

#[tokio::test]
async fn fill_writes_only_text_and_identity_the_user_or_the_material_gave() {
    let (_dir, state) = opened().await;
    let template = docx(&["{{姓名}}{{电话}}{{部门}}{{本人签名}}{{紧急联系人}}"]);
    // The name is split by a space: matching ignores whitespace.
    let source = docx(&["入职资料 姓名：张 三"]);
    put_docx(&state, TPL, "入职表.docx", &template).await;
    put_docx(&state, SRC, "个人资料.docx", &source).await;

    let listed = payload(&call(&state, "fields", json!({ "document_id": TPL })).await);
    let kind_of = |name: &str| -> String {
        listed["fields"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["name"] == name)
            .unwrap()["kind"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    assert_eq!(kind_of("姓名"), "identity");
    assert_eq!(kind_of("电话"), "identity");
    assert_eq!(kind_of("紧急联系人"), "identity");
    assert_eq!(kind_of("部门"), "text");
    assert_eq!(kind_of("本人签名"), "manual");

    // Identity with no material and not spoken: nothing is written.
    let answer = payload(
        &call(
            &state,
            "fill",
            json!({ "document_id": TPL, "values": { "姓名": "张三", "电话": "13800000000" } }),
        )
        .await,
    );
    assert_eq!(answer["written"], false, "{answer}");
    assert_eq!(uploads(&state).await, 0);
    assert!(
        strings(&answer["ask_user"]).contains(&"姓名".into()),
        "{answer}"
    );

    // An empty text value is left unfilled, and still nothing is written.
    let answer = payload(
        &call(
            &state,
            "fill",
            json!({ "document_id": TPL, "values": { "部门": "" } }),
        )
        .await,
    );
    assert_eq!(answer["written"], false, "{answer}");
    assert!(
        strings(&answer["unfilled"]).contains(&"部门".into()),
        "{answer}"
    );
    assert_eq!(uploads(&state).await, 0);

    let mut args = json!({ "document_id": TPL, "source_document_id": SRC });
    args["values"] = values();
    let answer = payload(&call(&state, "fill", args).await);
    assert_eq!(answer["written"], true, "{answer}");
    assert_eq!(strings(&answer["filled"]), ["姓名", "部门"]);
    assert_eq!(strings(&answer["ask_user"]), ["电话", "紧急联系人"]);
    assert_eq!(strings(&answer["manual"]), ["本人签名"]);
    let note = answer["note"].as_str().unwrap();
    assert!(
        note.contains("已填 2 项：姓名、部门")
            && note.contains("电话")
            && note.contains("紧急联系人")
            && note.contains("本人签名")
            && note.contains("不要自己补"),
        "{note}"
    );
    let new_id = answer["result"]["document_id"].as_str().unwrap();
    let text = head_text(&state, new_id).await;
    assert!(text.contains("张三") && text.contains("研发"), "{text}");
    assert!(
        text.contains("{{电话}}") && text.contains("{{本人签名}}"),
        "{text}"
    );
    assert!(!text.contains("13800000000"), "{text}");

    // The user said the phone number: that identity field is written.
    let mut args = json!({
        "document_id": TPL,
        "source_document_id": SRC,
        "user_said": ["电话"],
    });
    args["values"] = values();
    let answer = payload(&call(&state, "fill", args).await);
    assert_eq!(strings(&answer["filled"]), ["姓名", "电话", "部门"]);
    assert_eq!(strings(&answer["ask_user"]), ["紧急联系人"]);
    let new_id = answer["result"]["document_id"].as_str().unwrap();
    let text = head_text(&state, new_id).await;
    assert!(text.contains("13800000000"), "{text}");
    assert!(!text.contains("{{电话}}"), "{text}");
    assert!(text.contains("{{本人签名}}"), "{text}");
}

#[tokio::test]
async fn a_source_outside_the_scope_is_not_found() {
    let (_dir, state) = opened().await;
    put_docx(&state, TPL, "入职表.docx", &docx(&["{{部门}}"])).await;
    put_docx(&state, SRC, "个人资料.docx", &docx(&["张三"])).await;
    let body = call_in(
        &state,
        "fill",
        &[TPL],
        json!({
            "document_id": TPL,
            "source_document_id": SRC,
            "values": { "部门": "研发" },
        }),
    )
    .await;
    assert_module_error(&body, "not_found", false);
}

/// Codex C1/C2: a PDF's text is compared as a DOCX's is: whitespace removed
/// on both sides, case kept.
#[test]
fn material_matches_without_whitespace_and_with_case() {
    let text = Material::Text(compact("姓名：张 三\n账号：AbC 123"));
    assert!(material_has(&text, "张三"));
    assert!(material_has(&text, "AbC123"));
    assert!(!material_has(&text, "abc123"));
    assert!(!material_has(&text, " "));
    assert!(!material_has(&Material::Absent, "张三"));
}
