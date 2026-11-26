//! L8 DOC-4 (first slice): what a document still carries before it is sent
//! out. Read-only: it reports, it never changes the document.
//!
//! A DOCX is checked for tracked changes (`w:ins` / `w:del`), comments
//! (`word/comments.xml`), `{{fields}}` left unfilled, its author and last
//! modifier (`docProps/core.xml`) and hidden text (runs with `w:vanish`), in
//! the body, headers and footers. A PDF is only checked for its author, a
//! form (`/AcroForm` with fields) and annotations other than links; nothing
//! of a PDF is cleaned (loop-8 plan, item 1).
//!
//! `GET /documents/{id}/preflight` and the `preflight` tool answer the same.

use std::io::{Cursor, Read};
use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path as UrlPath, State};
use serde::Serialize;
use zip::ZipArchive;

use crate::docx_patch::{MAX_DOCUMENT_XML_BYTES, PatchError, list_fields, text_parts};
use crate::error::ApiError;
use crate::office::DOCX;
use crate::state::{AppState, Storage, blocking};

const PDF: &str = "application/pdf";
const WML: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
/// Examples kept per kind, and characters of each.
pub const EXAMPLES: usize = 3;
pub const CLIP: usize = 60;

/// One kind of thing found. Only kinds found at least once are listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub kind: Kind,
    /// How many: revisions, comments, hidden runs, distinct unfilled fields
    /// (the first ones as `{{name}}` in `examples`), 1 for a property or a form.
    pub count: usize,
    /// At most [`EXAMPLES`], each at most [`CLIP`] characters (then `…`):
    /// the text inserted / deleted / hidden, `author：text` of a comment,
    /// `{{name}}` of a field, the property's value.
    pub examples: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    TrackedInsertions,
    TrackedDeletions,
    Comments,
    UnfilledFields,
    Author,
    LastModifiedBy,
    HiddenText,
    PdfForm,
    PdfAnnotations,
}

#[derive(Debug, Serialize)]
pub struct Preflight {
    pub document_id: String,
    pub revision: i64,
    pub title: Option<String>,
    /// `docx` or `pdf`.
    pub format: &'static str,
    /// Nothing found.
    pub clean: bool,
    pub findings: Vec<Finding>,
    /// A DOCX (a clean copy can be made of it: `deliver.rs`, from the
    /// conversation's card); a PDF is only reported.
    pub cleanable: bool,
}

/// The findings of a DOCX, in the order of [`Kind`].
pub fn docx_findings(docx: &[u8]) -> Result<Vec<Finding>, PatchError> {
    let parts = text_parts(docx)?;
    let mut n_ins = 0usize;
    let mut ins_ex = Vec::new();
    let mut n_del = 0usize;
    let mut del_ex = Vec::new();
    let mut n_hidden = 0usize;
    let mut hidden_ex = Vec::new();
    for (_, xml) in &parts {
        let tree = parse_xml(xml)?;
        for node in tree.descendants() {
            if is_w(node, "ins") && !parent_is_rpr(node) {
                n_ins += 1;
                push_example(&mut ins_ex, clip(&join_w(node, "t")));
            } else if is_w(node, "del") && !parent_is_rpr(node) {
                n_del += 1;
                push_example(&mut del_ex, clip(&join_w(node, "delText")));
            } else if is_w(node, "r")
                && let Some(text) = hidden_run_text(node)
            {
                n_hidden += 1;
                push_example(&mut hidden_ex, clip(&text));
            }
        }
    }

    let mut out = Vec::new();
    push_finding(&mut out, Kind::TrackedInsertions, n_ins, ins_ex);
    push_finding(&mut out, Kind::TrackedDeletions, n_del, del_ex);
    if let Some(finding) = comments_finding(docx)? {
        out.push(finding);
    }
    let fields = list_fields(docx)?;
    if !fields.is_empty() {
        out.push(Finding {
            kind: Kind::UnfilledFields,
            count: fields.len(),
            examples: fields
                .iter()
                .take(EXAMPLES)
                .map(|f| format!("{{{{{}}}}}", f.name))
                .collect(),
        });
    }
    let (author, last) = core_props(docx)?;
    if let Some(finding) = author {
        out.push(finding);
    }
    if let Some(finding) = last {
        out.push(finding);
    }
    push_finding(&mut out, Kind::HiddenText, n_hidden, hidden_ex);
    Ok(out)
}

