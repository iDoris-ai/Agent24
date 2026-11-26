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
    let text = Material::Text(compact("姓名：张 三\n账号：AbC 123"), Default::default());
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
    let blocks = crate::docx_patch::blocks(&docx_body(&body)).unwrap();
    let names: Vec<String> = [
        "姓名",
        "部门",
        "入职日期",
        "联系电话",
        "身份证号",
        "紧急联系人",
    ]
    .map(String::from)
    .to_vec();
    let pairs = crate::form::labelled_blocks(&blocks, &names);
    let get = |f: &str| crate::form::from_source(f, &pairs).map(str::to_owned);
    assert_eq!(get("姓名").as_deref(), Some("陈晓明"));
    assert_eq!(get("部门").as_deref(), Some("产品研发部"));
    assert_eq!(get("入职日期").as_deref(), Some("2026年12月1日"));
    assert_eq!(get("联系电话"), None);
    assert_eq!(get("身份证号"), None);
    assert_eq!(get("紧急联系人"), None);
    // A manual field never takes a value from the material.
    let signed = crate::form::labelled(
        &["本人签名：陈晓明".to_owned()],
        &[],
        &["本人签名".to_owned()],
    );
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

    // PR-Daemon #2095 L4: the values are `fields`' own from_source, as the
    // model is told to pass them, plus the user's 「先空着」.
    let mut values = serde_json::Map::new();
    for f in listed["fields"].as_array().unwrap() {
        if let Some(v) = f.get("from_source") {
            values.insert(f["name"].as_str().unwrap().to_owned(), v.clone());
        }
    }
    values.insert("紧急联系人".to_owned(), json!(""));
    let answer = payload(
        &call(
            &state,
            "fill",
            json!({ "document_id": TPL, "source_document_id": SRC, "values": values }),
        )
        .await,
    );
    assert_eq!(answer["written"], true, "{answer}");
    assert_eq!(strings(&answer["filled"]), ["姓名", "部门", "入职日期"]);
    assert_eq!(strings(&answer["ask_user"]), ["联系电话", "身份证号"]);
    assert_eq!(strings(&answer["unfilled"]), ["紧急联系人"]);
    assert_eq!(strings(&answer["manual"]), ["本人签名"]);
}

/// Codex C1: a header row over a row of values is read by column, never as
/// 「姓名 → 联系电话」; Codex C2: several 「标签：值」 on one line are each their own.
#[test]
fn header_rows_and_several_labels_on_a_line_give_the_right_values() {
    let names: Vec<String> = ["姓名", "联系电话", "部门", "性别", "入职日期"]
        .map(String::from)
        .to_vec();
    let rows = vec![
        vec!["姓名".to_owned(), "联系电话".to_owned(), "部门".to_owned()],
        vec![
            "陈晓明".to_owned(),
            "13800000000".to_owned(),
            "研发部".to_owned(),
        ],
    ];
    let pairs = crate::form::labelled(&[], &rows, &names);
    let get =
        |p: &crate::form::Labelled, f: &str| crate::form::from_source(f, p).map(str::to_owned);
    assert_eq!(get(&pairs, "姓名").as_deref(), Some("陈晓明"), "{pairs:?}");
    assert_eq!(get(&pairs, "联系电话").as_deref(), Some("13800000000"));
    assert_eq!(get(&pairs, "部门").as_deref(), Some("研发部"));
    // r3 (监工): several fields on one line are not read: the user is asked.
    for line in [
        "姓名：陈晓明；联系电话：13800000000",
        "姓名：陈晓明    联系电话：13800000000",
        "姓名：陈晓明 联系电话：13800000000",
    ] {
        let pairs = crate::form::labelled(&[line.to_owned()], &[], &names);
        assert!(pairs.values.is_empty(), "{line}: {pairs:?}");
    }
    // A label cell beside another label is not its value.
    let pairs = crate::form::labelled(&[], &[vec!["姓名".to_owned(), "部门".to_owned()]], &names);
    assert_eq!(get(&pairs, "姓名"), None);
}

fn names(n: &[&str]) -> Vec<String> {
    n.iter().map(|s| (*s).to_owned()).collect()
}

fn cells(rows: &[&[&str]]) -> Vec<Vec<String>> {
    rows.iter().map(|r| names(r)).collect()
}

