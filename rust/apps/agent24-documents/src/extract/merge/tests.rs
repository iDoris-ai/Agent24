#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::json;

use super::*;
use crate::extract::prompt::{AnswerCandidate, Evidence};
use crate::extractions::{Engine, Geometry, TextRange};
use crate::text_layer::{Block, EngineRef, Line, ParseStatus, TextLayer};

const DOC: &str = "doc_01K74Z3QJ8V5N2W9RTX6YB4M00";
const SHA: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
const MAX: usize = 1 << 20;

/// An anchor on block `b`, bytes `start..start + 4`.
fn anchor(b: u32, start: u64) -> Anchor {
    Anchor {
        document_id: DOC.into(),
        revision: 1,
        content_sha256: SHA.into(),
        media_type: "application/pdf".into(),
        text_layer_sha256: SHA.into(),
        engine: Engine {
            id: "e".into(),
            version: "1".into(),
        },
        block_id: format!("p1/b{b}"),
        page: Some(1),
        block_text_sha256: SHA.into(),
        text_range: TextRange {
            unit: "utf8".into(),
            start,
            end: start + 4,
        },
        geometry: Some(Geometry {
            frame: "CropBox".into(),
            unit: "pt".into(),
            origin: "top-left-rotated".into(),
            rects: vec![[0.0, 0.0, 1.0, 1.0]],
        }),
        quote: "abcd".into(),
    }
}

/// A claim at the start of the document, where order does not matter.
fn claim(value: &str, normalized: &str, anchors: Result<Vec<Anchor>, &'static str>) -> Claim {
    claim_at(0, value, normalized, anchors)
}

fn claim_at(
    block: usize,
    value: &str,
    normalized: &str,
    anchors: Result<Vec<Anchor>, &'static str>,
) -> Claim {
    Claim {
        value: value.into(),
        normalized: normalized.into(),
        anchors,
        at: (block, 0),
    }
}

fn one(c: Claim) -> Said {
    Said::Values(vec![c])
}

/// The merge, checked against the contract as the store checks it.
fn merged(said: &[Said], partial: bool) -> ExtractedValue {
    let m = merge("date", said, partial, MAX).unwrap();
    assert_eq!(m.value.check(), Ok(()), "{:?}", m.value);
    m.value
}

#[test]
fn one_value_is_present_with_its_evidence_or_why_it_has_none() {
    let v = merged(
        &[one(claim("10月12日", "2026-10-12", Ok(vec![anchor(1, 0)])))],
        false,
    );
    assert_eq!(
        (v.status, v.value.as_deref(), v.normalized.as_deref()),
        (Status::Present, Some("10月12日"), Some("2026-10-12"))
    );
    assert_eq!(v.anchors, Some(vec![anchor(1, 0)]));
    let v = merged(
        &[one(claim(
            "全天",
            "",
            Err("a quote is not in the block it names"),
        ))],
        false,
    );
    assert_eq!(
        (v.normalized, v.anchors, v.unsourced_reason.as_deref()),
        (
            None,
            Some(vec![]),
            Some("a quote is not in the block it names")
        )
    );
}

#[test]
fn windows_that_agree_are_one_value_and_overlaps_count_once() {
    // The same quote seen in two overlapping windows, and a second one.
    let v = merged(
        &[
            one(claim("10月12日", "2026-10-12", Ok(vec![anchor(1, 0)]))),
            one(claim(
                "10月12日",
                "2026-10-12",
                Ok(vec![anchor(1, 0), anchor(2, 8)]),
            )),
            one(claim("2026年10月12日", "2026-10-12", Err("x"))),
        ],
        false,
    );
    assert_eq!(
        (v.status, v.value.as_deref()),
        (Status::Present, Some("10月12日")),
        "the first wording"
    );
    assert_eq!(v.anchors, Some(vec![anchor(1, 0), anchor(2, 8)]));
    // One window grounded it: it has evidence, whatever another could not.
    assert_eq!(v.unsourced_reason, None);
}

