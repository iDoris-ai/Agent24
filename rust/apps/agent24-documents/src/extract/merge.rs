//! One field's value from what every window said of it (§3.2). Each
//! window's present values and conflict candidates become claims; claims
//! with the same normal form (or, without one, the same wording) are one
//! value. One value is `present`, several a `conflict` the extractor never
//! resolves, in document order and at most eight, none `missing`. Evidence
//! is kept whole per window and never counted twice.

use super::ground::Source;
use super::prompt::{AnswerStatus, AnswerValue};
use super::window::Window;
use crate::extractions::{Anchor, Candidate, ExtractedValue, MissingReason, Status};

/// Candidates in a conflict, at most (the contract's limit).
pub const MAX_CANDIDATES: usize = 8;
/// Anchors on one value, at most.
pub const MAX_ANCHORS: usize = 16;
/// Why a value that has evidence shows none: it would not fit a page.
pub const TOO_LARGE: &str = "its evidence is too large to keep";

/// A value one window asserts, its evidence grounded or why not, and where
/// in the document it stands: its first anchor's block and offset, or,
/// without anchors, the end of the first block the window was shown.
#[derive(Debug, Clone, PartialEq)]
pub struct Claim {
    pub value: String,
    pub normalized: String,
    pub anchors: Result<Vec<Anchor>, &'static str>,
    pub at: (usize, u64),
}

/// What one window said of a field.
#[derive(Debug, Clone, PartialEq)]
pub enum Said {
    Values(Vec<Claim>),
    Missing(String),
}

/// What `a`, answered over `window`, says, its evidence grounded in `src`.
#[must_use]
pub fn said(src: &Source<'_>, window: &Window, a: &AnswerValue) -> Said {
    let claim = |value: &str, normalized: &str, evidence| {
        let anchors = src.ground(window, value, evidence);
        let first = window.0.first().copied().unwrap_or(0);
        let at = anchors
            .as_ref()
            .ok()
            .and_then(|a| a.first())
            .and_then(|a| {
                let block = src
                    .layer
                    .blocks
                    .iter()
                    .position(|b| b.block_id == a.block_id)?;
                Some((block, a.text_range.start))
            })
            .unwrap_or((first, u64::MAX));
        Claim {
            value: value.into(),
            normalized: normalized.into(),
            anchors,
            at,
        }
    };
    match a.status {
        AnswerStatus::Present => Said::Values(vec![claim(&a.value, &a.normalized, &a.evidence)]),
        AnswerStatus::Conflict => Said::Values(
            a.candidates
                .iter()
                .map(|c| claim(&c.value, &c.normalized, &c.evidence))
                .collect(),
        ),
        AnswerStatus::Missing => Said::Missing(a.missing_reason.clone()),
    }
}

/// Claims that are one value: the first wording, and evidence from each
/// window that grounded it, whole, while it fits.
struct Group {
    at: (usize, u64),
    by_normal: bool,
    value: String,
    normalized: String,
    anchors: Vec<Anchor>,
    unsourced: Option<&'static str>,
}

impl Group {
    fn add(&mut self, claim: &Claim) {
        self.at = self.at.min(claim.at);
        match &claim.anchors {
            Ok(set) => {
                let mut new: Vec<&Anchor> = Vec::new();
                for a in set {
                    if !self.anchors.contains(a) && !new.contains(&a) {
                        new.push(a);
                    }
                }
                if self.anchors.len() + new.len() <= MAX_ANCHORS {
                    self.anchors.extend(new.into_iter().cloned());
                }
            }
            Err(why) => {
                self.unsourced.get_or_insert(why);
            }
        }
    }

    /// Anchors, or none and why.
    fn evidence(&self) -> (Vec<Anchor>, Option<String>) {
        if self.anchors.is_empty() {
            (vec![], Some(self.unsourced.unwrap_or(TOO_LARGE).into()))
        } else {
            (self.anchors.clone(), None)
        }
    }
}

/// A field's merged value, and how many values past the first eight in
/// document order were left out (for the job's progress).
#[derive(Debug, Clone, PartialEq)]
pub struct Merged {
    pub value: ExtractedValue,
    pub dropped: usize,
}

