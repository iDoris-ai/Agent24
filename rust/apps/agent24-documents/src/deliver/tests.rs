#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Cursor, Read, Write};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::Request;
use tower::ServiceExt;
use zip::write::SimpleFileOptions;
use zip::{ZipArchive, ZipWriter};

use super::*;
use crate::preflight::{Kind, docx_findings};

const W: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main""#;
const REVISED: &[u8] = include_bytes!("../../tests/fixtures/contract-formula-revision-break.docx");
const NOW: &str = "2026-11-26T12:00:00Z";
const DOC: &str = "doc_01K74Z3QJ8V5N2W9RTX6Y00031";
const COPY: &str = "doc_01K74Z3QJ8V5N2W9RTX6Y00032";
const PDF_COPY: &str = "doc_01K74Z3QJ8V5N2W9RTX6Y00033";
/// A second clean copy of the same version: its own modified time, another hash.
const PDF_COPY2: &str = "doc_01K74Z3QJ8V5N2W9RTX6Y00034";

fn docx(parts: &[(&str, String)]) -> Vec<u8> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, body) in parts {
        zip.start_file(*name, SimpleFileOptions::default()).unwrap();
        zip.write_all(body.as_bytes()).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

fn entry(docx: &[u8], name: &str) -> Option<String> {
    let mut archive = ZipArchive::new(Cursor::new(docx)).unwrap();
    let mut s = String::new();
    archive.by_name(name).ok()?.read_to_string(&mut s).unwrap();
    Some(s)
}

fn body(paras: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><w:document {W}><w:body>{paras}</w:body></w:document>"#
    )
}

const CORE: &str = r#"<?xml version="1.0" encoding="UTF-8"?><cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/"><dc:title>合同</dc:title><dc:creator>李四</dc:creator><cp:lastModifiedBy>Agent24</cp:lastModifiedBy><dcterms:modified>2026-01-01T00:00:00Z</dcterms:modified></cp:coreProperties>"#;

fn dirty() -> Vec<u8> {
    let paras = concat!(
        r#"<w:p><w:pPr><w:rPr><w:ins w:id="9" w:author="A"/></w:rPr></w:pPr>"#,
        r#"<w:r><w:rPr><w:b/><w:rPrChange w:id="8" w:author="A"><w:rPr/></w:rPrChange></w:rPr><w:t xml:space="preserve">第一条 </w:t></w:r>"#,
        r#"<w:ins w:id="1" w:author="Agent24"><w:r><w:t>新增的</w:t></w:r><w:r><w:t>条款</w:t></w:r></w:ins>"#,
        r#"<w:del w:id="2" w:author="Agent24"><w:r><w:delText>旧条款</w:delText></w:r></w:del></w:p>"#,
        r#"<w:p><w:commentRangeStart w:id="0"/><w:r><w:t>甲方：{{甲方}}</w:t></w:r><w:commentRangeEnd w:id="0"/>"#,
        r#"<w:r><w:rPr><w:rStyle w:val="CommentReference"/></w:rPr><w:commentReference w:id="0"/></w:r></w:p>"#,
        r#"<w:p><w:moveFromRangeStart w:id="5" w:name="m"/><w:moveFrom w:id="6"><w:r><w:t>挪走的</w:t></w:r></w:moveFrom><w:moveFromRangeEnd w:id="5"/></w:p>"#,
        r#"<w:p><w:moveToRangeStart w:id="7" w:name="m"/><w:moveTo w:id="10"><w:r><w:t>挪来的</w:t></w:r></w:moveTo><w:moveToRangeEnd w:id="7"/></w:p>"#,
        r#"<w:p><w:r><w:rPr><w:vanish/></w:rPr><w:t>内部备注</w:t></w:r></w:p>"#,
    );
    let comments = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><w:comments {W}><w:comment w:id="0" w:author="张三"><w:p><w:r><w:t>这里要改</w:t></w:r></w:p></w:comment></w:comments>"#
    );
    docx(&[
        ("word/document.xml", body(paras)),
        ("word/comments.xml", comments),
        ("docProps/core.xml", CORE.to_owned()),
        ("word/styles.xml", "<x/>".to_owned()),
    ])
}

