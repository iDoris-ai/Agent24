#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Cursor, Write};

use lopdf::{Document, Object, Stream, dictionary};
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

use super::*;

const W: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main""#;
const LEASE: &[u8] = include_bytes!("../../tests/fixtures/template-lease.docx");
const REVISED: &[u8] = include_bytes!("../../tests/fixtures/contract-formula-revision-break.docx");

fn docx(parts: &[(&str, String)]) -> Vec<u8> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, body) in parts {
        zip.start_file(*name, SimpleFileOptions::default()).unwrap();
        zip.write_all(body.as_bytes()).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

fn body(paras: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><w:document {W}><w:body>{paras}</w:body></w:document>"#
    )
}

fn core(creator: &str, last: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:creator>{creator}</dc:creator><cp:lastModifiedBy>{last}</cp:lastModifiedBy></cp:coreProperties>"#
    )
}

fn dirty() -> Vec<u8> {
    let paras = concat!(
        // A paragraph mark marked inserted: no text, not a revision of its own.
        r#"<w:p><w:pPr><w:rPr><w:ins w:id="9" w:author="A"/></w:rPr></w:pPr>"#,
        r#"<w:r><w:t>第一条 </w:t></w:r>"#,
        r#"<w:ins w:id="1" w:author="Agent24"><w:r><w:t>新增的</w:t></w:r><w:r><w:t>条款</w:t></w:r></w:ins>"#,
        r#"<w:del w:id="2" w:author="Agent24"><w:r><w:delText>旧条款</w:delText></w:r></w:del></w:p>"#,
        r#"<w:p><w:r><w:t>甲方：{{甲方}}，乙方：{{乙方}}，再写一次 {{甲方}}</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:rPr><w:vanish/></w:rPr><w:t>内部备注：底价 90 万</w:t></w:r>"#,
        r#"<w:r><w:rPr><w:vanish w:val="0"/></w:rPr><w:t>看得见</w:t></w:r>"#,
        r#"<w:r><w:rPr><w:vanish w:val="false"/></w:rPr><w:t>也看得见</w:t></w:r></w:p>"#,
        // Hidden paragraph mark only: the text stays visible.
        r#"<w:p><w:pPr><w:rPr><w:vanish/></w:rPr></w:pPr><w:r><w:t>正文</w:t></w:r></w:p>"#,
    );
    let header = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><w:hdr {W}><w:p><w:r><w:t>日期 {{{{日期}}}}</w:t></w:r></w:p></w:hdr>"#
    );
    let comments = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><w:comments {W}><w:comment w:id="0" w:author="张三"><w:p><w:r><w:t>这里要改</w:t></w:r></w:p></w:comment><w:comment w:id="1" w:author="李四"><w:p><w:r><w:t>价格</w:t></w:r></w:p><w:p><w:r><w:t>再谈</w:t></w:r></w:p></w:comment></w:comments>"#
    );
    docx(&[
        ("word/document.xml", body(paras)),
        ("word/header1.xml", header),
        ("word/comments.xml", comments),
        ("docProps/core.xml", core("李四", "Agent24")),
    ])
}

fn find(findings: &[Finding], kind: Kind) -> Option<&Finding> {
    findings.iter().find(|f| f.kind == kind)
}