/// The value of field `key` from what each window `said`, in window order.
/// `partial`: the layer has regions not read, so absence is never claimed.
/// A value over `max_bytes` serialized keeps its values and candidates but
/// drops their anchors, saying why.
///
/// # Errors
/// When even without anchors it is over `max_bytes`: never cut down to
/// fit, which could resolve a conflict. Answers are held to
/// `MAX_VALUE_CHARS`, which keeps this from happening.
pub fn merge(key: &str, said: &[Said], partial: bool, max_bytes: usize) -> Result<Merged, String> {
    let mut groups: Vec<Group> = Vec::new();
    for claim in said
        .iter()
        .filter_map(|s| match s {
            Said::Values(c) => Some(c),
            Said::Missing(_) => None,
        })
        .flatten()
    {
        let by_normal = !claim.normalized.is_empty();
        let same = |g: &&mut Group| {
            g.by_normal == by_normal
                && if by_normal {
                    g.normalized == claim.normalized
                } else {
                    g.value == claim.value
                }
        };
        if let Some(g) = groups.iter_mut().find(|g| same(g)) {
            g.add(claim);
        } else {
            let mut g = Group {
                at: claim.at,
                by_normal,
                value: claim.value.clone(),
                normalized: claim.normalized.clone(),
                anchors: vec![],
                unsourced: None,
            };
            g.add(claim);
            groups.push(g);
        }
    }
    // Document order: the first eight are kept, the rest counted.
    groups.sort_by_key(|g| g.at);
    let dropped = groups.len().saturating_sub(MAX_CANDIDATES);
    groups.truncate(MAX_CANDIDATES);
    let mut v = ExtractedValue {
        key: key.into(),
        status: Status::Missing,
        value: None,
        normalized: None,
        anchors: None,
        unsourced_reason: None,
        missing_reason: None,
        candidates: None,
    };
    match &groups[..] {
        [] => v.missing_reason = Some(missing(said, partial)),
        [g] => {
            let (anchors, why) = g.evidence();
            v.status = Status::Present;
            v.value = Some(g.value.clone());
            v.normalized = g.by_normal.then(|| g.normalized.clone());
            (v.anchors, v.unsourced_reason) = (Some(anchors), why);
        }
        many => {
            v.status = Status::Conflict;
            let candidates = many.iter().map(|g| {
                let (anchors, unsourced_reason) = g.evidence();
                // A candidate always has a normal form: its wording, if none.
                let normalized = if g.by_normal { &g.normalized } else { &g.value };
                Candidate {
                    value: g.value.clone(),
                    normalized: normalized.clone(),
                    anchors,
                    unsourced_reason,
                }
            });
            v.candidates = Some(candidates.collect());
        }
    }
    let fits = |v: &ExtractedValue| serde_json::to_string(v).is_ok_and(|s| s.len() <= max_bytes);
    if !fits(&v) {
        drop_anchors(&mut v);
        if !fits(&v) {
            return Err(format!(
                "{key}: over {max_bytes} bytes even without its anchors"
            ));
        }
    }
    Ok(Merged { value: v, dropped })
}

/// Why a field is missing: the first specific reason a window gave, else
/// not in the document. A layer with unread regions cannot show absence:
/// the reasons that rest on it become `unread`.
fn missing(said: &[Said], partial: bool) -> MissingReason {
    let given = said.iter().find_map(|s| match s {
        Said::Missing(r) => match r.as_str() {
            "blank_in_template" => Some(MissingReason::BlankInTemplate),
            "referenced_but_absent" => Some(MissingReason::ReferencedButAbsent),
            "outside_page_scope" => Some(MissingReason::OutsidePageScope),
            _ => None,
        },
        Said::Values(_) => None,
    });
    match given.unwrap_or(MissingReason::NotInDocument) {
        MissingReason::NotInDocument | MissingReason::ReferencedButAbsent if partial => {
            MissingReason::Unread
        }
        reason => reason,
    }
}

/// The value without its anchors, each part saying why.
fn drop_anchors(v: &mut ExtractedValue) {
    if v.anchors.as_ref().is_some_and(|a| !a.is_empty()) {
        (v.anchors, v.unsourced_reason) = (Some(vec![]), Some(TOO_LARGE.into()));
    }
    for c in v.candidates.iter_mut().flatten() {
        if !c.anchors.is_empty() {
            (c.anchors, c.unsourced_reason) = (vec![], Some(TOO_LARGE.into()));
        }
    }
}

#[cfg(test)]
mod tests;
