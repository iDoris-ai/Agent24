//! L6-S05: compare two lists of text units (paragraphs or blocks).
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum BlockRef {
    Paragraph { paragraph_index: usize },
    Block { block_id: String, page: u32 },
}
#[derive(Debug, Clone)]
pub struct Unit {
    pub block_ref: BlockRef,
    pub text: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Added,
    Removed,
    Changed,
    Unaligned,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Equal,
    Insert,
    Delete,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Span {
    pub op: Op,
    pub text: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Side {
    pub block_ref: BlockRef,
    pub text: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Change {
    pub id: String,
    pub kind: Kind,
    pub left: Option<Side>,
    pub right: Option<Side>,
    pub spans: Vec<Span>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Summary {
    pub total: usize,
    pub added: usize,
    pub removed: usize,
    pub changed: usize,
    pub unaligned: usize,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Comparison {
    pub identical: bool,
    pub only_text: bool,
    pub summary: Summary,
    pub changes: Vec<Change>,
}

/// Two paragraphs this alike (0 to 1, by characters) are one paragraph edited.
const PAIR_RATIO: f32 = 0.5;

/// One unit with its text normalised: whitespace runs are one space and the
/// ends are trimmed, so spacing and empty paragraphs are never differences.
fn normalised(units: &[Unit]) -> Vec<(usize, String)> {
    units
        .iter()
        .enumerate()
        .filter_map(|(i, u)| {
            let t = u.text.split_whitespace().collect::<Vec<_>>().join(" ");
            (!t.is_empty()).then_some((i, t))
        })
        .collect()
}

fn side(units: &[Unit], (i, text): &(usize, String)) -> Side {
    Side {
        block_ref: units[*i].block_ref.clone(),
        text: text.clone(),
    }
}

/// The character-level spans from `a` to `b`, same-op neighbours merged.
fn spans(a: &str, b: &str) -> Vec<Span> {
    use similar::{ChangeTag, TextDiff};
    let mut out: Vec<Span> = Vec::new();
    for c in TextDiff::from_chars(a, b).iter_all_changes() {
        let op = match c.tag() {
            ChangeTag::Equal => Op::Equal,
            ChangeTag::Insert => Op::Insert,
            ChangeTag::Delete => Op::Delete,
        };
        match out.last_mut() {
            Some(last) if last.op == op => last.text.push_str(c.value()),
            _ => out.push(Span {
                op,
                text: c.value().to_owned(),
            }),
        }
    }
    out
}

fn change(kind: Kind, left: Option<Side>, right: Option<Side>, spans: Vec<Span>) -> Change {
    Change {
        id: String::new(),
        kind,
        left,
        right,
        spans,
    }
}

/// Compare two documents' text units, paragraph by paragraph, then by
/// character inside paragraphs that pair up. Paragraphs that only moved are
/// `unaligned` (listed on both sides), never reported as edited.
#[must_use]
pub fn compare(left: &[Unit], right: &[Unit]) -> Comparison {
    use similar::{Algorithm, DiffOp, TextDiff, capture_diff_slices};
    let l = normalised(left);
    let r = normalised(right);
    let lt: Vec<&str> = l.iter().map(|x| x.1.as_str()).collect();
    let rt: Vec<&str> = r.iter().map(|x| x.1.as_str()).collect();
    let mut changes: Vec<Change> = Vec::new();
    for op in capture_diff_slices(Algorithm::Myers, &lt, &rt) {
        match op {
            DiffOp::Equal { .. } => {}
            DiffOp::Delete {
                old_index, old_len, ..
            } => {
                for x in &l[old_index..old_index + old_len] {
                    changes.push(change(Kind::Removed, Some(side(left, x)), None, Vec::new()));
                }
            }
            DiffOp::Insert {
                new_index, new_len, ..
            } => {
                for y in &r[new_index..new_index + new_len] {
                    changes.push(change(Kind::Added, None, Some(side(right, y)), Vec::new()));
                }
            }
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                let mut next = new_index; // rights before this are settled
                let end = new_index + new_len;
                for x in &l[old_index..old_index + old_len] {
                    let best = (next..end)
                        .map(|j| (j, TextDiff::from_chars(&x.1, &r[j].1).ratio()))
                        .filter(|(_, q)| *q >= PAIR_RATIO)
                        .fold(None::<(usize, f32)>, |m, c| match m {
                            Some(b) if b.1 >= c.1 => Some(b),
                            _ => Some(c),
                        });
                    match best {
                        Some((j, _)) => {
                            for y in &r[next..j] {
                                changes.push(change(
                                    Kind::Added,
                                    None,
                                    Some(side(right, y)),
                                    Vec::new(),
                                ));
                            }
                            changes.push(change(
                                Kind::Changed,
                                Some(side(left, x)),
                                Some(side(right, &r[j])),
                                spans(&x.1, &r[j].1),
                            ));
                            next = j + 1;
                        }
                        None => changes.push(change(
                            Kind::Removed,
                            Some(side(left, x)),
                            None,
                            Vec::new(),
                        )),
                    }
                }
                for y in &r[next..end] {
                    changes.push(change(Kind::Added, None, Some(side(right, y)), Vec::new()));
                }
            }
        }
    }
    // A paragraph removed in one place and added word for word in another
    // moved: neither is an edit.
    let moved: Vec<String> = changes
        .iter()
        .filter(|c| c.kind == Kind::Removed)
        .filter_map(|c| c.left.as_ref().map(|s| s.text.clone()))
        .filter(|t| {
            changes
                .iter()
                .any(|c| c.kind == Kind::Added && c.right.as_ref().is_some_and(|s| &s.text == t))
        })
        .collect();
    for c in &mut changes {
        let t = match (c.kind, &c.left, &c.right) {
            (Kind::Removed, Some(s), _) | (Kind::Added, _, Some(s)) => &s.text,
            _ => continue,
        };
        if moved.contains(t) {
            c.kind = Kind::Unaligned;
        }
    }
    let mut summary = Summary {
        total: changes.len(),
        added: 0,
        removed: 0,
        changed: 0,
        unaligned: 0,
    };
    for (i, c) in changes.iter_mut().enumerate() {
        c.id = format!("c{}", i + 1);
        match c.kind {
            Kind::Added => summary.added += 1,
            Kind::Removed => summary.removed += 1,
            Kind::Changed => summary.changed += 1,
            Kind::Unaligned => summary.unaligned += 1,
        }
    }
    Comparison {
        identical: changes.is_empty(),
        only_text: true,
        summary,
        changes,
    }
}

pub mod route;
pub mod tool;

#[cfg(test)]
mod tests;