fn read(lines: &[&str], rows: &[&[&str]], fields: &[&str]) -> crate::form::Labelled {
    crate::form::labelled(&names(lines), &cells(rows), &names(fields))
}

fn value(read: &crate::form::Labelled, f: &str) -> Option<String> {
    crate::form::from_source(f, read).map(str::to_owned)
}

/// PR-Daemon #2095 B1: 「label | 值 | label | 值」 rows are not a header over
/// the next row; B1b: a header row with one template field is not read
/// sideways (姓名 ≠ 性别).
#[test]
fn label_value_rows_are_not_headers_and_a_header_is_never_read_sideways() {
    let r = read(
        &[],
        &[
            &["姓名", "陈晓明", "性别", "男"],
            &["部门", "产品研发部", "职位", "工程师"],
        ],
        &["姓名", "性别"],
    );
    assert_eq!(value(&r, "姓名").as_deref(), Some("陈晓明"), "{r:?}");
    assert_eq!(value(&r, "性别").as_deref(), Some("男"));
    // A row under 「label | 值」 cells whose words could be values reads
    // two ways (陈晓明 to the right, 研发 below): precision first (F1), none.
    let r = read(
        &[],
        &[
            &["姓名", "陈晓明", "性别", "男"],
            &["研发", "产品研发部", "高级", "工程师"],
        ],
        &["姓名", "性别"],
    );
    assert_eq!(value(&r, "姓名"), None, "{r:?}");
    assert_eq!(r.ambiguous, ["姓名", "性别"]);
    // A label to the right is never a value, with no row below too.
    let r = read(&[], &[&["姓名", "性别", "年龄"]], &["姓名"]);
    assert_eq!(value(&r, "姓名"), None, "{r:?}");
    let r = read(
        &[],
        &[&["姓名", "性别", "年龄"], &["陈晓明", "男", "30"]],
        &["姓名", "电话"],
    );
    assert_ne!(value(&r, "姓名").as_deref(), Some("性别"), "{r:?}");
    // A real header still reads by column.
    let r = read(
        &[],
        &[&["姓名", "联系电话"], &["陈晓明", "13800000000"]],
        &["姓名", "联系电话"],
    );
    assert_eq!(value(&r, "联系电话").as_deref(), Some("13800000000"));
}

/// PR-Daemon #2095 B2: a label with two different values gives none, and is
/// named ambiguous (the user's and a family member's, 甲方 and 乙方).
#[test]
fn a_label_with_two_values_is_ambiguous_and_gives_none() {
    let r = read(&["姓名：张三", "姓名：李四"], &[], &["姓名"]);
    assert_eq!(value(&r, "姓名"), None);
    assert_eq!(r.ambiguous, ["姓名"]);
    let r = read(
        &[
            "甲方",
            "姓名：张三",
            "电话：13800000001",
            "乙方",
            "姓名：李四",
            "电话：13800000002",
        ],
        &[],
        &["姓名", "电话"],
    );
    assert_eq!(
        (value(&r, "姓名"), value(&r, "电话")),
        (None, None),
        "{r:?}"
    );
    let r = read(
        &["姓名：陈晓明"],
        &[&["姓名", "关系", "电话"], &["李四", "父亲", "139"]],
        &["姓名", "电话"],
    );
    // r3 (监工): a family table with a cell that is no field is not read at
    // all, so the user's own 姓名 stands; 电话 (only in it) is not given.
    assert_eq!(value(&r, "姓名").as_deref(), Some("陈晓明"), "{r:?}");
    assert_eq!(value(&r, "电话"), None, "{r:?}");
    // The same value twice is one value.
    let r = read(&["姓名：陈晓明"], &[&["姓名", "陈晓明"]], &["姓名"]);
    assert_eq!(value(&r, "姓名").as_deref(), Some("陈晓明"));
}

