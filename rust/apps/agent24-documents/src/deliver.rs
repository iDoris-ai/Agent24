//! L8 DOC-4 (first slice): a clean copy to send, and a receipt of where it
//! was saved. Nothing is sent out: the desktop saves the copy where the user
//! picked in the system save dialog, then records the receipt here.
//!
//! `POST /documents/{id}/deliveries` (`expect_revision`: the version the user
//! checked): the head revision of a DOCX with its tracked changes accepted,
//! its comments removed and its author / last modifier emptied, imported as a
//! NEW document 「<title>（交付）」. The original is not changed. The answer is
//! the new document's import job (202), as a patch answers. A PDF copy is the
//! desktop's next step: the existing conversion of that new document.
//!
//! `POST /documents/{id}/receipts`: once the file is saved, one append-only
//! `oplog` row `op = 'deliver'` for the source document and revision, with
//! the copy's document id, the SHA-256 and format of the copy's file (taken
//! from the library, not from the caller), where it was saved and its name.
//! `GET /documents/{id}/receipts` and `GET /receipts`: the latest receipts,
//! newest first; `again` marks one of a source version already saved in
//! that format before (each clean copy has its own modified time, so its
//! SHA-256 differs; the source version is what tells a second save).

use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::docx_patch::{self, PatchError};
use crate::error::ApiError;
use crate::id::{IdKind, is_id};
use crate::jobs::Job;
use crate::office::DOCX;
use crate::oplog::{self, Origin};
use crate::state::{AppState, Storage, blocking};

const PDF: &str = "application/pdf";
/// Receipts in one answer, at most.
pub const RECEIPTS_SHOWN: i64 = 20;
/// A saved path, at most this many characters.
const PATH_MAX: usize = 1024;

/// The DOCX with its tracked changes accepted, its comments removed and its
/// author / last modifier emptied; every other part copied as stored.
pub fn clean_docx(docx: &[u8], now_iso: &str) -> Result<Vec<u8>, PatchError> {
    let mut parts = std::collections::BTreeMap::new();
    for (name, xml) in docx_patch::text_parts(docx)? {
        let cleaned = clean_part(&xml)?;
        if cleaned != xml {
            parts.insert(name, cleaned);
        }
    }
    if let Some(comments) = read_part(docx, "word/comments.xml")? {
        parts.insert("word/comments.xml".to_owned(), empty_comments(&comments)?);
    }
    if let Some(core) = read_part(docx, "docProps/core.xml")? {
        parts.insert("docProps/core.xml".to_owned(), clean_core(&core, now_iso)?);
    }
    docx_patch::splice_parts(docx, &parts, now_iso)
}

/// One text part (body, header, footer) with:
/// - `w:del` and `w:moveFrom` removed with everything in them (accepted:
///   the deleted text is gone), unless the parent is `w:rPr`;
/// - `w:ins` and `w:moveTo` unwrapped (their children stay where they were),
///   unless the parent is `w:rPr`;
/// - `w:ins` / `w:del` that are children of `w:rPr` (paragraph-mark or run
///   marks) removed;
/// - `w:rPrChange`, `w:pPrChange`, `w:sectPrChange`, `w:tblPrChange`,
///   `w:trPrChange`, `w:tcPrChange`, `w:tblGridChange`, `w:numberingChange`
///   removed (the current formatting stays);
/// - `w:moveFromRangeStart/End`, `w:moveToRangeStart/End`,
///   `w:commentRangeStart/End` removed;
/// - `w:commentReference` removed, and its `w:r` too when nothing but a
///   `w:rPr` is left in it.
///
/// Everything else is kept byte for byte. The result parses as XML.
pub fn clean_part(xml: &str) -> Result<String, PatchError> {
    let spans = {
        let doc = parse_part(xml)?;
        let mut spans = Vec::new();
        collect_part(doc.root_element(), &mut spans);
        spans
    };
    let out = apply_spans(xml, spans);
    require_xml(&out)?;
    Ok(out)
}

/// `word/comments.xml` as its root element with no `w:comment` left (the
/// XML declaration and the root's attributes kept), so its relationship and
/// content type stay valid.
pub fn empty_comments(xml: &str) -> Result<String, PatchError> {
    let doc = parse_part(xml)?;
    let root = doc.root_element();
    let out = if is_self_closing(xml, root) || root.first_child().is_none() {
        xml.to_owned()
    } else {
        emptied_root(xml, root)?
    };
    require_xml(&out)?;
    Ok(out)
}