/// The findings of a PDF, in the order of [`Kind`]: `Author`, `PdfForm`,
/// `PdfAnnotations`. A PDF lopdf cannot load (or an encrypted one) is
/// `PatchError::NotDocx` with the reason (the name is the existing error's).
pub fn pdf_findings(pdf: &[u8]) -> Result<Vec<Finding>, PatchError> {
    let doc = lopdf::Document::load_mem(pdf).map_err(not_docx)?;
    if doc.is_encrypted() {
        return Err(PatchError::NotDocx("encrypted PDF".into()));
    }
    let mut out = Vec::new();
    if let Some(author) = pdf_author(&doc) {
        out.push(Finding {
            kind: Kind::Author,
            count: 1,
            examples: vec![clip(&author)],
        });
    }
    if pdf_has_form(&doc) {
        out.push(Finding {
            kind: Kind::PdfForm,
            count: 1,
            examples: Vec::new(),
        });
    }
    let (count, examples) = pdf_annotations(&doc);
    push_finding(&mut out, Kind::PdfAnnotations, count, examples);
    Ok(out)
}

/// `s` cut to [`CLIP`] characters, `…` added when cut; whitespace runs
/// (newlines included) become one space and the ends are trimmed.
pub fn clip(s: &str) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.char_indices().nth(CLIP) {
        Some((end, _)) => format!("{}…", &flat[..end]),
        None => flat,
    }
}

fn parse_xml(xml: &str) -> Result<roxmltree::Document<'_>, PatchError> {
    roxmltree::Document::parse(xml).map_err(not_docx)
}

fn not_docx(err: impl std::fmt::Display) -> PatchError {
    PatchError::NotDocx(err.to_string())
}

fn is_w(node: roxmltree::Node<'_, '_>, name: &str) -> bool {
    node.has_tag_name((WML, name))
}

fn parent_is_rpr(node: roxmltree::Node<'_, '_>) -> bool {
    node.parent().is_some_and(|p| is_w(p, "rPr"))
}

fn w_attr<'a>(node: roxmltree::Node<'a, '_>, name: &str) -> Option<&'a str> {
    node.attribute((WML, name)).or_else(|| node.attribute(name))
}

fn inner_text(node: roxmltree::Node<'_, '_>) -> String {
    node.children().filter_map(|c| c.text()).collect()
}

fn join_w(node: roxmltree::Node<'_, '_>, local: &str) -> String {
    node.descendants()
        .filter(|n| is_w(*n, local))
        .map(inner_text)
        .collect()
}

fn push_example(examples: &mut Vec<String>, example: String) {
    if examples.len() < EXAMPLES {
        examples.push(example);
    }
}

fn push_finding(out: &mut Vec<Finding>, kind: Kind, count: usize, examples: Vec<String>) {
    if count >= 1 {
        out.push(Finding {
            kind,
            count,
            examples,
        });
    }
}

fn hidden_run_text(run: roxmltree::Node<'_, '_>) -> Option<String> {
    let rpr = run.children().find(|c| is_w(*c, "rPr"))?;
    let vanished = rpr
        .children()
        .any(|c| is_w(c, "vanish") && !matches!(w_attr(c, "val"), Some("0" | "false" | "off")));
    if !vanished {
        return None;
    }
    let text: String = run
        .children()
        .filter(|c| is_w(*c, "t"))
        .map(inner_text)
        .collect();
    if text.is_empty() { None } else { Some(text) }
}