#[test]
fn every_kind_of_a_dirty_docx_is_listed_in_order_with_counts_and_examples() {
    let f = docx_findings(&dirty()).unwrap();
    let kinds: Vec<Kind> = f.iter().map(|f| f.kind).collect();
    assert_eq!(
        kinds,
        [
            Kind::TrackedInsertions,
            Kind::TrackedDeletions,
            Kind::Comments,
            Kind::UnfilledFields,
            Kind::Author,
            Kind::LastModifiedBy,
            Kind::HiddenText,
        ]
    );
    let ins = find(&f, Kind::TrackedInsertions).unwrap();
    assert_eq!(
        (ins.count, ins.examples.as_slice()),
        (1, &["新增的条款".to_owned()][..])
    );
    let del = find(&f, Kind::TrackedDeletions).unwrap();
    assert_eq!(
        (del.count, del.examples.as_slice()),
        (1, &["旧条款".to_owned()][..])
    );
    let c = find(&f, Kind::Comments).unwrap();
    assert_eq!(c.count, 2);
    assert_eq!(c.examples, ["张三：这里要改", "李四：价格 再谈"]);
    let fields = find(&f, Kind::UnfilledFields).unwrap();
    assert_eq!(fields.count, 3);
    assert_eq!(fields.examples, ["{{甲方}}", "{{乙方}}", "{{日期}}"]);
    assert_eq!(find(&f, Kind::Author).unwrap().examples, ["李四"]);
    assert_eq!(
        find(&f, Kind::LastModifiedBy).unwrap().examples,
        ["Agent24"]
    );
    let hidden = find(&f, Kind::HiddenText).unwrap();
    assert_eq!(
        (hidden.count, hidden.examples.as_slice()),
        (1, &["内部备注：底价 90 万".to_owned()][..])
    );
    assert!(
        f.iter()
            .all(|f| f.count >= 1 && f.examples.len() <= EXAMPLES)
    );
}

#[test]
fn a_clean_docx_has_no_findings_and_empty_properties_are_not_findings() {
    let clean = docx(&[
        (
            "word/document.xml",
            body(r#"<w:p><w:r><w:t>正文</w:t></w:r></w:p>"#),
        ),
        ("docProps/core.xml", core("", " ")),
    ]);
    assert_eq!(docx_findings(&clean).unwrap(), []);
    // No core.xml and no comments.xml at all: also nothing.
    let bare = docx(&[("word/document.xml", body("<w:p/>"))]);
    assert_eq!(docx_findings(&bare).unwrap(), []);
}

#[test]
fn examples_are_at_most_three_and_clipped() {
    let long = "很长的一段".repeat(30);
    let paras: String = (0..5)
        .map(|i| {
            format!(
                r#"<w:p><w:ins w:id="{i}"><w:r><w:t>{long}
{i}</w:t></w:r></w:ins></w:p>"#
            )
        })
        .collect();
    let f = docx_findings(&docx(&[("word/document.xml", body(&paras))])).unwrap();
    let ins = find(&f, Kind::TrackedInsertions).unwrap();
    assert_eq!(ins.count, 5);
    assert_eq!(ins.examples.len(), EXAMPLES);
    for e in &ins.examples {
        assert_eq!(e.chars().count(), CLIP + 1);
        assert!(e.ends_with('…') && !e.contains('\n'));
    }
}

#[test]
fn the_real_fixtures_agree_with_what_they_hold() {
    let lease = docx_findings(LEASE).unwrap();
    let fields = find(&lease, Kind::UnfilledFields).unwrap();
    let listed = crate::docx_patch::list_fields(LEASE).unwrap();
    assert_eq!(fields.count, listed.len());
    assert_eq!(
        find(&lease, Kind::Author).unwrap().examples,
        ["python-docx"]
    );
    assert!(find(&lease, Kind::TrackedDeletions).is_none());

    let revised = docx_findings(REVISED).unwrap();
    assert!(find(&revised, Kind::TrackedDeletions).is_some_and(|d| d.count >= 1));
}

#[test]
fn not_a_docx_is_an_error() {
    assert!(docx_findings(b"not a zip").is_err());
    assert!(pdf_findings(b"%PDF-1.5 broken").is_err());
}

fn pdf(author: Option<&str>, form: bool, annots: &[&str]) -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let content = doc.add_object(Stream::new(dictionary! {}, b"BT ET".to_vec()));
    let annots: Vec<Object> = annots
        .iter()
        .map(|s| {
            doc.add_object(dictionary! { "Type" => "Annot", "Subtype" => *s, "Rect" => vec![0.into(), 0.into(), 10.into(), 10.into()] })
                .into()
        })
        .collect();
    let mut page = dictionary! { "Type" => "Page", "Parent" => pages_id, "Contents" => content };
    if !annots.is_empty() {
        page.set("Annots", annots);
    }
    let page_id = doc.add_object(page);
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages", "Kids" => vec![page_id.into()], "Count" => 1,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        }),
    );
    let mut catalog = dictionary! { "Type" => "Catalog", "Pages" => pages_id };
    if form {
        let field =
            doc.add_object(dictionary! { "FT" => "Tx", "T" => Object::string_literal("name") });
        catalog.set("AcroForm", dictionary! { "Fields" => vec![field.into()] });
    }
    let catalog_id = doc.add_object(catalog);
    doc.trailer.set("Root", catalog_id);
    if let Some(a) = author {
        let info = doc.add_object(dictionary! { "Author" => Object::string_literal(a) });
        doc.trailer.set("Info", info);
    }
    let mut out = Vec::new();
    doc.save_to(&mut out).unwrap();
    out
}