/// `docProps/core.xml` with the text of `dc:creator` and `cp:lastModifiedBy`
/// emptied (the elements kept, a self-closing one left as it is) and
/// `dcterms:modified` set to `now_iso` when there is one.
pub fn clean_core(xml: &str, now_iso: &str) -> Result<String, PatchError> {
    let stamped = escape_xml_text(now_iso);
    let spans = {
        let doc = parse_part(xml)?;
        let mut spans = Vec::new();
        collect_core(doc.root_element(), xml, &stamped, &mut spans);
        spans
    };
    let out = apply_spans(xml, spans);
    require_xml(&out)?;
    Ok(out)
}

const WML: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const DC: &str = "http://purl.org/dc/elements/1.1/";
const CP: &str = "http://schemas.openxmlformats.org/package/2006/metadata/core-properties";
const DCTERMS: &str = "http://purl.org/dc/terms/";

/// One slice of the original document replaced in place. An empty `with`
/// deletes the slice.
struct Span {
    start: usize,
    end: usize,
    with: String,
}

impl Span {
    fn delete(range: std::ops::Range<usize>) -> Self {
        Self {
            start: range.start,
            end: range.end,
            with: String::new(),
        }
    }

    fn replace(range: std::ops::Range<usize>, with: String) -> Self {
        Self {
            start: range.start,
            end: range.end,
            with,
        }
    }
}

enum PartAction {
    Delete,
    Unwrap,
    Keep,
}

enum CoreKind {
    None,
    Clear,
    Stamp,
}

fn parse_part(xml: &str) -> Result<roxmltree::Document<'_>, PatchError> {
    roxmltree::Document::parse(xml).map_err(|err| PatchError::NotDocx(err.to_string()))
}

fn require_xml(xml: &str) -> Result<(), PatchError> {
    roxmltree::Document::parse(xml).map_err(|err| PatchError::Invalid(err.to_string()))?;
    Ok(())
}

fn is_w(node: roxmltree::Node<'_, '_>, local: &str) -> bool {
    node.has_tag_name((WML, local))
}

fn is_self_closing(xml: &str, node: roxmltree::Node<'_, '_>) -> bool {
    if node.has_children() {
        return false;
    }
    let range = node.range();
    xml[range.start..range.end].ends_with("/>")
}

fn part_action(node: roxmltree::Node<'_, '_>) -> PartAction {
    if node.tag_name().namespace() != Some(WML) {
        return PartAction::Keep;
    }
    let in_rpr = node.parent().is_some_and(|parent| is_w(parent, "rPr"));
    match node.tag_name().name() {
        // Inserted or moved-in content stays; the wrapper does not.
        "ins" | "moveTo" if !in_rpr => PartAction::Unwrap,
        // Deleted or moved-out content goes, except a move mark kept on `w:rPr`.
        "moveFrom" if !in_rpr => PartAction::Delete,
        "del" | "ins" | "rPrChange" | "pPrChange" | "sectPrChange" | "tblPrChange"
        | "trPrChange" | "tcPrChange" | "tblGridChange" | "numberingChange"
        | "moveFromRangeStart" | "moveFromRangeEnd" | "moveToRangeStart" | "moveToRangeEnd"
        | "commentRangeStart" | "commentRangeEnd" | "commentReference" => PartAction::Delete,
        _ => PartAction::Keep,
    }
}

/// The run carries a comment mark and nothing else that would be visible.
fn run_is_only_comment_mark(node: roxmltree::Node<'_, '_>) -> bool {
    let mut saw_ref = false;
    for child in node.children() {
        if !child.is_element() {
            continue;
        }
        if is_w(child, "commentReference") {
            saw_ref = true;
        } else if !is_w(child, "rPr") {
            return false;
        }
    }
    saw_ref
}

fn collect_part(node: roxmltree::Node<'_, '_>, spans: &mut Vec<Span>) {
    if !node.is_element() {
        return;
    }
    if is_w(node, "r") && run_is_only_comment_mark(node) {
        spans.push(Span::delete(node.range()));
        return;
    }
    match part_action(node) {
        PartAction::Delete => spans.push(Span::delete(node.range())),
        PartAction::Unwrap => {
            if unwrap_element(node, spans) {
                for child in node.children() {
                    collect_part(child, spans);
                }
            }
        }
        PartAction::Keep => {
            for child in node.children() {
                collect_part(child, spans);
            }
        }
    }
}