/// PR-Daemon #2095 M1: a value is cut only where the next template field
/// starts; another colon in it (not a time's) gives none.
#[test]
fn a_value_is_cut_only_at_the_next_field_and_an_odd_colon_gives_none() {
    let r = read(
        &["住址：北京市海淀区 中关村大街1号（备注:近地铁）"],
        &[],
        &["住址"],
    );
    assert_eq!(value(&r, "住址"), None, "{r:?}");
    let r = read(&["时间：9:30"], &[], &["时间"]);
    assert_eq!(value(&r, "时间").as_deref(), Some("9:30"));
    let r = read(&["姓名：陈晓明 性别 男"], &[], &["姓名", "性别"]);
    assert_eq!(value(&r, "姓名"), None, "{r:?}");
    // r3 (监工): two fields on one line: neither is read.
    let r = read(&["姓名： 性别：男"], &[], &["姓名", "性别"]);
    assert_eq!((value(&r, "姓名"), value(&r, "性别")), (None, None));
    let r = read(&["地址：北京市 朝阳区 1号"], &[], &["地址"]);
    assert_eq!(value(&r, "地址").as_deref(), Some("北京市 朝阳区 1号"));
}

/// PR-Daemon #2095 M2: only the template's fields are read.
#[test]
fn only_the_template_s_fields_are_read() {
    let lines: Vec<String> = (0..5000).map(|i| format!("k{i}：v{i}")).collect();
    let r = crate::form::labelled(&lines, &[], &names(&["姓名"]));
    assert!(r.values.is_empty() && r.ambiguous.is_empty());
}

/// PR-Daemon #2095 M3: a blank to fill in is not a value.
#[test]
fn a_blank_to_fill_in_is_not_a_value() {
    for line in [
        "姓名：________",
        "姓名：（请填写）",
        "姓名：□",
        "姓名：……",
        "姓名：＿＿",
    ] {
        assert_eq!(
            value(&read(&[line], &[], &["姓名"]), "姓名"),
            None,
            "{line}"
        );
    }
    let r = read(&[], &[&["姓名", "____"]], &["姓名"]);
    assert_eq!(value(&r, "姓名"), None);
}

/// PR-Daemon #2095 L3: material that cannot be read says so.
#[tokio::test]
async fn material_that_cannot_be_read_says_so() {
    let (_dir, state) = opened().await;
    put_docx(
        &state,
        TPL,
        "入职登记表模板.docx",
        &docx(&["姓名：{{姓名}}"]),
    )
    .await;
    let storage = state.storage().await.unwrap();
    let bytes = b"not a document";
    let sha = storage.blobs.put_bytes(bytes).unwrap().sha256;
    exec(
        &state,
        &format!(
            "INSERT INTO documents (id, title, media_type, head_revision)
                    VALUES ('{SRC}', '资料.zip', 'application/zip', 1);
             INSERT INTO revisions (document_id, revision, content_sha256, size, media_type, origin)
                    VALUES ('{SRC}', 1, '{sha}', {n}, 'application/zip', 'import');",
            n = bytes.len()
        ),
    )
    .await;
    let listed = payload(
        &call(
            &state,
            "fields",
            json!({ "document_id": TPL, "source_document_id": SRC }),
        )
        .await,
    );
    let note = listed["note"].as_str().unwrap();
    assert!(note.starts_with("没能读到资料"), "{note}");
    assert!(listed["fields"][0].get("from_source").is_none());
}

/// PR-Daemon #2095 r2 F1: a header word outside any list is not a value:
/// 「姓名 | 职称」 over 「陈晓明 | 工程师」 reads both ways, so neither is given.
#[test]
fn a_header_word_outside_any_list_is_not_a_value() {
    let r = read(&[], &[&["姓名", "职称"], &["陈晓明", "工程师"]], &["姓名"]);
    assert_eq!(value(&r, "姓名"), None, "{r:?}");
    let r = read(
        &[],
        &[&["姓名", "毕业院校"], &["陈晓明", "北京大学"]],
        &["姓名"],
    );
    assert_eq!(value(&r, "姓名"), None, "{r:?}");
}

/// PR-Daemon #2095 r2 F2: someone else's identity (an emergency contact, a
/// spouse) is not the user's.
#[test]
fn someone_else_s_identity_is_not_the_user_s() {
    let f = &["姓名", "电话"];
    for line in [
        "紧急联系人 姓名：李四",
        "紧急联系人（姓名：李四，电话：139）",
    ] {
        let r = read(&[line], &[], f);
        assert_eq!(
            (value(&r, "姓名"), value(&r, "电话")),
            (None, None),
            "{line}: {r:?}"
        );
    }
    for rows in [
        &[&["配偶", "姓名", "李四"][..]][..],
        &[&["紧急联系人", "", "姓名", "李四"][..]][..],
        &[
            &["紧急联系人", ""][..],
            &["姓名", "电话"][..],
            &["李四", "139"][..],
        ][..],
    ] {
        let r = read(&[], rows, f);
        assert_eq!(
            (value(&r, "姓名"), value(&r, "电话")),
            (None, None),
            "{rows:?}: {r:?}"
        );
    }
}

