//! Turning the model's evidence into anchors (§3.2). The model names a
//! block and quotes it; the OS looks for the quote, by `find`'s rules
//! (§3.1, Q14), only in that block and only among the blocks the model was
//! shown. A quote found exactly once is an anchor; found nowhere, or more
//! than once, it is not. A value's evidence counts whole or not at all: a
//! value keeps no partial evidence, which could drop the condition or the
//! negation that decides its meaning. And the value itself must be in one
//! of its quotes. What the OS cannot check is whether the quotes support
//! the value; that is for the reader, who sees every anchor.

use std::collections::HashSet;

use super::prompt::Evidence;
use super::window::Window;
use crate::extractions::{Anchor, Engine, Geometry, TextRange};
use crate::read::matcher::Query;
use crate::text_layer::{Block, TextLayer, sha256_address};

/// The revision and its pinned layer, which anchors point into.
pub struct Source<'a> {
    pub document_id: &'a str,
    pub revision: i64,
    pub content_sha256: &'a str,
    pub media_type: &'a str,
    pub text_layer_sha256: &'a str,
    pub layer: &'a TextLayer,
}

/// Why a value has no anchors: its `unsourced_reason`.
pub mod unsourced {
    pub const NO_EVIDENCE: &str = "the model gave no evidence";
    pub const NOT_SHOWN: &str = "a quote names a block the model was not shown";
    pub const NOT_FOUND: &str = "a quote is not in the block it names";
    pub const AMBIGUOUS: &str = "a quote occurs more than once in its block";
    pub const VALUE_NOT_QUOTED: &str = "the value is in none of its quotes";
}

/// Where `quote` is in `text`, if exactly once. `find` gives matches that
/// do not overlap, so a second one is looked for from just after the first
/// one's start: "ana" is twice in "banana". None can start earlier: the
/// first match `find` gives ends first and, of those, starts earliest, and
/// a later start never overtakes an earlier match (" a " is at 0..4 and
/// 1..4 in "  a  ": found at 0, then again from 1).
fn once(quote: &str, text: &str) -> Result<(usize, usize), &'static str> {
    let Some(q) = Query::new(quote) else {
        return Err(unsourced::NOT_FOUND);
    };
    let Some(&(start, end)) = q.find(quote, text).first() else {
        return Err(unsourced::NOT_FOUND);
    };
    let next = start + text[start..].chars().next().map_or(1, char::len_utf8);
    if q.find(quote, &text[next..]).is_empty() {
        Ok((start, end))
    } else {
        Err(unsourced::AMBIGUOUS)
    }
}

impl Source<'_> {
    /// The anchor of `b.text[start..end]`: the whole lines it touches, as
    /// `find` gives them.
    fn anchor(&self, b: &Block, (start, end): (usize, usize)) -> Anchor {
        let rects: Vec<[f64; 4]> = b
            .lines
            .iter()
            .filter(|l| l.start < end && l.end > start)
            .map(|l| l.rect)
            .collect();
        Anchor {
            document_id: self.document_id.into(),
            revision: self.revision,
            content_sha256: self.content_sha256.into(),
            media_type: self.media_type.into(),
            text_layer_sha256: self.text_layer_sha256.into(),
            engine: Engine {
                id: self.layer.engine.id.clone(),
                version: self.layer.engine.version.clone(),
            },
            block_id: b.block_id.clone(),
            page: Some(b.page),
            block_text_sha256: sha256_address(b.text.as_bytes()),
            text_range: TextRange {
                unit: "utf8".into(),
                start: start as u64,
                end: end as u64,
            },
            geometry: Some(Geometry {
                frame: "CropBox".into(),
                unit: "pt".into(),
                origin: "top-left-rotated".into(),
                rects,
            }),
            quote: b.text[start..end].into(),
        }
    }

    /// The anchors of `value`'s `evidence`, read in `window`: all of them,
    /// or why there are none.
    ///
    /// # Errors
    /// The `unsourced_reason` when any quote fails, or the value is in none.
    pub fn ground(
        &self,
        window: &Window,
        value: &str,
        evidence: &[Evidence],
    ) -> Result<Vec<Anchor>, &'static str> {
        if evidence.is_empty() {
            return Err(unsourced::NO_EVIDENCE);
        }
        let shown: HashSet<&str> = window
            .0
            .iter()
            .map(|&i| self.layer.blocks[i].block_id.as_str())
            .collect();
        let mut anchors = Vec::with_capacity(evidence.len());
        for e in evidence {
            if !shown.contains(e.block_id.as_str()) {
                return Err(unsourced::NOT_SHOWN);
            }
            let Some(b) = self.layer.blocks.iter().find(|b| b.block_id == e.block_id) else {
                return Err(unsourced::NOT_SHOWN);
            };
            anchors.push(self.anchor(b, once(&e.quote, &b.text)?));
        }
        let quoted = Query::new(value)
            .is_some_and(|q| anchors.iter().any(|a| !q.find(value, &a.quote).is_empty()));
        if !quoted {
            return Err(unsourced::VALUE_NOT_QUOTED);
        }
        Ok(anchors)
    }
}

#[cfg(test)]
mod tests;