#[test]
fn windows_that_differ_are_a_conflict_never_resolved() {
    let v = merged(
        &[
            one(claim(
                "2026年10月12日",
                "2026-10-12",
                Ok(vec![anchor(1, 0)]),
            )),
            Said::Missing("not_in_document".into()),
            Said::Values(vec![
                claim(
                    "10月13日",
                    "2026-10-13",
                    Err("a quote occurs more than once in its block"),
                ),
                claim("10月12日", "2026-10-12", Ok(vec![anchor(3, 0)])),
            ]),
        ],
        false,
    );
    assert_eq!(v.status, Status::Conflict);
    let c = v.candidates.unwrap();
    assert_eq!(
        c.iter().map(|c| c.value.as_str()).collect::<Vec<_>>(),
        ["2026年10月12日", "10月13日"]
    );
    assert_eq!(c[0].anchors, vec![anchor(1, 0), anchor(3, 0)]);
    // Kept without evidence, not dropped: dropping it would choose the other.
    assert_eq!(
        (c[1].anchors.len(), c[1].unsourced_reason.as_deref()),
        (0, Some("a quote occurs more than once in its block"))
    );
    // Without a normal form, the wording is one, and the comparison.
    let v = merged(
        &[
            one(claim("甲", "", Ok(vec![anchor(1, 0)]))),
            one(claim("乙", "", Ok(vec![anchor(2, 0)]))),
        ],
        false,
    );
    assert_eq!(
        v.candidates
            .unwrap()
            .iter()
            .map(|c| c.normalized.as_str())
            .collect::<Vec<_>>(),
        ["甲", "乙"]
    );
}

#[test]
fn more_than_eight_values_keep_the_first_eight_in_document_order() {
    // Codex R1: the first window finds the values at blocks 2..=9; an
    // overlapping second window finds the one at block 1. Block 1 is kept,
    // block 9 dropped and counted.
    let mut said: Vec<Said> = (2..=9)
        .map(|b| {
            one(claim_at(
                b,
                &format!("v{b}"),
                "",
                Ok(vec![anchor(b as u32, 0)]),
            ))
        })
        .collect();
    said.push(one(claim_at(1, "v1", "", Ok(vec![anchor(1, 0)]))));
    let m = merge("date", &said, false, MAX).unwrap();
    assert_eq!(m.dropped, 1);
    let c = m.value.candidates.unwrap();
    assert_eq!(
        c.iter().map(|c| c.value.as_str()).collect::<Vec<_>>(),
        (1..=8).map(|b| format!("v{b}")).collect::<Vec<_>>()
    );
    // A value without anchors stands where its window starts.
    let said = vec![
        one(claim_at(5, "late", "", Ok(vec![anchor(5, 0)]))),
        one(claim_at(3, "early", "", Err("x"))),
    ];
    let c = merged(&said, false).candidates.unwrap();
    assert_eq!(
        c.iter().map(|c| c.value.as_str()).collect::<Vec<_>>(),
        ["early", "late"]
    );
}

#[test]
fn evidence_is_added_whole_while_it_fits() {
    let set = |b: u32, n: u64| Ok((0..n).map(|i| anchor(b, i * 10)).collect());
    let v = merged(
        &[
            one(claim("a", "", set(1, 10))),
            one(claim("a", "", set(2, 7))),
            one(claim("a", "", set(3, 6))),
        ],
        false,
    );
    // 10, then 7 would make 17: skipped whole; then 6 fits.
    let blocks: Vec<String> = v
        .anchors
        .unwrap()
        .iter()
        .map(|a| a.block_id.clone())
        .collect();
    assert_eq!(
        (
            blocks.len(),
            blocks.iter().filter(|b| *b == "p1/b2").count()
        ),
        (16, 0)
    );
    // Codex R1: a set repeating an anchor counts it once. 14 kept, then
    // [kept A, B, B, C] is two new: 16, and fits.
    let fourteen: Vec<Anchor> = (0..14).map(|i| anchor(1, i * 10)).collect();
    let next = vec![anchor(1, 0), anchor(2, 0), anchor(2, 0), anchor(3, 0)];
    let v = merged(
        &[
            one(claim("a", "", Ok(fourteen))),
            one(claim("a", "", Ok(next))),
        ],
        false,
    );
    assert_eq!(v.anchors.unwrap().len(), 16);
}