/// PR-Daemon #2095 r2 F3: field names inside one another never panic.
#[test]
fn overlapping_field_names_never_panic() {
    let r = read(&["联系人、电话：139"], &[], &["联系人、电话", "电话"]);
    assert_eq!(value(&r, "联系人、电话").as_deref(), Some("139"), "{r:?}");
    let r = read(&["(1)姓名：陈晓明"], &[], &["(1)姓名", "姓名"]);
    assert_ne!(value(&r, "姓名").as_deref(), Some(""), "{r:?}");
}

/// PR-Daemon #2095 r2 F4: a value ends at two spaces, a tab or U+3000.
#[test]
fn a_value_spread_by_two_spaces_a_tab_or_an_ideographic_space_is_ambiguous() {
    // r3 (监工): never cut there; 「张三  部门 研发」 gives none, and is ambiguous.
    for line in [
        "姓名：张三  部门 研发",
        "姓名：张三\t部门 研发",
        "姓名：张三\u{3000}部门",
    ] {
        let r = read(&[line], &[], &["姓名"]);
        assert_eq!(value(&r, "姓名"), None, "{line}");
        assert_eq!(r.ambiguous, ["姓名"], "{line}");
    }
}

/// PR-Daemon #2095 r3 B1: a later field after someone else's label, in a
/// line or a table row, is not the user's either.
#[test]
fn a_later_field_after_someone_else_s_label_is_not_the_user_s() {
    let f = &["姓名", "电话", "性别"];
    let r = read(&[], &[&["紧急联系人", "姓名", "李四", "电话", "139"]], f);
    assert_eq!(
        (value(&r, "姓名"), value(&r, "电话")),
        (None, None),
        "{r:?}"
    );
    for line in [
        "姓名：陈晓明  紧急联系人 电话：139",
        "姓名：陈晓明；紧急联系人 电话：139",
        "姓名：陈晓明 紧急联系人 电话：139",
    ] {
        let r = read(&[line], &[], f);
        assert_eq!(value(&r, "电话"), None, "{line}: {r:?}");
        assert_ne!(
            value(&r, "姓名").as_deref(),
            Some("陈晓明；紧急联系人"),
            "{line}"
        );
        assert!(
            value(&r, "姓名").is_none_or(|v| v == "陈晓明"),
            "{line}: {r:?}"
        );
    }
    let r = read(&[], &[&["姓名", "陈晓明", "性别", "男"]], f);
    assert_eq!(
        (value(&r, "姓名").as_deref(), value(&r, "性别").as_deref()),
        (Some("陈晓明"), Some("男"))
    );
}

/// PR-Daemon #2095 r3 B2: a name spread by two spaces is never cut to one
/// character.
#[test]
fn a_name_spread_by_spaces_is_never_cut_to_one_character() {
    for line in ["姓名：陈  晓明", "姓名：张  三", "姓名：张\u{3000}三"] {
        let v = value(&read(&[line], &[], &["姓名"]), "姓名");
        assert!(
            v.as_deref()
                .is_none_or(|v| v.chars().filter(|c| !c.is_whitespace()).count() > 1),
            "{line}: {v:?}"
        );
    }
    for line in ["姓名：陈  晓明", "姓名：张  三", "姓名：张\u{3000}三"] {
        assert_eq!(
            value(&read(&[line], &[], &["姓名"]), "姓名"),
            None,
            "{line}"
        );
    }
    // A numbered line with one field is read.
    for line in ["1. 姓名：陈晓明", "（一）姓名：陈晓明", "  姓名：陈晓明"] {
        let v = value(&read(&[line], &[], &["姓名"]), "姓名");
        assert_eq!(v.as_deref(), Some("陈晓明"), "{line}");
    }
}

/// PR-Daemon #2095 r3 N1: a title row 「个人基本信息」 does not hide the table under it.
#[test]
fn a_title_row_does_not_hide_the_table_under_it() {
    let r = read(
        &[],
        &[&["个人基本信息", "", ""], &["姓名", "陈晓明", ""]],
        &["姓名"],
    );
    assert_eq!(value(&r, "姓名").as_deref(), Some("陈晓明"), "{r:?}");
    let r = read(&[], &[&["配偶", ""], &["姓名", "李四"]], &["姓名"]);
    assert_eq!(value(&r, "姓名"), None, "{r:?}");
}