fn comments_finding(docx: &[u8]) -> Result<Option<Finding>, PatchError> {
    let Some(xml) = zip_xml(docx, "word/comments.xml")? else {
        return Ok(None);
    };
    let tree = parse_xml(&xml)?;
    let mut examples = Vec::new();
    let mut count = 0usize;
    for comment in tree.descendants().filter(|n| is_w(*n, "comment")) {
        count += 1;
        push_example(&mut examples, comment_example(comment));
    }
    Ok(if count >= 1 {
        Some(Finding {
            kind: Kind::Comments,
            count,
            examples,
        })
    } else {
        None
    })
}

fn comment_example(comment: roxmltree::Node<'_, '_>) -> String {
    let paras: Vec<String> = comment
        .descendants()
        .filter(|n| is_w(*n, "p"))
        .map(|p| join_w(p, "t"))
        .collect();
    let text = paras.join(" ");
    let raw = match w_attr(comment, "author") {
        Some(author) if !author.is_empty() => format!("{author}：{text}"),
        _ => text,
    };
    clip(&raw)
}

fn core_props(docx: &[u8]) -> Result<(Option<Finding>, Option<Finding>), PatchError> {
    let Some(xml) = zip_xml(docx, "docProps/core.xml")? else {
        return Ok((None, None));
    };
    let tree = parse_xml(&xml)?;
    let mut creator = None;
    let mut last = None;
    for node in tree.descendants() {
        if node.has_tag_name("creator") && creator.is_none() {
            creator = property_finding(Kind::Author, &inner_text(node));
        } else if node.has_tag_name("lastModifiedBy") && last.is_none() {
            last = property_finding(Kind::LastModifiedBy, &inner_text(node));
        }
    }
    Ok((creator, last))
}

fn property_finding(kind: Kind, value: &str) -> Option<Finding> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(Finding {
            kind,
            count: 1,
            examples: vec![clip(value)],
        })
    }
}

fn zip_xml(docx: &[u8], name: &str) -> Result<Option<String>, PatchError> {
    let mut archive = ZipArchive::new(Cursor::new(docx)).map_err(not_docx)?;
    let file = match archive.by_name(name) {
        Ok(file) => file,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(e) => return Err(not_docx(e)),
    };
    let mut xml = String::new();
    file.take(MAX_DOCUMENT_XML_BYTES + 1)
        .read_to_string(&mut xml)
        .map_err(not_docx)?;
    if xml.len() as u64 > MAX_DOCUMENT_XML_BYTES {
        return Err(PatchError::TooLarge);
    }
    Ok(Some(xml))
}

fn deref_obj<'a>(doc: &'a lopdf::Document, obj: &'a lopdf::Object) -> &'a lopdf::Object {
    match doc.dereference(obj) {
        Ok((_, resolved)) => resolved,
        Err(_) => obj,
    }
}

fn pdf_author(doc: &lopdf::Document) -> Option<String> {
    let info = doc.trailer.get(b"Info").ok()?;
    let lopdf::Object::Dictionary(dict) = deref_obj(doc, info) else {
        return None;
    };
    let author = dict.get(b"Author").ok()?;
    let text = lopdf::decode_text_string(deref_obj(doc, author)).ok()?;
    let text = text.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_owned())
    }
}

fn pdf_has_form(doc: &lopdf::Document) -> bool {
    let Ok(catalog) = doc.catalog() else {
        return false;
    };
    let Ok(acro) = catalog.get(b"AcroForm") else {
        return false;
    };
    let lopdf::Object::Dictionary(dict) = deref_obj(doc, acro) else {
        return false;
    };
    let Ok(fields) = dict.get(b"Fields") else {
        return false;
    };
    matches!(deref_obj(doc, fields), lopdf::Object::Array(arr) if !arr.is_empty())
}