fn kinds(docx: &[u8]) -> Vec<Kind> {
    docx_findings(docx)
        .unwrap()
        .iter()
        .map(|f| f.kind)
        .collect()
}

fn texts(docx: &[u8]) -> Vec<String> {
    crate::docx_patch::list_paragraphs(docx)
        .unwrap()
        .into_iter()
        .map(|p| p.text)
        .collect()
}

#[test]
fn a_clean_copy_has_no_revisions_comments_or_author_and_keeps_the_rest() {
    let src = dirty();
    let out = clean_docx(&src, NOW).unwrap();
    // What deliver removes is gone; what it only reports stays.
    assert_eq!(kinds(&out), [Kind::UnfilledFields, Kind::HiddenText]);
    assert_eq!(
        texts(&out),
        [
            "第一条 新增的条款",
            "甲方：{{甲方}}",
            "",
            "挪来的",
            "内部备注"
        ]
    );
    let doc = entry(&out, "word/document.xml").unwrap();
    for gone in [
        "w:ins",
        "w:del",
        "rPrChange",
        "commentRange",
        "commentReference",
        "moveFrom",
        "moveTo",
        "CommentReference",
    ] {
        assert!(!doc.contains(gone), "{gone} left in {doc}");
    }
    // The run property that was the change's target stays.
    assert!(doc.contains("<w:b/>"));
    let comments = entry(&out, "word/comments.xml").unwrap();
    assert!(comments.contains("w:comments") && !comments.contains("w:comment "));
    roxmltree::Document::parse(&comments).unwrap();
    let core = entry(&out, "docProps/core.xml").unwrap();
    assert!(
        !core.contains("李四") && !core.contains("Agent24"),
        "{core}"
    );
    assert!(core.contains("<dc:title>合同</dc:title>") && core.contains(NOW));
    // Every other part as it was, and the source not touched.
    assert_eq!(entry(&out, "word/styles.xml").unwrap(), "<x/>");
    assert_eq!(src, dirty());
}

#[test]
fn clean_part_keeps_a_part_without_revisions_byte_for_byte() {
    let plain = body(r#"<w:p><w:r><w:t xml:space="preserve"> 原样 &amp; 不动 </w:t></w:r></w:p>"#);
    assert_eq!(clean_part(&plain).unwrap(), plain);
    assert!(clean_part("<w:p").is_err());
}

#[test]
fn clean_core_empties_a_self_closing_creator_without_breaking_it() {
    let core = CORE.replace("<dc:creator>李四</dc:creator>", "<dc:creator/>");
    let out = clean_core(&core, NOW).unwrap();
    roxmltree::Document::parse(&out).unwrap();
    assert!(out.contains("<dc:creator/>") && !out.contains("Agent24"));
}

#[test]
fn the_real_revised_contract_comes_out_without_its_deletion() {
    assert!(kinds(REVISED).contains(&Kind::TrackedDeletions));
    let out = clean_docx(REVISED, NOW).unwrap();
    let after = kinds(&out);
    assert!(
        !after.contains(&Kind::TrackedDeletions) && !after.contains(&Kind::TrackedInsertions),
        "{after:?}"
    );
    assert_eq!(texts(&out).len(), texts(REVISED).len(), "no paragraph lost");
}

// ── the routes ──

async fn setup() -> (tempfile::TempDir, AppState, Arc<Storage>) {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::open(dir.path()).await;
    let storage = state.storage().await.unwrap();
    for (id, title, media, bytes) in [
        (DOC, "租赁合同.docx", DOCX, dirty()),
        (COPY, "租赁合同（交付）.docx", DOCX, b"docx copy".to_vec()),
        (PDF_COPY, "租赁合同（交付）.pdf", PDF, b"%PDF copy".to_vec()),
        (
            PDF_COPY2,
            "租赁合同（交付）.pdf",
            PDF,
            b"%PDF copy, later".to_vec(),
        ),
    ] {
        let sha = storage.blobs.put_bytes(&bytes).unwrap().sha256;
        sqlx::raw_sql(&format!(
            "INSERT INTO documents (id, title, media_type, head_revision) VALUES ('{id}', '{title}', '{media}', 1);
             INSERT INTO revisions (document_id, revision, content_sha256, size, media_type, origin)
                    VALUES ('{id}', 1, '{sha}', {n}, '{media}', 'import');",
            n = bytes.len()
        ))
        .execute(storage.db.pool())
        .await
        .unwrap();
    }
    (dir, state, storage)
}

async fn call(state: &AppState, req: Request<Body>) -> (StatusCode, Value) {
    let res = crate::router(state.clone()).oneshot(req).await.unwrap();
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), 1 << 22)
        .await
        .unwrap();
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

