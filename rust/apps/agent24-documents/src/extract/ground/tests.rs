#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::json;

use super::*;
use crate::text_layer::{EngineRef, Line, ParseStatus};

const DOC: &str = "doc_01K74Z3QJ8V5N2W9RTX6YB4M00";
const SHA: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";

/// Two lines per block: "line one\n" and "line two".
fn block(id: &str, page: u32, first: &str, second: &str) -> Block {
    let text = format!("{first}\n{second}");
    let cut = first.len() + 1;
    Block {
        block_id: id.into(),
        page,
        lines: vec![
            Line {
                start: 0,
                end: cut,
                rect: [72.0, 100.0, 300.0, 112.0],
            },
            Line {
                start: cut,
                end: text.len(),
                rect: [72.0, 114.0, 280.0, 126.0],
            },
        ],
        text,
    }
}

fn layer() -> TextLayer {
    TextLayer {
        v: 1,
        content_sha256: SHA.into(),
        engine: EngineRef {
            id: "apple-pdfkit".into(),
            version: "26.6.2".into(),
        },
        config: json!({}),
        pages: 2,
        parse_status: ParseStatus::Complete,
        unparsed_regions: vec![],
        blocks: vec![
            block("p1/b1", 1, "停水通知：因管道检修，", "10月12日全天停水。"),
            block(
                "p1/b2",
                1,
                "到期换发20元/证，",
                "补领40元/证，补领40元/证。",
            ),
            block("p2/b1", 2, "Fee: 40 yuan", "for late renewal only."),
        ],
    }
}

fn source(layer: &TextLayer) -> Source<'_> {
    Source {
        document_id: DOC,
        revision: 1,
        content_sha256: SHA,
        media_type: "application/pdf",
        text_layer_sha256: SHA,
        layer,
    }
}

fn ev(block_id: &str, quote: &str) -> Evidence {
    Evidence {
        block_id: block_id.into(),
        quote: quote.into(),
    }
}

#[test]
fn a_quote_found_once_in_its_block_is_an_anchor_of_the_lines_it_touches() {
    let l = layer();
    let all = Window(vec![0, 1, 2]);
    let anchors = source(&l)
        .ground(
            &all,
            "10月12日",
            &[ev("p1/b1", "因管道检修，\n10月12日全天停水")],
        )
        .unwrap();
    assert_eq!(anchors.len(), 1);
    let a = &anchors[0];
    assert_eq!(
        (a.block_id.as_str(), a.page, a.quote.as_str()),
        ("p1/b1", Some(1), "因管道检修，\n10月12日全天停水")
    );
    let b = &l.blocks[0];
    assert_eq!(
        &b.text[a.text_range.start as usize..a.text_range.end as usize],
        a.quote
    );
    assert_eq!(a.geometry.as_ref().unwrap().rects.len(), 2, "both lines");
    assert_eq!(a.block_text_sha256, sha256_address(b.text.as_bytes()));
    // The anchor is the contract's, as the store checks it.
    let v = crate::extractions::ExtractedValue {
        key: "date".into(),
        status: crate::extractions::Status::Present,
        value: Some("10月12日".into()),
        normalized: None,
        anchors: Some(anchors),
        unsourced_reason: None,
        missing_reason: None,
        candidates: None,
    };
    assert_eq!(v.check(), Ok(()));
}

#[test]
fn find_rules_apply_line_breaks_case_and_spaces() {
    let l = layer();
    let all = Window(vec![0, 1, 2]);
    // The quote skips the line break the text has; ASCII case and spacing differ.
    let a = source(&l)
        .ground(&all, "40 YUAN", &[ev("p2/b1", "fee:  40 YUAN for late")])
        .unwrap();
    assert_eq!(a[0].quote, "Fee: 40 yuan\nfor late");
    let a = source(&l)
        .ground(&all, "10月12日", &[ev("p1/b1", "检修，10月12日")])
        .unwrap();
    assert_eq!(a[0].quote, "检修，\n10月12日");
}

#[test]
fn every_quote_must_hold_or_the_value_has_no_anchors() {
    let l = layer();
    let src = source(&l);
    let all = Window(vec![0, 1, 2]);
    let date = ev("p1/b1", "10月12日全天停水");
    for (what, value, evidence, why) in [
        ("none given", "10月12日", vec![], unsourced::NO_EVIDENCE),
        (
            "no such block",
            "10月12日",
            vec![ev("p9/b9", "10月12日")],
            unsourced::NOT_SHOWN,
        ),
        (
            "not in the block it names",
            "10月12日",
            vec![ev("p1/b2", "10月12日")],
            unsourced::NOT_FOUND,
        ),
        (
            "twice in its block",
            "40元",
            vec![ev("p1/b2", "补领40元/证")],
            unsourced::AMBIGUOUS,
        ),
        (
            "only spaces",
            "10月12日",
            vec![ev("p1/b1", "  ")],
            unsourced::NOT_FOUND,
        ),
        // One good quote does not carry a bad one: the condition may be the bad one.
        (
            "one of two fails",
            "10月12日",
            vec![date.clone(), ev("p1/b1", "仅限A栋")],
            unsourced::NOT_FOUND,
        ),
        (
            "value in no quote",
            "10月13日",
            vec![date.clone()],
            unsourced::VALUE_NOT_QUOTED,
        ),
    ] {
        assert_eq!(src.ground(&all, value, &evidence), Err(why), "{what}");
    }
    // A block that exists but the model was not shown is not evidence either.
    assert_eq!(
        src.ground(&Window(vec![1, 2]), "10月12日", &[date]),
        Err(unsourced::NOT_SHOWN)
    );
}

#[test]
fn overlapping_occurrences_are_ambiguous_too() {
    let mut l = layer();
    l.blocks[2].text = "banana aaa".into();
    l.blocks[2].lines = vec![Line {
        start: 0,
        end: 10,
        rect: [0.0, 0.0, 1.0, 1.0],
    }];
    let src = source(&l);
    let all = Window(vec![0, 1, 2]);
    for quote in ["ana", "aa", "AA"] {
        assert_eq!(
            src.ground(&all, quote, &[ev("p2/b1", quote)]),
            Err(unsourced::AMBIGUOUS),
            "{quote}"
        );
    }
    // Next to good evidence, it still takes all of it.
    let good = ev("p1/b1", "10月12日全天停水");
    assert_eq!(
        src.ground(&all, "10月12日", &[good.clone(), ev("p2/b1", "ana")]),
        Err(unsourced::AMBIGUOUS)
    );
    // Once is once: "banan" starts only at 0, "aaa" only at 7.
    for (quote, at) in [("banan", 0), ("aaa", 7)] {
        let a = src.ground(&all, quote, &[ev("p2/b1", quote)]).unwrap();
        assert_eq!(a[0].text_range.start, at, "{quote}");
    }
}

#[test]
fn a_longer_quote_settles_what_a_short_one_cannot() {
    let l = layer();
    let a = source(&l)
        .ground(&Window(vec![1]), "20元", &[ev("p1/b2", "到期换发20元/证")])
        .unwrap();
    assert_eq!(a[0].quote, "到期换发20元/证");
    assert_eq!(
        a[0].geometry.as_ref().unwrap().rects,
        vec![[72.0, 100.0, 300.0, 112.0]],
        "one line"
    );
}