/// Record the start and end tags for removal. A childless element is removed
/// whole (it is self-closing, or empty). Returns false when the whole element
/// was removed, so nothing inside it is edited again.
fn unwrap_element(node: roxmltree::Node<'_, '_>, spans: &mut Vec<Span>) -> bool {
    let range = node.range();
    let (Some(first), Some(last)) = (node.first_child(), node.last_child()) else {
        spans.push(Span::delete(range));
        return false;
    };
    let open_end = first.range().start;
    let close_start = last.range().end;
    if range.start < open_end && open_end <= close_start && close_start <= range.end {
        spans.push(Span::delete(range.start..open_end));
        if close_start < range.end {
            spans.push(Span::delete(close_start..range.end));
        }
        true
    } else {
        spans.push(Span::delete(range));
        false
    }
}

fn emptied_root<'a>(xml: &'a str, root: roxmltree::Node<'_, 'a>) -> Result<String, PatchError> {
    let Some(first) = root.first_child() else {
        return Ok(xml.to_owned());
    };
    let qname = qualified_name(xml, root.range().start)?;
    let mut out = String::with_capacity(first.range().start + qname.len() + 3);
    out.push_str(&xml[..first.range().start]);
    out.push_str("</");
    out.push_str(qname);
    out.push('>');
    Ok(out)
}

fn qualified_name(xml: &str, elem_start: usize) -> Result<&str, PatchError> {
    let bytes = xml.as_bytes();
    if elem_start >= bytes.len() || bytes[elem_start] != b'<' {
        return Err(PatchError::Invalid(
            "the root element is missing its start tag".into(),
        ));
    }
    let mut end = elem_start + 1;
    while end < bytes.len() {
        let byte = bytes[end];
        if byte.is_ascii_whitespace() || byte == b'>' || byte == b'/' {
            break;
        }
        end += 1;
    }
    if end == elem_start + 1 {
        return Err(PatchError::Invalid("the root element has no name".into()));
    }
    Ok(&xml[elem_start + 1..end])
}

fn core_kind(node: roxmltree::Node<'_, '_>) -> CoreKind {
    match (node.tag_name().namespace(), node.tag_name().name()) {
        (Some(DC), "creator") | (Some(CP), "lastModifiedBy") => CoreKind::Clear,
        (Some(DCTERMS), "modified") => CoreKind::Stamp,
        _ => CoreKind::None,
    }
}

fn collect_core(node: roxmltree::Node<'_, '_>, xml: &str, now_iso: &str, spans: &mut Vec<Span>) {
    for el in node.descendants().filter(|el| el.is_element()) {
        match core_kind(el) {
            CoreKind::None => {}
            _ if is_self_closing(xml, el) => {}
            CoreKind::Clear => clear_text(el, spans),
            CoreKind::Stamp => stamp_text(xml, el, now_iso, spans),
        }
    }
}

fn clear_text(el: roxmltree::Node<'_, '_>, spans: &mut Vec<Span>) {
    for child in el.children().filter(|child| child.is_text()) {
        let range = child.range();
        if range.start < range.end {
            spans.push(Span::delete(range));
        }
    }
}

fn stamp_text(xml: &str, el: roxmltree::Node<'_, '_>, now_iso: &str, spans: &mut Vec<Span>) {
    let mut stamped = false;
    for child in el.children().filter(|child| child.is_text()) {
        let range = child.range();
        if stamped {
            if range.start < range.end {
                spans.push(Span::delete(range));
            }
        } else {
            spans.push(Span::replace(range, now_iso.to_owned()));
            stamped = true;
        }
    }
    if stamped {
        return;
    }
    if let Some(at) = content_start(xml, el) {
        spans.push(Span::replace(at..at, now_iso.to_owned()));
    }
}

/// The byte just after an element's start tag.
fn content_start(xml: &str, node: roxmltree::Node<'_, '_>) -> Option<usize> {
    if let Some(child) = node.first_child() {
        return Some(child.range().start);
    }
    let range = node.range();
    let bytes = xml.as_bytes();
    let mut quote = None;
    for (offset, byte) in bytes[range.start..range.end].iter().enumerate() {
        if let Some(open) = quote {
            if *byte == open {
                quote = None;
            }
            continue;
        }
        match *byte {
            b'"' | b'\'' => quote = Some(*byte),
            b'>' => return Some(range.start + offset + 1),
            _ => {}
        }
    }
    None
}

