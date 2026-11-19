//! E1b: edit some paragraphs of a DOCX and leave the rest of the file as it
//! was. Only `word/document.xml` is written again, and in it only the
//! paragraphs edited (each one sliced out and replaced in place); every other
//! zip entry is copied as stored (same compressed bytes, same CRC), and
//! `docProps/core.xml` gets its modified time. An edit can be a tracked change
//! (`w:del` of the old text, `w:ins` of the new), which Word and WPS can then
//! accept or reject (E1a, knowledge-materials/plan/e1a-docx-patch-spike.md).
//!
//! The approach follows GenOffice's docx-engine (paragraph patch, zip
//! splice; Apache-2.0, Copyright 2026 Mainfunc, Inc.); this is an independent
//! implementation, none of its code is copied.

use std::io::{Cursor, Read, Write};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

/// A DOCX larger than this is refused.
pub const MAX_DOCX_BYTES: usize = 50 * 1024 * 1024;
/// `word/document.xml`, uncompressed, at most.
pub const MAX_DOCUMENT_XML_BYTES: u64 = 20 * 1024 * 1024;
/// Edits in one patch, at most.
pub const MAX_EDITS: usize = 200;

const DOCUMENT: &str = "word/document.xml";
const CORE: &str = "docProps/core.xml";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParaInfo {
    pub index: usize,
    pub text: String,
    pub in_table: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParaEdit {
    pub index: usize,
    pub text: String,
    pub tracked: bool,
    /// The paragraph's current text; another text is [`PatchError::Stale`].
    pub expect: Option<String>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PatchError {
    #[error("not a DOCX: {0}")]
    NotDocx(String),
    #[error("the document is too large")]
    TooLarge,
    #[error("there is no paragraph {0}")]
    IndexOutOfRange(usize),
    #[error("paragraph {0} is no longer the text expected")]
    Stale(usize),
    #[error("{0}")]
    Invalid(String),
}

/// The body's paragraphs, in document order.
pub fn list_paragraphs(docx: &[u8]) -> Result<Vec<ParaInfo>, PatchError> {
    let (_, xml) = read_document(docx)?;
    let paras = scan_paragraphs(&xml)?;
    Ok(paras
        .iter()
        .enumerate()
        .map(|(index, p)| ParaInfo {
            index,
            text: paragraph_text(&xml[p.start..p.end]),
            in_table: p.in_table,
        })
        .collect())
}

/// The DOCX with `edits` applied; see the module notes for what changes.
pub fn apply(
    docx: &[u8],
    edits: &[ParaEdit],
    author: &str,
    now_iso: &str,
) -> Result<Vec<u8>, PatchError> {
    if edits.is_empty() || edits.len() > MAX_EDITS {
        return Err(PatchError::Invalid(format!(
            "1 to {MAX_EDITS} edits are needed"
        )));
    }
    let (_, xml) = read_document(docx)?;
    let paras = scan_paragraphs(&xml)?;
    let mut sorted: Vec<&ParaEdit> = edits.iter().collect();
    sorted.sort_by_key(|e| e.index);
    for pair in sorted.windows(2) {
        if pair[0].index == pair[1].index {
            return Err(PatchError::Invalid(format!(
                "paragraph {} is edited twice",
                pair[0].index
            )));
        }
    }
    for edit in &sorted {
        let Some(p) = paras.get(edit.index) else {
            return Err(PatchError::IndexOutOfRange(edit.index));
        };
        if let Some(expect) = &edit.expect
            && paragraph_text(&xml[p.start..p.end]) != *expect
        {
            return Err(PatchError::Stale(edit.index));
        }
    }
    let mut next_id = max_revision_id(&xml) + 1;
    let mut out = String::with_capacity(xml.len() + 1024);
    let mut cursor = 0;
    for edit in &sorted {
        let p = &paras[edit.index];
        out.push_str(&xml[cursor..p.start]);
        let rewritten = rewrite_paragraph(&xml[p.start..p.end], edit, author, now_iso, next_id);
        if edit.tracked {
            next_id += 2;
        }
        out.push_str(&rewritten);
        cursor = p.end;
    }
    out.push_str(&xml[cursor..]);
    splice(docx, &out, now_iso)
}

/// The zip of a DOCX, read in memory.
type Archive<'a> = ZipArchive<Cursor<&'a [u8]>>;

fn read_document(docx: &[u8]) -> Result<(Archive<'_>, String), PatchError> {
    if docx.len() > MAX_DOCX_BYTES {
        return Err(PatchError::TooLarge);
    }
    let mut archive =
        ZipArchive::new(Cursor::new(docx)).map_err(|e| PatchError::NotDocx(e.to_string()))?;
    let xml = {
        let mut file = archive
            .by_name(DOCUMENT)
            .map_err(|_| PatchError::NotDocx("no word/document.xml".into()))?;
        if file.size() > MAX_DOCUMENT_XML_BYTES {
            return Err(PatchError::TooLarge);
        }
        let mut xml = String::new();
        file.by_ref()
            .take(MAX_DOCUMENT_XML_BYTES + 1)
            .read_to_string(&mut xml)
            .map_err(|e| PatchError::NotDocx(e.to_string()))?;
        if xml.len() as u64 > MAX_DOCUMENT_XML_BYTES {
            return Err(PatchError::TooLarge);
        }
        xml
    };
    Ok((archive, xml))
}

/// The DOCX again: `document_xml` as `word/document.xml`, `core.xml`'s
/// modified time set, every other entry copied as stored, in the same order.
fn splice(docx: &[u8], document_xml: &str, now_iso: &str) -> Result<Vec<u8>, PatchError> {
    let zip_err = |e: zip::result::ZipError| PatchError::NotDocx(e.to_string());
    let io_err = |e: std::io::Error| PatchError::NotDocx(e.to_string());
    let mut archive = ZipArchive::new(Cursor::new(docx)).map_err(zip_err)?;
    let mut writer = ZipWriter::new(Cursor::new(Vec::with_capacity(docx.len() + 4096)));
    for i in 0..archive.len() {
        let name = archive.by_index_raw(i).map_err(zip_err)?.name().to_owned();
        let rewrite = if name == DOCUMENT {
            Some(document_xml.to_owned())
        } else if name == CORE {
            let mut core = String::new();
            archive
                .by_index(i)
                .map_err(zip_err)?
                .read_to_string(&mut core)
                .map_err(io_err)?;
            set_modified(&core, now_iso)
        } else {
            None
        };
        match rewrite {
            Some(content) => {
                let options = {
                    let file = archive.by_index_raw(i).map_err(zip_err)?;
                    let mut o = SimpleFileOptions::default()
                        .compression_method(CompressionMethod::Deflated);
                    if let Some(time) = file.last_modified() {
                        o = o.last_modified_time(time);
                    }
                    o
                };
                writer.start_file(name, options).map_err(zip_err)?;
                writer.write_all(content.as_bytes()).map_err(io_err)?;
            }
            None => {
                let file = archive.by_index_raw(i).map_err(zip_err)?;
                writer.raw_copy_file(file).map_err(zip_err)?;
            }
        }
    }
    Ok(writer.finish().map_err(zip_err)?.into_inner())
}

/// `core.xml` with `dcterms:modified` set to `now_iso`, or None (copied as
/// stored) when it has none.
fn set_modified(core: &str, now_iso: &str) -> Option<String> {
    let open = core.find("<dcterms:modified")?;
    let value_start = open + core[open..].find('>')? + 1;
    let value_end = value_start + core[value_start..].find("</dcterms:modified>")?;
    Some(format!(
        "{}{}{}",
        &core[..value_start],
        escape_text(now_iso),
        &core[value_end..]
    ))
}

// ── A scanner that knows tags and their byte offsets, nothing more ──

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TagKind {
    Open,
    Close,
    Empty,
}

#[derive(Debug, Clone)]
struct Tag<'a> {
    name: &'a str,
    kind: TagKind,
    start: usize,
    end: usize,
}

/// Every element tag of `xml` in order; comments, processing instructions,
/// CDATA and the doctype are skipped.
fn tags(xml: &str) -> Result<Vec<Tag<'_>>, PatchError> {
    let bytes = xml.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(off) = xml[i..].find('<') {
        let start = i + off;
        let rest = &xml[start..];
        if rest.starts_with("<!--") {
            i = start + rest.find("-->").map(|e| e + 3).ok_or_else(broken)?;
            continue;
        }
        if rest.starts_with("<![CDATA[") {
            i = start + rest.find("]]>").map(|e| e + 3).ok_or_else(broken)?;
            continue;
        }
        if rest.starts_with("<?") || rest.starts_with("<!") {
            i = start + rest.find('>').map(|e| e + 1).ok_or_else(broken)?;
            continue;
        }
        // The tag ends at the first `>` outside a quoted attribute value.
        let mut j = start + 1;
        let mut quote: Option<u8> = None;
        while j < bytes.len() {
            match (quote, bytes[j]) {
                (Some(q), b) if b == q => quote = None,
                (None, b'"' | b'\'') => quote = Some(bytes[j]),
                (None, b'>') => break,
                _ => {}
            }
            j += 1;
        }
        if j >= bytes.len() {
            return Err(broken());
        }
        let end = j + 1;
        let inner = &xml[start + 1..j];
        let (kind, body) = if let Some(b) = inner.strip_prefix('/') {
            (TagKind::Close, b)
        } else if let Some(b) = inner.strip_suffix('/') {
            (TagKind::Empty, b)
        } else {
            (TagKind::Open, inner)
        };
        let name_end = body
            .find(|c: char| c.is_whitespace() || c == '/')
            .unwrap_or(body.len());
        out.push(Tag {
            name: &body[..name_end],
            kind,
            start,
            end,
        });
        i = end;
    }
    Ok(out)
}