/// 监工 11-26 真机验收的两份原件（tests/fixtures/s02-*.docx）: the three the
/// material states are read, the rest are not, nothing is ambiguous.
#[test]
fn the_acceptance_materials_give_the_three_values_they_state() {
    let tpl: &[u8] = include_bytes!("../../../tests/fixtures/s02-template.docx");
    let src: &[u8] = include_bytes!("../../../tests/fixtures/s02-material.docx");
    let names: Vec<String> = crate::docx_patch::list_fields(tpl)
        .unwrap()
        .into_iter()
        .map(|f| f.name)
        .collect();
    let blocks = crate::docx_patch::blocks(src).unwrap();
    // 监工: the material's first line is 「个人信息」, a heading of the user's own.
    assert!(
        matches!(blocks.first(), Some(crate::form::Block::Line(l)) if l.trim() == "个人信息"),
        "{blocks:?}"
    );
    let r = crate::form::labelled_blocks(&blocks, &names);
    assert_eq!(value(&r, "姓名").as_deref(), Some("陈晓明"));
    assert_eq!(value(&r, "部门").as_deref(), Some("产品研发部"));
    assert_eq!(value(&r, "入职日期").as_deref(), Some("2026年12月1日"));
    for f in ["联系电话", "身份证号", "紧急联系人", "本人签名"] {
        assert_eq!(value(&r, f), None, "{f}");
    }
    assert!(r.ambiguous.is_empty(), "{r:?}");
}

/// PR-Daemon #2095 r4 B1: a heading is someone else's unless it is plainly
/// the user's own (个人 / 本人 / 基本信息 / 申请人…): one outside any list
/// (紧急联络人、监护人、推荐人、介绍人) closes what follows, in a table and in
/// paragraphs.
#[test]
fn a_heading_is_someone_else_s_unless_plainly_the_user_s() {
    let f = &["姓名", "电话"];
    for title in [
        "紧急联络人",
        "监护人",
        "推荐人",
        "介绍人",
        "紧急联系人",
        "联系人",
        "担保人",
        "法定代表人",
        "委托代理人",
        "其他联系人",
        "配偶信息",
        "家庭成员信息",
        "直系亲属",
        "申请人家庭成员",
        "本人紧急联系人",
    ] {
        let r = read(&[], &[&[title], &["姓名", "李四"], &["电话", "139"]], f);
        assert_eq!(
            (value(&r, "姓名"), value(&r, "电话")),
            (None, None),
            "table {title}: {r:?}"
        );
        let r = read(&[title, "姓名：李四", "电话：139"], &[], f);
        assert_eq!(
            (value(&r, "姓名"), value(&r, "电话")),
            (None, None),
            "lines {title}: {r:?}"
        );
    }
    for title in ["个人基本信息", "本人信息", "申请人信息", "入职资料"] {
        let r = read(&[], &[&[title], &["姓名", "陈晓明"]], f);
        assert_eq!(
            value(&r, "姓名").as_deref(),
            Some("陈晓明"),
            "table {title}: {r:?}"
        );
        let r = read(&[title, "姓名：陈晓明"], &[], f);
        assert_eq!(
            value(&r, "姓名").as_deref(),
            Some("陈晓明"),
            "lines {title}: {r:?}"
        );
    }
    // The user's own, then someone else's: only the first.
    let r = read(&["姓名：陈晓明", "紧急联络人", "电话：139"], &[], f);
    assert_eq!(
        (value(&r, "姓名").as_deref(), value(&r, "电话")),
        (Some("陈晓明"), None)
    );
}

/// PR-Daemon #2095 r4 N2: an opening bracket is not a number: 「（姓名：李四）」
/// is not read.
#[test]
fn an_open_bracket_before_a_field_is_not_numbering() {
    let r = read(&["（姓名：李四）"], &[], &["姓名"]);
    assert_eq!(value(&r, "姓名"), None, "{r:?}");
    let r = read(&["（一）姓名：陈晓明"], &[], &["姓名"]);
    assert_eq!(value(&r, "姓名").as_deref(), Some("陈晓明"));
}