#[test]
fn missing_takes_the_first_specific_reason_and_never_claims_absence_unread() {
    let m = |r: &str| Said::Missing(r.into());
    for (said, partial, want) in [
        (
            vec![m("not_in_document"), m("blank_in_template")],
            false,
            MissingReason::BlankInTemplate,
        ),
        (
            vec![m("not_in_document")],
            false,
            MissingReason::NotInDocument,
        ),
        (vec![m("not_in_document")], true, MissingReason::Unread),
        (
            vec![m("referenced_but_absent")],
            true,
            MissingReason::Unread,
        ),
        (
            vec![m("blank_in_template")],
            true,
            MissingReason::BlankInTemplate,
        ),
        (
            vec![m("outside_page_scope")],
            true,
            MissingReason::OutsidePageScope,
        ),
    ] {
        assert_eq!(
            merged(&said, partial).missing_reason,
            Some(want),
            "{said:?} {partial}"
        );
    }
    // Found in one window, missing in the others: present.
    let v = merged(
        &[
            m("not_in_document"),
            one(claim("a", "", Ok(vec![anchor(1, 0)]))),
        ],
        true,
    );
    assert_eq!(v.status, Status::Present);
}

#[test]
fn a_value_too_large_for_a_page_keeps_its_values_but_not_their_anchors() {
    let said = vec![
        one(claim_at(
            0,
            "甲",
            "",
            Ok((0..16).map(|i| anchor(1, i * 10)).collect()),
        )),
        one(claim_at(1, "乙", "", Err("x"))),
    ];
    let m = merge("date", &said, false, 2048).unwrap();
    assert_eq!(m.value.check(), Ok(()));
    assert!(serde_json::to_string(&m.value).unwrap().len() <= 2048);
    let c = m.value.candidates.unwrap();
    assert_eq!(
        c.iter().map(|c| c.value.as_str()).collect::<Vec<_>>(),
        ["甲", "乙"]
    );
    assert_eq!(
        (c[0].anchors.len(), c[0].unsourced_reason.as_deref()),
        (0, Some(TOO_LARGE))
    );
    assert_eq!(
        c[1].unsourced_reason.as_deref(),
        Some("x"),
        "its own reason stands"
    );
    // Still over without anchors: an error, never a cut conflict.
    let long = vec![one(claim(&"x".repeat(4096), "", Err("x")))];
    assert!(merge("date", &long, false, 2048).is_err());
}

#[test]
fn an_answer_says_what_its_status_says() {
    let layer = TextLayer {
        v: 1,
        content_sha256: SHA.into(),
        engine: EngineRef {
            id: "e".into(),
            version: "1".into(),
        },
        config: json!({}),
        pages: 1,
        parse_status: ParseStatus::Complete,
        unparsed_regions: vec![],
        blocks: vec![Block {
            block_id: "p1/b1".into(),
            page: 1,
            text: "补领40元/证".into(),
            lines: vec![Line {
                start: 0,
                end: 15,
                rect: [0.0, 0.0, 1.0, 1.0],
            }],
        }],
    };
    let src = Source {
        document_id: DOC,
        revision: 1,
        content_sha256: SHA,
        media_type: "application/pdf",
        text_layer_sha256: SHA,
        layer: &layer,
    };
    let w = Window(vec![0]);
    let ev = |q: &str| {
        vec![Evidence {
            block_id: "p1/b1".into(),
            quote: q.into(),
        }]
    };
    let base = AnswerValue {
        key: "fee".into(),
        status: AnswerStatus::Present,
        value: "40元".into(),
        normalized: "40".into(),
        evidence: ev("补领40元"),
        missing_reason: String::new(),
        candidates: vec![],
    };
    let Said::Values(c) = said(&src, &w, &base) else {
        panic!()
    };
    assert_eq!((c.len(), c[0].anchors.as_ref().map(Vec::len)), (1, Ok(1)));
    let conflict = AnswerValue {
        status: AnswerStatus::Conflict,
        value: String::new(),
        normalized: String::new(),
        evidence: vec![],
        candidates: vec![
            AnswerCandidate {
                value: "40元".into(),
                normalized: "40".into(),
                evidence: ev("40元"),
            },
            AnswerCandidate {
                value: "20元".into(),
                normalized: "20".into(),
                evidence: ev("20元"),
            },
        ],
        ..base.clone()
    };
    let Said::Values(c) = said(&src, &w, &conflict) else {
        panic!()
    };
    assert_eq!(
        c.iter().map(|c| c.anchors.is_ok()).collect::<Vec<_>>(),
        [true, false]
    );
    let missing = AnswerValue {
        status: AnswerStatus::Missing,
        missing_reason: "blank_in_template".into(),
        ..base
    };
    assert_eq!(
        said(&src, &w, &missing),
        Said::Missing("blank_in_template".into())
    );
}