fn broken() -> PatchError {
    PatchError::NotDocx("word/document.xml is not well formed".into())
}

#[derive(Debug, Clone, Copy)]
struct Para {
    start: usize,
    end: usize,
    in_table: bool,
}

/// The paragraphs under `w:body`, in order: those in tables too, never one
/// inside another paragraph (a text box's).
fn scan_paragraphs(xml: &str) -> Result<Vec<Para>, PatchError> {
    let mut paras = Vec::new();
    let mut stack: Vec<&str> = Vec::new();
    let mut in_body = false;
    let mut open_para: Option<(usize, usize, bool)> = None; // start, depth, in_table
    for tag in tags(xml)? {
        match tag.kind {
            TagKind::Open => {
                if tag.name == "w:body" {
                    in_body = true;
                }
                if in_body && tag.name == "w:p" && open_para.is_none() {
                    let in_table = stack.contains(&"w:tbl");
                    open_para = Some((tag.start, stack.len(), in_table));
                }
                stack.push(tag.name);
            }
            TagKind::Close => {
                if stack.pop() != Some(tag.name) {
                    return Err(broken());
                }
                if tag.name == "w:body" {
                    in_body = false;
                }
                if tag.name == "w:p"
                    && let Some((start, depth, in_table)) = open_para
                    && depth == stack.len()
                {
                    paras.push(Para {
                        start,
                        end: tag.end,
                        in_table,
                    });
                    open_para = None;
                }
            }
            TagKind::Empty => {
                if in_body && tag.name == "w:p" && open_para.is_none() {
                    paras.push(Para {
                        start: tag.start,
                        end: tag.end,
                        in_table: stack.contains(&"w:tbl"),
                    });
                }
            }
        }
    }
    if !stack.is_empty() {
        return Err(broken());
    }
    Ok(paras)
}