fn escape_xml_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch != '\t'
            && ch != '\n'
            && ch != '\r'
            && (ch < ' ' || ch == '\u{FFFE}' || ch == '\u{FFFF}')
        {
            continue;
        }
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Apply non-overlapping spans from left to right. A span contained in an
/// earlier one is dropped, so an edit inside a removed element never lands.
fn apply_spans(xml: &str, mut spans: Vec<Span>) -> String {
    if spans.is_empty() {
        return xml.to_owned();
    }
    spans.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
    let mut kept = Vec::with_capacity(spans.len());
    for span in spans {
        if span.start > span.end || span.end > xml.len() {
            continue;
        }
        if kept.last().is_some_and(|prev: &Span| span.start < prev.end) {
            continue;
        }
        kept.push(span);
    }
    let mut out = String::with_capacity(xml.len());
    let mut cursor = 0usize;
    for span in &kept {
        if span.start < cursor {
            continue;
        }
        out.push_str(&xml[cursor..span.start]);
        out.push_str(&span.with);
        cursor = span.end;
    }
    out.push_str(&xml[cursor..]);
    out
}

/// A zip entry as text, bounded as the text parts are; None when absent.
fn read_part(docx: &[u8], name: &str) -> Result<Option<String>, PatchError> {
    use std::io::Read;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(docx))
        .map_err(|e| PatchError::NotDocx(e.to_string()))?;
    let file = match archive.by_name(name) {
        Ok(file) => file,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(e) => return Err(PatchError::NotDocx(e.to_string())),
    };
    let limit = docx_patch::MAX_DOCUMENT_XML_BYTES;
    let mut xml = String::new();
    file.take(limit + 1)
        .read_to_string(&mut xml)
        .map_err(|e| PatchError::NotDocx(e.to_string()))?;
    if xml.len() as u64 > limit {
        return Err(PatchError::TooLarge);
    }
    Ok(Some(xml))
}