fn pdf_annotations(doc: &lopdf::Document) -> (usize, Vec<String>) {
    let mut count = 0usize;
    let mut examples = Vec::new();
    for &page_id in doc.get_pages().values() {
        let annots = doc.get_page_annotations(page_id).unwrap_or_default();
        for annot in annots {
            let Ok(name) = annot.get(b"Subtype").and_then(lopdf::Object::as_name) else {
                continue;
            };
            // A Popup is the window of another annotation, not one of its own.
            if name == b"Link" || name == b"Widget" || name == b"Popup" {
                continue;
            }
            count += 1;
            let shown = String::from_utf8_lossy(name).into_owned();
            if !examples.contains(&shown) {
                push_example(&mut examples, shown);
            }
        }
    }
    (count, examples)
}

fn failure(e: PatchError) -> ApiError {
    match e {
        PatchError::TooLarge => {
            ApiError::unprocessable("payload_too_large", "the document is too large to check")
        }
        other => ApiError::unprocessable("parse_failed", other.to_string()),
    }
}

/// The service behind the route and the tool: the head revision checked.
pub async fn preflight_of(storage: &Arc<Storage>, id: String) -> Result<Preflight, ApiError> {
    let (revision, title, media_type, bytes) = crate::patch::head_bytes(
        storage,
        &id,
        &[DOCX, PDF],
        "only Word (DOCX) and PDF documents can be checked before sending",
    )
    .await?;
    let (format, findings) = match media_type.as_str() {
        DOCX => ("docx", blocking(move || docx_findings(&bytes)).await),
        PDF => ("pdf", blocking(move || pdf_findings(&bytes)).await),
        _ => {
            return Err(ApiError::unprocessable(
                "unsupported_format",
                "only Word (DOCX) and PDF documents can be checked before sending",
            ));
        }
    };
    let findings = findings.map_err(failure)?;
    Ok(Preflight {
        document_id: id,
        revision,
        title,
        format,
        clean: findings.is_empty(),
        findings,
        cleanable: format == "docx",
    })
}

/// The tool's answer: the same facts, and how to tell them.
pub fn for_the_model(p: Preflight) -> serde_json::Value {
    // PR-Daemon #2086 B1/B2: say only what was checked, and promise no
    // cleaning the module cannot do.
    let scope = if p.cleanable {
        "只检查了 Word 正文、页眉和页脚里的修订（插入/删除）、批注、未填的 {{字段}}、作者和最后修改者、隐藏文字；脚注、尾注、移动修订、格式修订、样式里设的隐藏没有检查。"
    } else {
        "这是 PDF，只检查了作者、有没有表单、有没有批注；修订、未填字段、隐藏文字和其他属性都没有检查。"
    };
    let what = if p.clean {
        "检查过的这几类里没有发现问题。照实告诉用户检查了哪些、没检查哪些，不要说「放心发出」或「完全干净」。"
    } else {
        "逐类告诉用户 findings 里列出的内容（kind 是类别，count 是数量，examples 是例子），不要补充没列出的东西。"
    };
    let fix = if p.cleanable {
        "这是只读检查，文档没有被改动。要清理时，请用户在对话里说「去掉修订和批注，存一份干净的副本」，App 会给出按钮，让用户选好位置后另存一份接受修订、删除批注、清空作者的新副本（原文档不动）；你自己不能做这一步。"
    } else {
        "这是只读检查，文档没有被改动。PDF 不能清理；要清理请改用原来的 Word 文档。"
    };
    let note = format!("{scope}{what}{fix}");
    let mut v = serde_json::to_value(p).unwrap_or_default();
    v["note"] = serde_json::Value::from(note);
    v
}

pub async fn preflight(
    State(state): State<AppState>,
    path: Result<UrlPath<String>, PathRejection>,
) -> Result<Json<Preflight>, ApiError> {
    let UrlPath(id) = path.map_err(|e| ApiError::invalid_request(e.body_text()))?;
    let storage = state
        .storage()
        .await
        .map_err(ApiError::storage_unavailable)?;
    preflight_of(&storage, id).await.map(Json)
}

#[cfg(test)]
mod tests;