/// The text of a paragraph's own `w:t` (not inside a nested paragraph).
fn paragraph_text(para: &str) -> String {
    let Ok(all) = tags(para) else {
        return String::new();
    };
    let mut text = String::new();
    let mut depth_p = 0usize;
    let mut open_t: Option<usize> = None;
    for tag in all {
        match (tag.kind, tag.name) {
            (TagKind::Open, "w:p") => depth_p += 1,
            (TagKind::Close, "w:p") => depth_p = depth_p.saturating_sub(1),
            (TagKind::Open, "w:t") if depth_p <= 1 => open_t = Some(tag.end),
            (TagKind::Close, "w:t") => {
                if let Some(from) = open_t.take() {
                    text.push_str(&decode(&para[from..tag.start]));
                }
            }
            _ => {}
        }
    }
    text
}

/// The top-level children of a paragraph slice: (name, start, end).
fn children(para: &str) -> Vec<(&str, usize, usize)> {
    let Ok(all) = tags(para) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut open: Option<(&str, usize)> = None;
    for tag in all {
        match tag.kind {
            TagKind::Open => {
                depth += 1;
                if depth == 2 {
                    open = Some((tag.name, tag.start));
                }
            }
            TagKind::Close => {
                if depth == 2
                    && let Some((name, start)) = open.take()
                {
                    out.push((name, start, tag.end));
                }
                depth = depth.saturating_sub(1);
            }
            TagKind::Empty => {
                if depth == 1 {
                    out.push((tag.name, tag.start, tag.end));
                }
            }
        }
    }
    out
}