async fn storage_of(state: &AppState) -> Result<Arc<Storage>, ApiError> {
    state.storage().await.map_err(ApiError::storage_unavailable)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliverRequest {
    /// The version the user checked; a newer head is never cleaned instead.
    pub expect_revision: i64,
}

pub async fn deliver(
    State(state): State<AppState>,
    path: Result<UrlPath<String>, PathRejection>,
    body: Result<Json<DeliverRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<Job>), ApiError> {
    let UrlPath(id) = path.map_err(|e| ApiError::invalid_request(e.body_text()))?;
    let Json(req) = body.map_err(|e| ApiError::invalid_request(e.body_text()))?;
    let storage = storage_of(&state).await?;
    let (head, title, _, bytes) = crate::patch::head_bytes(
        &storage,
        &id,
        &[DOCX],
        "only Word (DOCX) documents can be cleaned; a PDF is only checked",
    )
    .await?;
    if req.expect_revision != head {
        return Err(ApiError::conflict(
            "revision_conflict",
            format!(
                "the document is at version {head}, not {}; check it again",
                req.expect_revision
            ),
        ));
    }
    let now = crate::patch::now_utc(&storage).await?;
    let cleaned = blocking(move || clean_docx(&bytes, &now))
        .await
        .map_err(crate::patch::patch_failure)?;
    let title = crate::patch::derived_title(title, "（交付）");
    crate::patch::import_docx(&storage, title, cleaned).await
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptRequest {
    /// The source document's version the copy was made from.
    pub revision: i64,
    /// The copy saved (the DOCX of `deliveries`, or its PDF conversion).
    pub copy_document_id: String,
    /// The absolute path the file was saved to.
    pub saved_to: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Receipt {
    pub document_id: String,
    pub title: Option<String>,
    pub revision: i64,
    pub at: String,
    pub format: String,
    pub file_name: String,
    pub saved_to: String,
    pub sha256: String,
    pub copy_document_id: String,
    /// This source version was saved in this format before.
    pub again: bool,
}

#[derive(Debug, Serialize)]
pub struct Receipts {
    pub receipts: Vec<Receipt>,
}

fn saved_path(path: &str) -> Result<String, ApiError> {
    let ok = path.starts_with('/')
        && (2..=PATH_MAX).contains(&path.chars().count())
        && !path.chars().any(char::is_control)
        && !path.ends_with('/');
    if !ok {
        return Err(ApiError::invalid_request(
            "saved_to is the absolute path of the saved file",
        ));
    }
    Ok(path.to_owned())
}

pub async fn record_receipt(
    State(state): State<AppState>,
    path: Result<UrlPath<String>, PathRejection>,
    body: Result<Json<ReceiptRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<Receipt>), ApiError> {
    let UrlPath(id) = path.map_err(|e| ApiError::invalid_request(e.body_text()))?;
    let Json(req) = body.map_err(|e| ApiError::invalid_request(e.body_text()))?;
    let saved_to = saved_path(&req.saved_to)?;
    if !is_id(IdKind::Document, &id) || !is_id(IdKind::Document, &req.copy_document_id) {
        return Err(ApiError::invalid_request("not a document id"));
    }
    let storage = storage_of(&state).await?;
    // The source version exists; the copy's file is the library's own.
    crate::read::revision(&storage, &id, req.revision).await?;
    let copy_head: Option<i64> =
        match sqlx::query_scalar("SELECT head_revision FROM documents WHERE id = ?")
            .bind(&req.copy_document_id)
            .fetch_optional(storage.db.pool())
            .await
        {
            Ok(head) => head,
            Err(e) => return Err(storage.db_failure(e).await),
        };
    let copy_head = copy_head.ok_or_else(|| ApiError::not_found("no such copy"))?;
    let copy = crate::read::revision(&storage, &req.copy_document_id, copy_head).await?;
    let format = match copy.media_type.as_str() {
        DOCX => "docx",
        PDF => "pdf",
        _ => {
            return Err(ApiError::unprocessable(
                "unsupported_format",
                "a delivered copy is a DOCX or a PDF",
            ));
        }
    };
    let file_name = saved_to
        .rsplit('/')
        .next()
        .unwrap_or(saved_to.as_str())
        .to_owned();
    let details = json!({
        "copy_document_id": req.copy_document_id,
        "sha256": copy.content_sha256,
        "format": format,
        "saved_to": saved_to,
        "file_name": file_name,
    });
    let mut conn = match storage.db.pool().acquire().await {
        Ok(conn) => conn,
        Err(e) => return Err(storage.db_failure(e).await),
    };
    if let Err(e) = oplog::append(
        &mut conn,
        "deliver",
        &id,
        req.revision,
        &Origin::Page,
        &details,
    )
    .await
    {
        return Err(storage.db_failure(e).await);
    }
    drop(conn);
    let latest = receipts_of(&storage, Some(&id)).await?;
    let receipt = latest
        .into_iter()
        .next()
        .ok_or_else(|| ApiError::internal("the receipt was not written"))?;
    Ok((StatusCode::CREATED, Json(receipt)))
}

/// One `deliver` oplog row: document, title, revision, time, details, again.
type ReceiptRow = (String, Option<String>, i64, String, String, i64);

/// The latest receipts, newest first: of one document, or of all.
pub async fn receipts_of(storage: &Storage, id: Option<&str>) -> Result<Vec<Receipt>, ApiError> {
    if id.is_some_and(|id| !is_id(IdKind::Document, id)) {
        return Err(ApiError::invalid_request("not a document id"));
    }
    let rows: Result<Vec<ReceiptRow>, _> = sqlx::query_as(
        "SELECT o.document_id, d.title, o.revision, o.at, o.details,
                EXISTS (SELECT 1 FROM oplog p
                         WHERE p.op = 'deliver' AND p.document_id = o.document_id AND p.seq < o.seq
                           AND p.revision = o.revision
                           AND json_extract(p.details, '$.format') = json_extract(o.details, '$.format'))
           FROM oplog o LEFT JOIN documents d ON d.id = o.document_id
          WHERE o.op = 'deliver' AND (?1 IS NULL OR o.document_id = ?1)
          ORDER BY o.seq DESC LIMIT ?2",
    )
    .bind(id)
    .bind(RECEIPTS_SHOWN)
    .fetch_all(storage.db.pool())
    .await;
    let rows = match rows {
        Ok(rows) => rows,
        Err(e) => return Err(storage.db_failure(e).await),
    };
    Ok(rows
        .into_iter()
        .map(|(document_id, title, revision, at, details, again)| {
            let d: Value = serde_json::from_str(&details).unwrap_or_default();
            let s = |k: &str| d[k].as_str().unwrap_or_default().to_owned();
            Receipt {
                document_id,
                title,
                revision,
                at,
                format: s("format"),
                file_name: s("file_name"),
                saved_to: s("saved_to"),
                sha256: s("sha256"),
                copy_document_id: s("copy_document_id"),
                again: again != 0,
            }
        })
        .collect())
}

pub async fn document_receipts(
    State(state): State<AppState>,
    path: Result<UrlPath<String>, PathRejection>,
) -> Result<Json<Receipts>, ApiError> {
    let UrlPath(id) = path.map_err(|e| ApiError::invalid_request(e.body_text()))?;
    let storage = storage_of(&state).await?;
    let receipts = receipts_of(&storage, Some(&id)).await?;
    Ok(Json(Receipts { receipts }))
}

pub async fn all_receipts(State(state): State<AppState>) -> Result<Json<Receipts>, ApiError> {
    let storage = storage_of(&state).await?;
    let receipts = receipts_of(&storage, None).await?;
    Ok(Json(Receipts { receipts }))
}

#[cfg(test)]
mod tests;
