//! An extracted value as the contract has it (`DocumentsExtractedValue`,
//! `DocumentsExtractCandidate`, `DocumentsAnchor`), and the rules JSON
//! Schema states and the OS checks: a value that breaks one is never
//! stored, and one read back broken is the OS's failure. A value and a
//! candidate allow no other properties; an anchor and its parts do, as the
//! contract has them. None of them is ever `null`.

use serde::{Deserialize, Serialize};

use serde_json::Value;

/// Anchors on one value or candidate, at most.
pub const MAX_ANCHORS: usize = 16;
/// Candidates in a conflict.
pub const CANDIDATES: std::ops::RangeInclusive<usize> = 2..=8;
const PAGINATED: [&str; 3] = ["application/pdf", "image/jpeg", "image/png"];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextRange {
    pub unit: String,
    pub start: u64,
    pub end: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Geometry {
    #[serde(rename = "box")]
    pub frame: String,
    pub unit: String,
    pub origin: String,
    pub rects: Vec<[f64; 4]>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Anchor {
    pub document_id: String,
    pub revision: i64,
    pub content_sha256: String,
    pub media_type: String,
    pub text_layer_sha256: String,
    pub engine: Engine,
    pub block_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,
    pub block_text_sha256: String,
    pub text_range: TextRange,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geometry: Option<Geometry>,
    pub quote: String,
}

/// `DocumentsEngineRef`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Engine {
    pub id: String,
    pub version: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Present,
    Missing,
    Conflict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissingReason {
    BlankInTemplate,
    NotInDocument,
    ReferencedButAbsent,
    OutsidePageScope,
    Unread,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    pub value: String,
    pub normalized: String,
    pub anchors: Vec<Anchor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unsourced_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtractedValue {
    pub key: String,
    pub status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normalized: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchors: Option<Vec<Anchor>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unsourced_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub missing_reason: Option<MissingReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidates: Option<Vec<Candidate>>,
}

fn is_sha256(s: &str) -> bool {
    s.strip_prefix("sha256:")
        .is_some_and(|h| h.len() == 64 && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
}

/// `DocumentsMediaType`: `^[a-z0-9.+-]+/[a-z0-9.+-]+$`.
fn is_media_type(s: &str) -> bool {
    let part = |p: &str| {
        !p.is_empty()
            && p.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b".+-".contains(&b))
    };
    s.split_once('/')
        .is_some_and(|(t, sub)| part(t) && part(sub))
}

/// Whether a property the contract declares is `null`: none of them is
/// nullable, and serde would read one as absent. Extra properties, which an
/// anchor and its parts may have, are left as they are, nulls included.
pub fn has_null(value: &Value) -> bool {
    const ANCHOR: &str = "document_id revision content_sha256 media_type text_layer_sha256 \
                          engine block_id page block_text_sha256 text_range geometry quote";
    let declared = |v: &Value, names: &[&str]| {
        v.as_object()
            .is_some_and(|o| names.iter().any(|n| o.get(*n).is_some_and(Value::is_null)))
    };
    let nested =
        |v: &Value, name: &str, names: &[&str]| v.get(name).is_some_and(|x| declared(x, names));
    let anchor = |a: &Value| {
        declared(a, &ANCHOR.split_whitespace().collect::<Vec<_>>())
            || nested(a, "engine", &["id", "version"])
            || nested(a, "text_range", &["unit", "start", "end"])
            || nested(a, "geometry", &["box", "unit", "origin", "rects"])
            || a.pointer("/geometry/rects")
                .and_then(Value::as_array)
                .is_some_and(|rects| {
                    rects
                        .iter()
                        .any(|r| r.as_array().is_some_and(|r| r.iter().any(Value::is_null)))
                })
    };
    let anchors = |v: &Value| {
        v.get("anchors")
            .and_then(Value::as_array)
            .is_some_and(|a| a.iter().any(anchor))
    };
    // A value and a candidate allow no other properties: any null is declared.
    let any = |v: &Value| {
        v.as_object()
            .is_some_and(|o| o.values().any(Value::is_null))
    };
    any(value)
        || anchors(value)
        || value
            .get("candidates")
            .and_then(Value::as_array)
            .is_some_and(|cs| cs.iter().any(|c| any(c) || anchors(c)))
}

/// `^[a-z][a-z0-9_]{0,63}$`, the field-key syntax.
pub fn is_field_key(k: &str) -> bool {
    let b = k.as_bytes();
    (1..=64).contains(&b.len())
        && b[0].is_ascii_lowercase()
        && b.iter()
            .all(|&c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
}

impl Anchor {
    fn check(&self) -> Result<(), String> {
        let shas = [
            &self.content_sha256,
            &self.text_layer_sha256,
            &self.block_text_sha256,
        ];
        let r = &self.text_range;
        let ok = crate::id::is_id(crate::id::IdKind::Document, &self.document_id)
            && self.revision >= 1
            && shas.iter().all(|s| is_sha256(s))
            && !self.engine.id.is_empty()
            && !self.engine.version.is_empty()
            && !self.block_id.is_empty()
            && !self.quote.is_empty()
            && is_media_type(&self.media_type)
            && r.unit == "utf8"
            && r.start <= r.end;
        if !ok {
            return Err(format!("anchor in {}: a field is malformed", self.block_id));
        }
        // Paginated sources give a page and its geometry; flowing ones neither.
        match (
            PAGINATED.contains(&self.media_type.as_str()),
            self.page,
            &self.geometry,
        ) {
            (true, Some(p), Some(g)) if p >= 1 => g.check(),
            (false, None, None) => Ok(()),
            _ => Err(format!(
                "anchor in {}: page and geometry do not fit {}",
                self.block_id, self.media_type
            )),
        }
    }
}

impl Geometry {
    fn check(&self) -> Result<(), String> {
        let rect = |[x0, y0, x1, y1]: [f64; 4]| {
            [x0, y0, x1, y1].iter().all(|v| v.is_finite() && *v >= 0.0) && x0 <= x1 && y0 <= y1
        };
        let ok = self.frame == "CropBox"
            && self.unit == "pt"
            && self.origin == "top-left-rotated"
            && !self.rects.is_empty()
            && self.rects.iter().all(|r| rect(*r));
        ok.then_some(())
            .ok_or_else(|| "geometry is malformed".into())
    }
}

/// Evidence, or none and why: both or neither is a break (§3).
fn evidence(anchors: &[Anchor], unsourced: Option<&String>) -> Result<(), String> {
    if anchors.len() > MAX_ANCHORS {
        return Err(format!("{} anchors", anchors.len()));
    }
    match (anchors.is_empty(), unsourced) {
        (true, Some(why)) if !why.is_empty() => Ok(()),
        (false, None) => anchors.iter().try_for_each(Anchor::check),
        _ => Err("anchors and an unsourced reason go one or the other".into()),
    }
}

impl ExtractedValue {
    /// The rules of `DocumentsExtractedValue` for this value's status.
    pub fn check(&self) -> Result<(), String> {
        if !is_field_key(&self.key) {
            return Err(format!("{:?} is not a field key", self.key));
        }
        let none = |what: &[bool]| what.iter().all(|present| !present);
        let ok = match self.status {
            Status::Present => {
                let Some(anchors) = &self.anchors else {
                    return Err(format!("{}: present without anchors", self.key));
                };
                evidence(anchors, self.unsourced_reason.as_ref())
                    .map_err(|e| format!("{}: {e}", self.key))?;
                self.value.is_some()
                    && none(&[self.missing_reason.is_some(), self.candidates.is_some()])
            }
            Status::Missing => {
                self.missing_reason.is_some()
                    && none(&[
                        self.value.is_some(),
                        self.normalized.is_some(),
                        self.anchors.is_some(),
                        self.unsourced_reason.is_some(),
                        self.candidates.is_some(),
                    ])
            }
            Status::Conflict => {
                let Some(candidates) = &self.candidates else {
                    return Err(format!("{}: conflict without candidates", self.key));
                };
                if !CANDIDATES.contains(&candidates.len()) {
                    return Err(format!("{}: {} candidates", self.key, candidates.len()));
                }
                for c in candidates {
                    evidence(&c.anchors, c.unsourced_reason.as_ref())
                        .map_err(|e| format!("{}: {e}", self.key))?;
                }
                none(&[
                    self.value.is_some(),
                    self.normalized.is_some(),
                    self.anchors.is_some(),
                    self.unsourced_reason.is_some(),
                    self.missing_reason.is_some(),
                ])
            }
        };
        ok.then_some(())
            .ok_or_else(|| format!("{}: fields do not fit status {:?}", self.key, self.status))
    }
}

#[cfg(test)]
mod tests;