fn post(path: &str, body: Value) -> Request<Body> {
    Request::post(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn get(path: &str) -> Request<Body> {
    Request::get(path).body(Body::empty()).unwrap()
}

async fn settled(state: &AppState, job_id: &str) -> Value {
    let start = Instant::now();
    loop {
        let (status, v) = call(state, get(&format!("/jobs/{job_id}"))).await;
        assert_eq!(status, StatusCode::OK, "{v}");
        if !matches!(
            v["status"].as_str(),
            Some("queued" | "running" | "cancelling")
        ) {
            return v;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "job never settled: {v}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn file(state: &AppState, id: &str) -> Vec<u8> {
    let res = crate::router(state.clone())
        .oneshot(get(&format!("/documents/{id}/content")))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    axum::body::to_bytes(res.into_body(), 1 << 23)
        .await
        .unwrap()
        .to_vec()
}

#[tokio::test]
async fn a_delivery_is_a_new_clean_document_and_the_original_stays() {
    let (_d, state, storage) = setup().await;
    let (status, job) = call(
        &state,
        post(
            &format!("/documents/{DOC}/deliveries"),
            json!({"expect_revision": 1}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{job}");
    let done = settled(&state, job["job_id"].as_str().unwrap()).await;
    assert_eq!(done["status"], "succeeded", "{done}");
    let copy = done["result"]["document_id"].as_str().unwrap().to_owned();
    assert_ne!(copy, DOC);
    let title: String = sqlx::query_scalar("SELECT title FROM documents WHERE id = ?")
        .bind(&copy)
        .fetch_one(storage.db.pool())
        .await
        .unwrap();
    assert!(title.starts_with("租赁合同（交付）"), "{title}");
    assert_eq!(
        kinds(&file(&state, &copy).await),
        [Kind::UnfilledFields, Kind::HiddenText]
    );
    // The original: one version, its revisions and comments still there.
    assert_eq!(file(&state, DOC).await, dirty());
    let versions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM revisions WHERE document_id = ?")
        .bind(DOC)
        .fetch_one(storage.db.pool())
        .await
        .unwrap();
    assert_eq!(versions, 1);
    // No receipt until the file is saved.
    let (_, r) = call(&state, get("/receipts")).await;
    assert_eq!(r["receipts"], json!([]));
}

#[tokio::test]
async fn a_delivery_of_a_version_not_checked_or_of_a_pdf_is_refused() {
    let (_d, state, _s) = setup().await;
    let (status, v) = call(
        &state,
        post(
            &format!("/documents/{DOC}/deliveries"),
            json!({"expect_revision": 2}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["error"]["code"], "revision_conflict");
    let (status, v) = call(
        &state,
        post(
            &format!("/documents/{PDF_COPY}/deliveries"),
            json!({"expect_revision": 1}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
    let (status, _) = call(
        &state,
        post(&format!("/documents/{DOC}/deliveries"), json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

fn receipt(copy: &str, saved_to: &str) -> Value {
    json!({"revision": 1, "copy_document_id": copy, "saved_to": saved_to})
}

#[tokio::test]
async fn a_receipt_records_the_library_copys_hash_and_a_second_save_is_marked_again() {
    let (_d, state, storage) = setup().await;
    let path = format!("/documents/{DOC}/receipts");
    let (status, r1) = call(
        &state,
        post(
            &path,
            receipt(PDF_COPY, "/Users/u/Desktop/租赁合同（交付）.pdf"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{r1}");
    let sha: String =
        sqlx::query_scalar("SELECT content_sha256 FROM revisions WHERE document_id = ?")
            .bind(PDF_COPY)
            .fetch_one(storage.db.pool())
            .await
            .unwrap();
    assert_eq!(r1["sha256"], sha);
    assert_eq!(r1["format"], "pdf");
    assert_eq!(r1["file_name"], "租赁合同（交付）.pdf");
    assert_eq!(r1["revision"], 1);
    assert_eq!(r1["title"], "租赁合同.docx");
    assert_eq!(r1["again"], false);
    assert!(r1["at"].as_str().is_some_and(|a| a.starts_with("20")));

    let (_, r2) = call(
        &state,
        post(
            &path,
            receipt(PDF_COPY2, "/Users/u/Desktop/租赁合同（交付） (2).pdf"),
        ),
    )
    .await;
    assert_eq!(r2["again"], true);
    let (_, r3) = call(
        &state,
        post(
            &path,
            receipt(COPY, "/Users/u/Downloads/租赁合同（交付）.docx"),
        ),
    )
    .await;
    assert_eq!(
        (r3["format"].as_str(), r3["again"].as_bool()),
        (Some("docx"), Some(false))
    );

    let (status, listed) = call(&state, get(&path)).await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<&str> = listed["receipts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["file_name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "租赁合同（交付）.docx",
            "租赁合同（交付） (2).pdf",
            "租赁合同（交付）.pdf"
        ]
    );
    let (_, all) = call(&state, get("/receipts")).await;
    assert_eq!(all["receipts"].as_array().unwrap().len(), 3);
    let (_, other) = call(&state, get(&format!("/documents/{COPY}/receipts"))).await;
    assert_eq!(other["receipts"], json!([]));
}

#[tokio::test]
async fn a_receipt_needs_an_absolute_path_a_real_version_and_a_copy_in_the_library() {
    let (_d, state, _s) = setup().await;
    let path = format!("/documents/{DOC}/receipts");
    for bad in [
        receipt(COPY, "Desktop/x.docx"),
        receipt(COPY, "/Users/u/Desktop/"),
        receipt(COPY, "/Users/u/a\nb.docx"),
        json!({"revision": 1, "copy_document_id": "not-an-id", "saved_to": "/x.docx"}),
        json!({"revision": 1, "copy_document_id": COPY, "saved_to": "/x.docx", "sha256": "forged"}),
    ] {
        let (status, v) = call(&state, post(&path, bad.clone())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {v}");
    }
    let (status, _) = call(
        &state,
        post(
            &path,
            json!({"revision": 7, "copy_document_id": COPY, "saved_to": "/x.docx"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(
        &state,
        post(&path, receipt("doc_01K74Z3QJ8V5N2W9RTX6Y00039", "/x.docx")),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, all) = call(&state, get("/receipts")).await;
    assert_eq!(all["receipts"], json!([]));
}

#[test]
fn footnotes_endnotes_comment_authors_and_the_company_are_cleaned_too() {
    // PR-Daemon #2087 B1: read the copy's parts themselves, not through preflight.
    let note = |root: &str| {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><w:{root} {W}><w:footnote w:id="1"><w:p><w:r><w:t>脚注</w:t></w:r><w:del w:id="3" w:author="张三"><w:r><w:delText>脚注里删掉的底价</w:delText></w:r></w:del><w:ins w:id="4" w:author="张三"><w:r><w:t>新价</w:t></w:r></w:ins></w:p></w:footnote></w:{root}>"#
        )
    };
    let people = r#"<?xml version="1.0" encoding="UTF-8"?><w15:people xmlns:w15="http://schemas.microsoft.com/office/word/2012/wordml"><w15:person w15:author="张三"/></w15:people>"#;
    let ext = r#"<?xml version="1.0" encoding="UTF-8"?><w15:commentsEx xmlns:w15="http://schemas.microsoft.com/office/word/2012/wordml"><w15:commentEx w15:paraId="1" w15:done="0"/></w15:commentsEx>"#;
    let app = r#"<?xml version="1.0" encoding="UTF-8"?><Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties"><Company>内部公司</Company><Manager>王五</Manager><Pages>1</Pages></Properties>"#;
    let src = docx(&[
        ("word/document.xml", body("<w:p/>")),
        ("word/footnotes.xml", note("footnotes")),
        ("word/endnotes.xml", note("endnotes")),
        ("word/people.xml", people.to_owned()),
        ("word/commentsExtended.xml", ext.to_owned()),
        ("docProps/app.xml", app.to_owned()),
    ]);
    let out = clean_docx(&src, NOW).unwrap();
    for part in ["word/footnotes.xml", "word/endnotes.xml"] {
        let xml = entry(&out, part).unwrap();
        assert!(
            !xml.contains("底价")
                && !xml.contains("w:del")
                && !xml.contains("w:ins")
                && !xml.contains("张三"),
            "{xml}"
        );
        assert!(xml.contains("新价") && xml.contains("脚注"), "{xml}");
    }
    let people = entry(&out, "word/people.xml").unwrap();
    assert!(
        !people.contains("张三") && people.contains("w15:people"),
        "{people}"
    );
    assert!(
        !entry(&out, "word/commentsExtended.xml")
            .unwrap()
            .contains("commentEx ")
    );
    let app = entry(&out, "docProps/app.xml").unwrap();
    assert!(
        !app.contains("内部公司") && !app.contains("王五") && app.contains("<Pages>1</Pages>"),
        "{app}"
    );
}

#[tokio::test]
async fn again_is_only_the_same_document_version_and_format() {
    let (_d, state, storage) = setup().await;
    let other = "doc_01K74Z3QJ8V5N2W9RTX6Y00035";
    let sha: String =
        sqlx::query_scalar("SELECT content_sha256 FROM revisions WHERE document_id = ?")
            .bind(DOC)
            .fetch_one(storage.db.pool())
            .await
            .unwrap();
    sqlx::raw_sql(&format!(
        "INSERT INTO revisions (document_id, revision, content_sha256, size, media_type, origin)
                VALUES ('{DOC}', 2, '{sha}', 1, '{DOCX}', 'import');
         INSERT INTO documents (id, title, media_type, head_revision) VALUES ('{other}', '另一份.docx', '{DOCX}', 1);
         INSERT INTO revisions (document_id, revision, content_sha256, size, media_type, origin)
                VALUES ('{other}', 1, '{sha}', 1, '{DOCX}', 'import');"
    ))
    .execute(storage.db.pool())
    .await
    .unwrap();
    let save = |doc: &str, revision: i64| {
        let doc = doc.to_owned();
        let state = state.clone();
        async move {
            let body =
                json!({"revision": revision, "copy_document_id": PDF_COPY, "saved_to": "/x.pdf"});
            call(&state, post(&format!("/documents/{doc}/receipts"), body))
                .await
                .1
        }
    };
    assert_eq!(save(DOC, 1).await["again"], false);
    assert_eq!(save(DOC, 2).await["again"], false, "another version");
    assert_eq!(save(other, 1).await["again"], false, "another document");
    let r = save(DOC, 1).await;
    assert_eq!(
        (r["again"].as_bool(), r["revision"].as_i64()),
        (Some(true), Some(1))
    );
}