/// Zero-width markers kept where they were relative to the text.
fn is_marker(name: &str) -> bool {
    matches!(
        name,
        "w:bookmarkStart" | "w:bookmarkEnd" | "w:commentRangeStart" | "w:commentRangeEnd"
    )
}

/// The paragraph again: its own start tag and `w:pPr` as they were, its
/// markers, and the new text in one run with the first run's `w:rPr`
/// (or, tracked, the old text deleted and the new inserted).
fn rewrite_paragraph(para: &str, edit: &ParaEdit, author: &str, date: &str, id: u64) -> String {
    let open_end = para.find('>').map_or(para.len(), |e| e + 1);
    let self_closing = para[..open_end].ends_with("/>");
    let open_tag = if self_closing {
        format!("{}>", para[..open_end - 2].trim_end())
    } else {
        para[..open_end].to_owned()
    };
    let kids = if self_closing {
        Vec::new()
    } else {
        children(para)
    };
    let mut ppr = String::new();
    let mut before = String::new();
    let mut after = String::new();
    let mut seen_content = false;
    for (name, start, end) in &kids {
        let slice = &para[*start..*end];
        if *name == "w:pPr" {
            ppr.push_str(slice);
        } else if is_marker(name) {
            if seen_content {
                after.push_str(slice);
            } else {
                before.push_str(slice);
            }
        } else if *name == "w:r" && slice.contains("<w:commentReference") {
            after.push_str(slice);
        } else if *name != "w:proofErr" {
            seen_content = true;
        }
    }
    let rpr = first_rpr(para).unwrap_or_default();
    let new_run = |tag: &str, text: &str| {
        format!(
            "<w:r>{rpr}<w:{tag} xml:space=\"preserve\">{}</w:{tag}></w:r>",
            escape_text(text)
        )
    };
    let body = if edit.tracked {
        let attrs = format!(
            "w:author=\"{}\" w:date=\"{}\"",
            escape_attr(author),
            escape_attr(date)
        );
        format!(
            "<w:del w:id=\"{id}\" {attrs}>{}</w:del><w:ins w:id=\"{}\" {attrs}>{}</w:ins>",
            new_run("delText", &paragraph_text(para)),
            id + 1,
            new_run("t", &edit.text)
        )
    } else {
        new_run("t", &edit.text)
    };
    format!("{open_tag}{ppr}{before}{body}{after}</w:p>")
}

/// The first text run's `w:rPr`, as written.
fn first_rpr(para: &str) -> Option<String> {
    for (name, start, end) in children(para) {
        if name != "w:r" {
            continue;
        }
        let run = &para[start..end];
        if run.contains("<w:commentReference") {
            continue;
        }
        let from = run.find("<w:rPr")?;
        let rest = &run[from..];
        let head_end = rest.find('>')? + 1;
        if rest[..head_end].ends_with("/>") {
            return Some(rest[..head_end].to_owned());
        }
        let close = rest.find("</w:rPr>")? + "</w:rPr>".len();
        return Some(rest[..close].to_owned());
    }
    None
}

/// The largest `w:id` in the document (revisions, comments, bookmarks share
/// the attribute; a new revision takes one above them all).
fn max_revision_id(xml: &str) -> u64 {
    let mut max = 0;
    let mut rest = xml;
    while let Some(at) = rest.find("w:id=\"") {
        let tail = &rest[at + 6..];
        let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
        if let Ok(n) = digits.parse::<u64>() {
            max = max.max(n);
        }
        rest = tail;
    }
    max
}

fn escape_text(s: &str) -> String {
    s.chars()
        .filter(|&c| {
            c == '\t' || c == '\n' || c == '\r' || (c >= ' ' && c != '\u{FFFE}' && c != '\u{FFFF}')
        })
        .collect::<String>()
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn escape_attr(s: &str) -> String {
    escape_text(s).replace('"', "&quot;")
}

fn decode(s: &str) -> String {
    if !s.contains('&') {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        let Some(semi) = tail.find(';') else {
            out.push_str(tail);
            return out;
        };
        let entity = &tail[1..semi];
        let ch = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            e if e.starts_with("#x") || e.starts_with("#X") => u32::from_str_radix(&e[2..], 16)
                .ok()
                .and_then(char::from_u32),
            e if e.starts_with('#') => e[1..].parse::<u32>().ok().and_then(char::from_u32),
            _ => None,
        };
        match ch {
            Some(c) => out.push(c),
            None => out.push_str(&tail[..=semi]),
        }
        rest = &tail[semi + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests;