/// r4 (监工): paragraphs and tables share one reading state, in document
/// order: a paragraph heading closes the table under it, and the user's own
/// heading opens it again; a value in brackets loses them.
#[test]
fn paragraphs_and_tables_share_one_reading_state() {
    use crate::form::Block::{Line, Row};
    let f = names(&["姓名", "电话"]);
    let row = |c: &[&str]| Row(names(c));
    let r = crate::form::labelled_blocks(
        &[
            Line("紧急联络人".into()),
            row(&["姓名", "李四"]),
            row(&["电话", "139"]),
        ],
        &f,
    );
    assert_eq!(
        (value(&r, "姓名"), value(&r, "电话")),
        (None, None),
        "{r:?}"
    );
    let r = crate::form::labelled_blocks(
        &[
            Line("监护人".into()),
            Line("电话：139".into()),
            Line("个人基本信息".into()),
            row(&["姓名", "陈晓明"]),
        ],
        &f,
    );
    // r5 (监工): a material that does not open with the user's own fields
    // is not read at all, whatever comes later.
    assert_eq!(
        (value(&r, "姓名"), value(&r, "电话")),
        (None, None),
        "{r:?}"
    );
    assert!(r.layout_unknown);
    let r = read(&["姓名：（陈晓明）"], &[], &["姓名"]);
    assert_eq!(value(&r, "姓名").as_deref(), Some("陈晓明"));
}

/// r5 (监工, after PR-Daemon #2095 round 5): only the run of field blocks
/// opening the material is read. B1: a heading that is not exactly the
/// user's own (「共同申请人」「本人前雇主」) and nothing is read.
#[test]
fn a_heading_not_exactly_the_user_s_own_reads_nothing() {
    use crate::form::Block::{Line, Row};
    let f = names(&["姓名", "电话"]);
    for title in [
        "共同申请人",
        "第二申请人",
        "申请人（二）",
        "申请人兄弟姐妹",
        "申请人同事",
        "申请人室友",
        "本人亲友",
        "申请人同行人员",
        "本人及同住人",
        "本人前雇主",
        "个人信息（代填人）",
        "兄弟姐妹",
        "共同借款人",
    ] {
        let r = crate::form::labelled_blocks(
            &[
                Row(names(&[title])),
                Row(names(&["姓名", "李四"])),
                Row(names(&["电话", "139"])),
            ],
            &f,
        );
        assert!(
            r.values.is_empty() && r.layout_unknown,
            "table {title}: {r:?}"
        );
        let r = crate::form::labelled_blocks(
            &[
                Line(title.into()),
                Line("姓名：李四".into()),
                Line("电话：139".into()),
            ],
            &f,
        );
        assert!(
            r.values.is_empty() && r.layout_unknown,
            "lines {title}: {r:?}"
        );
    }
    for title in [
        "个人信息",
        "个人基本信息",
        "本人资料",
        "基本情况",
        "申请人信息",
        "入职登记表",
        "员工信息",
        "简历",
    ] {
        let r =
            crate::form::labelled_blocks(&[Line(title.into()), Line("姓名：陈晓明".into())], &f);
        assert_eq!(
            value(&r, "姓名").as_deref(),
            Some("陈晓明"),
            "{title}: {r:?}"
        );
    }
}

/// r5 B2: however a heading of someone else looks (a colon, a 「（选填）」
/// cell, a sentence), it ends the reading; a heading between fields too.
#[test]
fn whatever_follows_the_opening_fields_ends_the_reading() {
    use crate::form::Block::{Line, Row};
    let f = names(&["姓名", "电话"]);
    let cases: Vec<Vec<crate::form::Block>> = vec![
        vec![
            Line("紧急联系人：".into()),
            Line("姓名：李四".into()),
            Line("电话：139".into()),
        ],
        vec![
            Row(names(&["紧急联系人", "（选填）"])),
            Row(names(&["姓名", "李四"])),
            Row(names(&["电话", "139"])),
        ],
        vec![
            Line("紧急联系人（请填写亲属或朋友的姓名及电话以便联络）".into()),
            Line("姓名：李四".into()),
        ],
        vec![
            Line("如遇紧急情况，联系人信息".into()),
            Line("姓名：李四".into()),
        ],
    ];
    for blocks in &cases {
        let r = crate::form::labelled_blocks(blocks, &f);
        assert!(r.values.is_empty(), "{blocks:?}: {r:?}");
    }
    // The user's fields, then anything that is no field block: the rest is not read.
    for between in [
        "紧急联系人",
        "紧急联系人：",
        "毕业于北京大学",
        "联系人（选填）",
    ] {
        let r = crate::form::labelled_blocks(
            &[
                Line("姓名：陈晓明".into()),
                Line(between.into()),
                Line("电话：139".into()),
            ],
            &f,
        );
        assert_eq!(
            (value(&r, "姓名").as_deref(), value(&r, "电话")),
            (Some("陈晓明"), None),
            "{between}"
        );
    }
    // Control: plain field paragraphs with no heading, and blank lines among them.
    let r = crate::form::labelled_blocks(
        &[
            Line("".into()),
            Line("姓名：陈晓明".into()),
            Line("".into()),
            Line("电话：13800000000".into()),
        ],
        &f,
    );
    assert_eq!(
        (value(&r, "姓名").as_deref(), value(&r, "电话").as_deref()),
        (Some("陈晓明"), Some("13800000000"))
    );
    assert!(!r.layout_unknown);
}

