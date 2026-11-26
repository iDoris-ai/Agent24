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

fn para(p: &str) -> String {
    format!(r#"<w:p><w:r><w:t xml:space="preserve">{p}</w:t></w:r></w:p>"#)
}

fn docx(paragraphs: &[&str]) -> Vec<u8> {
    docx_body(&paragraphs.iter().map(|p| para(p)).collect::<String>())
}

fn docx_body(body: &str) -> Vec<u8> {
    let w = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main""#;
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
    let text = Material::Text(compact("姓名：张 三\n账号：AbC 123"), Vec::new());
    assert!(material_has(&text, "张三"));
    assert!(material_has(&text, "AbC123"));
    assert!(!material_has(&text, "abc123"));
    assert!(!material_has(&text, " "));
    assert!(!material_has(&Material::Absent, "张三"));
}

/// PR-Daemon #2092 B1: the identity names its review found as `text`.
#[test]
fn common_identity_names_and_english_whole_words_are_identity() {
    for name in [
        "户名",
        "收款人",
        "持卡人",
        "开户行",
        "邮箱",
        "电子邮件",
        "出生日期",
        "性别",
        "学号",
        "微信",
        "甲方",
        "乙方",
        "承租人",
        "出租方",
        "法定代表人",
        "申请人",
        "工作单位",
        "Name",
        "Phone",
        "Email",
        "Address",
        "ID",
        "full_name",
        "EmailAddress",
    ] {
        assert_eq!(field_kind(name), FieldKind::Identity, "{name}");
    }
    for name in ["valid", "width", "Amount", "面积", "月租金", "签订日期"] {
        assert_eq!(field_kind(name), FieldKind::Text, "{name}");
    }
}

/// PR-Daemon #2092 B2: a failed import is not written, and says so.
#[test]
fn a_failed_import_is_not_written() {
    let fields = vec![("部门".to_owned(), FieldKind::Text)];
    let values = std::collections::BTreeMap::from([("部门".to_owned(), "研发".to_owned())]);
    let report = crate::form::plan_fill(&fields, &values, &[], &Default::default());
    let mut failed = json!({ "status": "failed", "note": "没能生成新文档（导入失败）。" });
    assert!(!made_document(&failed));
    let made = made_document(&failed);
    stamp_fill(&mut failed, &report, made);
    assert_eq!(failed["written"], false);
    assert_eq!(failed["filled"], json!([]));
    let note = failed["note"].as_str().unwrap();
    assert!(
        note.starts_with("没能生成新文档") && !note.contains("已填"),
        "{note}"
    );
    let mut ok = json!({ "status": "succeeded", "note": "已完成。" });
    let made = made_document(&ok);
    stamp_fill(&mut ok, &report, made);
    assert_eq!(ok["written"], true);
    assert_eq!(ok["filled"], json!(["部门"]));
}

/// L8-S02 round 2: 「标签：值」 in paragraphs (either colon), and a label cell
/// beside its value cell; an empty value or a `{{field}}` is none.
#[test]
fn the_material_s_labelled_values_are_read_from_lines_and_table_rows() {
    let row = |c: &[&str]| -> String {
        let cells: String = c
            .iter()
            .map(|t| format!("<w:tc>{}</w:tc>", para(t)))
            .collect();
        format!("<w:tr>{cells}</w:tr>")
    };
    let body = format!(
        "{}{}{}{}<w:tbl>{}{}</w:tbl>",
        para("姓名：陈晓明"),
        para("部门: 产品研发部"),
        para("联系电话："),
        para("身份证号：{{身份证号}}"),
        row(&["入职日期", "2026年12月1日"]),
        row(&["紧急联系人：", ""]),
    );
    let (lines, rows) = crate::docx_patch::lines_and_rows(&docx_body(&body)).unwrap();
    let pairs = crate::form::labelled(&lines, &rows);
    let get = |f: &str| crate::form::from_source(f, &pairs).map(str::to_owned);
    assert_eq!(get("姓名").as_deref(), Some("陈晓明"));
    assert_eq!(get("部门").as_deref(), Some("产品研发部"));
    assert_eq!(get("入职日期").as_deref(), Some("2026年12月1日"));
    assert_eq!(get("联系电话"), None);
    assert_eq!(get("身份证号"), None);
    assert_eq!(get("紧急联系人"), None);
    // A manual field never takes a value from the material.
    let signed = crate::form::labelled(&["本人签名：陈晓明".to_owned()], &[]);
    assert_eq!(crate::form::from_source("本人签名", &signed), None);
}

/// 监工 11-26 真机 ①: the acceptance materials. `fields` with the material
/// gives the values it states; `fill` with them writes all three, the
/// identity one too (it is in the material), and asks only for the rest.
#[tokio::test]
async fn fields_with_the_material_give_its_values_and_fill_writes_them() {
    let (_dir, state) = opened().await;
    let template = docx(&[
        "姓名：{{姓名}}",
        "部门：{{部门}}",
        "入职日期：{{入职日期}}",
        "联系电话：{{联系电话}}",
        "身份证号：{{身份证号}}",
        "紧急联系人：{{紧急联系人}}",
        "本人签名：{{本人签名}}",
    ]);
    let source = docx(&[
        "姓名：陈晓明",
        "部门：产品研发部",
        "入职日期：2026年12月1日",
        "爱好：跑步、摄影",
    ]);
    put_docx(&state, TPL, "入职登记表模板.docx", &template).await;
    put_docx(&state, SRC, "个人信息.docx", &source).await;

    let listed = payload(
        &call(
            &state,
            "fields",
            json!({ "document_id": TPL, "source_document_id": SRC }),
        )
        .await,
    );
    let from = |name: &str| {
        listed["fields"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["name"] == name)
            .unwrap()
            .get("from_source")
            .cloned()
    };
    assert_eq!(from("姓名"), Some(json!("陈晓明")), "{listed}");
    assert_eq!(from("部门"), Some(json!("产品研发部")));
    assert_eq!(from("入职日期"), Some(json!("2026年12月1日")));
    assert_eq!(from("联系电话"), None);
    assert_eq!(from("本人签名"), None);
    let note = listed["note"].as_str().unwrap();
    assert!(
        note.contains("姓名=陈晓明") && note.contains("联系电话"),
        "{note}"
    );
    // Without the material, `fields` answers as before.
    let plain = payload(&call(&state, "fields", json!({ "document_id": TPL })).await);
    assert!(plain.get("note").is_none() && plain["fields"][0].get("from_source").is_none());

    let answer = payload(
        &call(
            &state,
            "fill",
            json!({
                "document_id": TPL,
                "source_document_id": SRC,
                "values": { "姓名": "陈晓明", "部门": "产品研发部", "入职日期": "2026年12月1日", "紧急联系人": "" },
            }),
        )
        .await,
    );
    assert_eq!(answer["written"], true, "{answer}");
    assert_eq!(strings(&answer["filled"]), ["姓名", "部门", "入职日期"]);
    assert_eq!(strings(&answer["ask_user"]), ["联系电话", "身份证号"]);
    assert_eq!(strings(&answer["unfilled"]), ["紧急联系人"]);
    assert_eq!(strings(&answer["manual"]), ["本人签名"]);
}