#[test]
fn a_pdf_reports_its_author_form_and_annotations_but_not_links() {
    let f = pdf_findings(&pdf(Some("Wang Wu"), true, &["Link", "Text", "Highlight"])).unwrap();
    let kinds: Vec<Kind> = f.iter().map(|f| f.kind).collect();
    assert_eq!(kinds, [Kind::Author, Kind::PdfForm, Kind::PdfAnnotations]);
    assert_eq!(f[0].examples, ["Wang Wu"]);
    assert_eq!(f[1].count, 1);
    assert_eq!(f[2].count, 2);
    assert!(
        pdf_findings(&pdf(None, false, &["Link"]))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn clip_flattens_whitespace() {
    assert_eq!(clip("  a\n\tb  "), "a b");
}

fn note_of(format: &'static str, findings: Vec<Finding>) -> String {
    let p = Preflight {
        document_id: "doc_x".into(),
        revision: 1,
        title: None,
        format,
        clean: findings.is_empty(),
        cleanable: format == "docx",
        findings,
    };
    for_the_model(p)["note"].as_str().unwrap().to_owned()
}

#[test]
fn the_note_says_what_was_not_checked_and_promises_no_cleaning() {
    // PR-Daemon #2086 B1: never 「放心」; PDF says what it did not check.
    let of_pdf = note_of("pdf", Vec::new());
    assert!(
        of_pdf.contains("只检查了作者") && of_pdf.contains("没有检查"),
        "{of_pdf}"
    );
    let docx = note_of("docx", Vec::new());
    assert!(docx.contains("脚注") && docx.contains("没有检查"), "{docx}");
    for n in [&of_pdf, &docx] {
        assert!(n.contains("不要说「放心发出」"), "{n}");
    }
    // B2 (PR2 on): the copy is the App's button, never the model's.
    let dirty = note_of("docx", docx_findings(&dirty()).unwrap());
    assert!(
        dirty.contains("App 会给出按钮") && dirty.contains("你自己不能做"),
        "{dirty}"
    );
    // PR-Daemon #2086 (non-blocking): no affirmative 「放心发出」 anywhere,
    // a dirty PDF too; a Popup alone counts 0, a Text alone 1.
    let dirty_pdf = note_of(
        "pdf",
        pdf_findings(&pdf(Some("A"), false, &["Text"])).unwrap(),
    );
    for n in [&of_pdf, &docx, &dirty, &dirty_pdf] {
        assert!(!n.replace("不要说「放心发出」", "").contains("放心"), "{n}");
    }
    assert!(dirty_pdf.contains("PDF 不能清理"), "{dirty_pdf}");
    let count = |annots: &[&str]| {
        pdf_findings(&pdf(None, false, annots))
            .unwrap()
            .iter()
            .find(|f| f.kind == Kind::PdfAnnotations)
            .map_or(0, |f| f.count)
    };
    assert_eq!(
        (
            count(&["Text"]),
            count(&["Popup"]),
            count(&["Text", "Popup"])
        ),
        (1, 0, 1)
    );
}