/// r5: `fields` says when the material's layout is not plainly the user's.
#[tokio::test]
async fn fields_say_when_the_material_is_not_plainly_the_user_s() {
    let (_dir, state) = opened().await;
    put_docx(
        &state,
        TPL,
        "入职登记表模板.docx",
        &docx(&["姓名：{{姓名}}"]),
    )
    .await;
    put_docx(
        &state,
        SRC,
        "资料.docx",
        &docx(&["紧急联系人", "姓名：李四"]),
    )
    .await;
    let listed = payload(
        &call(
            &state,
            "fields",
            json!({ "document_id": TPL, "source_document_id": SRC }),
        )
        .await,
    );
    let note = listed["note"].as_str().unwrap();
    assert!(note.starts_with("资料版式无法确定是本人信息"), "{note}");
    assert!(listed["fields"][0].get("from_source").is_none());
}

/// PR-Daemon #2095 r6 B1: a header over two or more rows of data is a list
/// (a roster, a family table): whose the first row is cannot be told.
#[test]
fn a_header_over_several_rows_is_a_list_and_ambiguous() {
    use crate::form::Block::{Line, Row};
    let row = |c: &[&str]| Row(names(c));
    let f = names(&["姓名", "电话", "关系"]);
    for blocks in [
        vec![
            row(&["姓名", "电话"]),
            row(&["陈晓明", "138"]),
            row(&["李四", "139"]),
            row(&["王五", "137"]),
        ],
        vec![
            row(&["姓名", "关系", "电话"]),
            row(&["李四", "父亲", "139"]),
            row(&["王五", "母亲", "138"]),
        ],
        vec![
            Line("个人信息".into()),
            row(&["姓名", "电话"]),
            row(&["陈晓明", "138"]),
            row(&["李四", "139"]),
        ],
    ] {
        let r = crate::form::labelled_blocks(&blocks, &f);
        assert!(r.values.is_empty(), "{blocks:?}: {r:?}");
        assert!(r.ambiguous.contains(&"姓名".to_owned()), "{r:?}");
    }
    // Control: a header and its one row, then no more data, is read.
    for after in [None, Some(Line("以上信息属实".into()))] {
        let mut blocks = vec![row(&["姓名", "电话"]), row(&["陈晓明", "138"])];
        blocks.extend(after);
        let r = crate::form::labelled_blocks(&blocks, &f);
        assert_eq!(
            (value(&r, "姓名").as_deref(), value(&r, "电话").as_deref()),
            (Some("陈晓明"), Some("138")),
            "{blocks:?}"
        );
    }
}

/// r6 N1: a heading's suffixes come off until none is left
/// (「个人信息表」「个人基本情况表」).
#[test]
fn every_suffix_of_an_own_heading_comes_off() {
    use crate::form::Block::Line;
    for title in [
        "个人信息表",
        "员工信息表",
        "个人资料表",
        "申请人情况表",
        "个人基本情况表",
        "基本信息表",
        "个人信息登记表",
    ] {
        let r = crate::form::labelled_blocks(
            &[Line(title.into()), Line("姓名：陈晓明".into())],
            &names(&["姓名"]),
        );
        assert_eq!(
            value(&r, "姓名").as_deref(),
            Some("陈晓明"),
            "{title}: {r:?}"
        );
    }
}
