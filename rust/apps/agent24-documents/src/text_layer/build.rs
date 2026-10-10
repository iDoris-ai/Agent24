//! From the PDFKit helper's report (each page's lines in reading order) to a
//! text layer (ADR-DOC-02 §3.1): rows joined, paragraphs grouped into blocks
//! within the caps. Pure, so tested anywhere; the engine runs only on macOS.

use serde::Deserialize;
use serde_json::Value;

use super::{
    Block, EngineRef, Line, MAX_BLOCK_BYTES, MAX_LINES, MAX_REGION_RECTS, ParseStatus, REASONS,
    Rect, Region, TextLayer,
};

/// What the helper prints (protocol 1).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Read {
    pub protocol: u32,
    pub os_version: String,
    pub pages: Vec<ReadPage>,
    pub unparsed: Vec<ReadRegion>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadPage {
    pub page: u32,
    pub width: f64,
    pub height: f64,
    pub lines: Vec<ReadLine>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadLine {
    pub text: String,
    pub rect: Rect,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadRegion {
    pub page: u32,
    pub rects: Vec<Rect>,
    pub reason: String,
}

/// A paragraph ends where the next line is more line heights below than this,
/// or a heading changes the height (tuned on S01: OCR heights vary a lot).
const PARAGRAPH_GAP: f64 = 1.2;
const HEIGHT_CHANGE: f64 = 0.5;

fn height(r: &Rect) -> f64 {
    r[3] - r[1]
}

fn union(a: &Rect, b: &Rect) -> Rect {
    let (lo, hi) = (|i: usize| a[i].min(b[i]), |i: usize| a[i].max(b[i]));
    [lo(0), lo(1), hi(2), hi(3)]
}

/// The layer for `content_sha256`, or why the helper's report cannot be one.
pub fn layer(
    content_sha256: &str,
    engine: EngineRef,
    config: Value,
    read: Read,
) -> Result<TextLayer, String> {
    if read.protocol != 1 {
        return Err(format!("helper protocol {}", read.protocol));
    }
    let pages = u32::try_from(read.pages.len()).map_err(|_| "too many pages".to_owned())?;
    let mut blocks = Vec::new();
    for (i, p) in read.pages.iter().enumerate() {
        let ok = |v: f64| v.is_finite() && v > 0.0;
        if p.page as usize != i + 1 || !ok(p.width) || !ok(p.height) {
            return Err(format!("page {}: out of order or without a size", p.page));
        }
        let mut n = 0;
        for lines in paragraphs(rows(clean(p)?)) {
            for part in within_caps(lines) {
                n += 1;
                blocks.push(block(p.page, n, part));
            }
        }
    }
    let mut unparsed_regions = Vec::new();
    for r in read.unparsed {
        let Some(p) = read.pages.get((r.page as usize).wrapping_sub(1)) else {
            return Err(format!("region on page {}: no such page", r.page));
        };
        if !REASONS.contains(&r.reason.as_str()) {
            return Err(format!("region on page {}: unknown reason", r.page));
        }
        // Too many to list, or none: the whole page, wider never narrower.
        let rects = if r.rects.is_empty() || r.rects.len() > MAX_REGION_RECTS {
            vec![[0.0, 0.0, p.width, p.height]]
        } else {
            r.rects
                .iter()
                .map(|q| clamp(q, p))
                .collect::<Result<_, _>>()?
        };
        unparsed_regions.push(Region {
            page: r.page,
            rects,
            reason: r.reason,
        });
    }
    let layer = TextLayer {
        v: 1,
        content_sha256: content_sha256.to_owned(),
        engine,
        config,
        pages,
        parse_status: if unparsed_regions.is_empty() {
            ParseStatus::Complete
        } else {
            ParseStatus::Partial
        },
        unparsed_regions,
        blocks,
    };
    layer.check()?;
    Ok(layer)
}

/// A rectangle inside its page, corners in order.
fn clamp(r: &Rect, p: &ReadPage) -> Result<Rect, String> {
    if !r.iter().all(|v| v.is_finite()) {
        return Err(format!("page {}: a rectangle that is not a number", p.page));
    }
    let side = |a: f64, b: f64, end: f64| (a.min(b).clamp(0.0, end), a.max(b).clamp(0.0, end));
    let ((x0, x1), (y0, y1)) = (side(r[0], r[2], p.width), side(r[1], r[3], p.height));
    Ok([x0, y0, x1, y1])
}

/// The page's lines with their rectangles inside the page; a line holds no
/// break and is not empty.
fn clean(p: &ReadPage) -> Result<Vec<ReadLine>, String> {
    p.lines
        .iter()
        .filter(|l| !l.text.is_empty())
        .map(|l| {
            if l.text.contains('\n') {
                return Err(format!("page {}: a line with a break in it", p.page));
            }
            Ok(ReadLine {
                text: l.text.clone(),
                rect: clamp(&l.rect, p)?,
            })
        })
        .collect()
}

/// A visual line, and the helper's rectangles for its first and last piece:
/// the parts of a selection split at a break share one, marking the break.
struct Row {
    text: String,
    rect: Rect,
    head: Rect,
    tail: Rect,
}

/// Joins a piece to the row it follows on (half a line of overlap, close to
/// its right) unless a break parts them. A space goes between Latin letters
/// or digits more than an eighth of a line apart (a word space is a quarter).
fn rows(lines: Vec<ReadLine>) -> Vec<Row> {
    let mut out: Vec<Row> = Vec::new();
    for l in lines {
        if let Some(last) = out.last_mut() {
            let h = height(&last.rect).min(height(&l.rect));
            let overlap = last.rect[3].min(l.rect[3]) - last.rect[1].max(l.rect[1]);
            let gap = l.rect[0] - last.rect[2];
            if l.rect != last.tail
                && h > 0.0
                && overlap >= h / 2.0
                && (-2.0..=1.5 * h).contains(&gap)
            {
                let word = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric());
                if gap > h / 8.0 && (word(last.text.chars().last()) || word(l.text.chars().next()))
                {
                    last.text.push(' ');
                }
                last.text.push_str(&l.text);
                (last.rect, last.tail) = (union(&last.rect, &l.rect), l.rect);
                continue;
            }
        }
        out.push(Row {
            text: l.text,
            rect: l.rect,
            head: l.rect,
            tail: l.rect,
        });
    }
    out
}

/// Groups rows into paragraphs: a row continues one below it that it overlaps
/// across (not another column), within [`PARAGRAPH_GAP`] and [`HEIGHT_CHANGE`],
/// or as the next part of the same selection.
fn paragraphs(rows: Vec<Row>) -> Vec<Vec<Row>> {
    let mut out: Vec<Vec<Row>> = Vec::new();
    for r in rows {
        if let Some(prev) = out.last().and_then(|p| p.last()) {
            let (h, ph) = (height(&r.rect), height(&prev.rect));
            let gap = r.rect[1] - prev.rect[3];
            let tall = h.max(ph);
            let across = r.rect[2].min(prev.rect[2]) - r.rect[0].max(prev.rect[0]);
            let continues = r.head == prev.tail
                || (gap >= -tall / 2.0
                    && gap <= PARAGRAPH_GAP * tall
                    && across > 0.0
                    && (h - ph).abs() <= HEIGHT_CHANGE * tall);
            if let (true, Some(p)) = (continues, out.last_mut()) {
                p.push(r);
                continue;
            }
        }
        out.push(vec![r]);
    }
    out
}

/// Blocks of at most [`MAX_LINES`] rows and [`MAX_BLOCK_BYTES`] bytes, breaks
/// included; a row too long is cut at character boundaries, a block a piece.
fn within_caps(rows: Vec<Row>) -> Vec<Vec<Row>> {
    let mut out: Vec<Vec<Row>> = Vec::new();
    let mut size = 0;
    for piece in rows.into_iter().flat_map(pieces) {
        // Its text, and the break before it if it is not the first.
        let grown = size + 1 + piece.text.len();
        match out.last_mut() {
            Some(b) if b.len() < MAX_LINES && grown <= MAX_BLOCK_BYTES => {
                size = grown;
                b.push(piece);
            }
            _ => {
                size = piece.text.len();
                out.push(vec![piece]);
            }
        }
    }
    out
}

/// A row cut so each piece fits a block.
fn pieces(r: Row) -> Vec<Row> {
    if r.text.len() <= MAX_BLOCK_BYTES {
        return vec![r];
    }
    let mut out = Vec::new();
    let mut rest = r.text.as_str();
    while !rest.is_empty() {
        let mut cut = rest.len().min(MAX_BLOCK_BYTES);
        while !rest.is_char_boundary(cut) {
            cut -= 1;
        }
        let text = rest[..cut].to_owned();
        out.push(Row { text, ..r });
        rest = &rest[cut..];
    }
    out
}

/// Block `n` of `page`: its rows joined by breaks, each break owned by the
/// line it ends (§3.1).
fn block(page: u32, n: usize, rows: Vec<Row>) -> Block {
    let mut text = String::new();
    let mut lines = Vec::with_capacity(rows.len());
    let last = rows.len() - 1;
    for (i, r) in rows.into_iter().enumerate() {
        let start = text.len();
        text.push_str(&r.text);
        if i < last {
            text.push('\n');
        }
        lines.push(Line {
            start,
            end: text.len(),
            rect: r.rect,
        });
    }
    Block {
        block_id: format!("p{page}/b{n}"),
        page,
        text,
        lines,
    }
}

#[cfg(test)]
mod tests;
