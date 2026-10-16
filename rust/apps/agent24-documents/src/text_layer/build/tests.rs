#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::json;

use super::*;

const CONTENT: &str = "sha256:4f26fc6d4e157f4c2eb6c72b4faa687467ea3331634a21c65fc4b1162c6008ce";

fn l(text: &str, rect: Rect) -> ReadLine {
    ReadLine {
        text: text.into(),
        rect,
    }
}

fn page(n: u32, lines: Vec<ReadLine>) -> ReadPage {
    ReadPage {
        page: n,
        width: 612.0,
        height: 792.0,
        lines,
    }
}

fn read(pages: Vec<ReadPage>, unparsed: Vec<ReadRegion>) -> Read {
    Read {
        protocol: 1,
        os_version: "26.6.2".into(),
        pages,
        unparsed,
    }
}

fn build(r: Read) -> Result<TextLayer, String> {
    let engine = EngineRef {
        id: "apple-pdfkit".into(),
        version: "26.6.2".into(),
    };
    layer(CONTENT, engine, json!({ "helper": 1 }), r)
}

fn texts(layer: &TextLayer) -> Vec<(&str, &str)> {
    layer
        .blocks
        .iter()
        .map(|b| (b.block_id.as_str(), b.text.as_str()))
        .collect()
}

#[test]
fn pieces_of_one_visual_line_are_joined_and_paragraphs_grouped() {
    let lines = vec![
        // A heading twice as tall, right above the text: its own block.
        l("国务院办公厅关于2026年", [190.0, 300.0, 420.0, 346.0]),
        // Two pieces of one line, then the next line of the paragraph.
        l("一", [115.0, 351.0, 129.0, 371.0]),
        l(
            "、元旦：1月1日至3日放假调休，共3天。1月",
            [129.0, 351.0, 526.0, 371.0],
        ),
        l("4日（周日）上班。", [86.0, 380.0, 208.0, 400.0]),
        // Latin pieces with a real gap get a space; a large gap starts a block.
        // A 3 pt gap at a 13.3 pt height is a word space.
        l("Boil", [72.0, 600.0, 100.0, 613.3]),
        l("water", [103.0, 600.0, 140.0, 613.3]),
    ];
    let layer = build(read(vec![page(1, lines)], vec![])).unwrap();
    assert_eq!(
        texts(&layer),
        [
            ("p1/b1", "国务院办公厅关于2026年"),
            (
                "p1/b2",
                "一、元旦：1月1日至3日放假调休，共3天。1月\n4日（周日）上班。"
            ),
            ("p1/b3", "Boil water"),
        ]
    );
    let b = &layer.blocks[1];
    assert_eq!(b.lines[0].rect, [115.0, 351.0, 526.0, 371.0]);
    assert_eq!(
        (b.lines[0].start, b.lines[0].end, b.lines[1].end),
        (0, 60, b.text.len())
    );
    assert_eq!(layer.parse_status, ParseStatus::Complete);
}

#[test]
fn blocks_keep_within_the_caps_and_ids_restart_on_each_page() {
    // 70 lines of one paragraph: 64 + 6.
    let many = (0..70)
        .map(|i| {
            l(
                "x",
                [
                    72.0,
                    50.0 + 10.0 * f64::from(i),
                    80.0,
                    58.0 + 10.0 * f64::from(i),
                ],
            )
        })
        .collect();
    // One line longer than a block: cut at character boundaries.
    let long = "通".repeat(MAX_BLOCK_BYTES / 3 * 2 + 5);
    let layer = build(read(
        vec![
            page(1, many),
            page(2, vec![]),
            page(3, vec![l(&long, [72.0, 100.0, 500.0, 110.0])]),
        ],
        vec![],
    ))
    .unwrap();
    let ids: Vec<_> = layer
        .blocks
        .iter()
        .map(|b| (b.block_id.as_str(), b.lines.len()))
        .collect();
    assert_eq!(
        ids,
        [
            ("p1/b1", 64),
            ("p1/b2", 6),
            ("p3/b1", 1),
            ("p3/b2", 1),
            ("p3/b3", 1)
        ]
    );
    assert!(layer.blocks.iter().all(|b| b.text.len() <= MAX_BLOCK_BYTES));
    let joined: String = layer.blocks[2..].iter().map(|b| b.text.as_str()).collect();
    assert_eq!(joined, long);
    assert_eq!(layer.pages, 3);
}

#[test]
fn unread_regions_make_the_layer_partial_and_stay_bounded() {
    let regions = vec![
        ReadRegion {
            page: 1,
            rects: vec![[0.0, 0.0, 10.0, 10.0]; 17],
            reason: "ocr_failed".into(),
        },
        ReadRegion {
            page: 2,
            rects: vec![[-5.0, 0.0, 9999.0, 10.0]],
            reason: "render_failed".into(),
        },
    ];
    let layer = build(read(vec![page(1, vec![]), page(2, vec![])], regions)).unwrap();
    assert_eq!(layer.parse_status, ParseStatus::Partial);
    assert_eq!(layer.unparsed_regions[0].rects, [[0.0, 0.0, 612.0, 792.0]]);
    assert_eq!(layer.unparsed_regions[1].rects, [[0.0, 0.0, 612.0, 10.0]]);
}

#[test]
fn a_report_that_is_not_a_layer_is_refused() {
    let bad: Vec<(&str, Read)> = vec![
        (
            "protocol 2",
            Read {
                protocol: 2,
                ..read(vec![page(1, vec![])], vec![])
            },
        ),
        ("no pages", read(vec![], vec![])),
        ("pages out of order", read(vec![page(2, vec![])], vec![])),
        (
            "a page without a size",
            read(
                vec![ReadPage {
                    width: 0.0,
                    ..page(1, vec![])
                }],
                vec![],
            ),
        ),
        (
            "a NaN rect",
            read(
                vec![page(1, vec![l("a", [f64::NAN, 0.0, 1.0, 1.0])])],
                vec![],
            ),
        ),
        (
            "a break in a line",
            read(vec![page(1, vec![l("a\nb", [0.0, 0.0, 1.0, 1.0])])], vec![]),
        ),
        (
            "an unknown reason",
            read(
                vec![page(1, vec![])],
                vec![ReadRegion {
                    page: 1,
                    rects: vec![],
                    reason: "/x".into(),
                }],
            ),
        ),
        (
            "a region past the pages",
            read(
                vec![page(1, vec![])],
                vec![ReadRegion {
                    page: 2,
                    rects: vec![],
                    reason: "ocr_failed".into(),
                }],
            ),
        ),
    ];
    for (why, r) in bad {
        assert!(build(r).is_err(), "{why} passed");
    }
}

#[test]
fn the_helpers_json_reads_into_a_report() {
    let json = r#"{"protocol":1,"os_version":"26.6.2","unparsed":[],
        "pages":[{"page":1,"width":612,"height":792,"lines":[{"text":"a","rect":[1,2,3,4]}]}]}"#;
    let r: Read = serde_json::from_str(json).unwrap();
    assert_eq!(texts(&build(r).unwrap()), [("p1/b1", "a")]);
    assert!(
        serde_json::from_str::<Read>(
            r#"{"protocol":1,"os_version":"x","pages":[],"unparsed":[],"x":1}"#
        )
        .is_err()
    );
}

#[test]
fn columns_stay_apart_and_a_split_selection_stays_together() {
    let lines = vec![
        // Two columns, the right one a little lower: not one paragraph.
        l("left column", [20.0, 100.0, 200.0, 110.0]),
        l("right column", [330.0, 115.0, 520.0, 125.0]),
        // Parts of one selection the helper split at a break share its rect.
        l("first part", [72.0, 300.0, 200.0, 340.0]),
        l("second part", [72.0, 300.0, 200.0, 340.0]),
    ];
    let layer = build(read(vec![page(1, lines)], vec![])).unwrap();
    assert_eq!(
        texts(&layer),
        [
            ("p1/b1", "left column"),
            ("p1/b2", "right column"),
            ("p1/b3", "first part\nsecond part")
        ]
    );
}

#[test]
fn a_break_between_parts_of_one_selection_survives_row_joining() {
    let shared = [129.0, 351.0, 526.0, 400.0];
    let lines = vec![
        // A piece joined before the split selection widens the row...
        l("一", [115.0, 351.0, 129.0, 371.0]),
        l("、元旦：1月", shared),
        // ...yet the next part of that selection is still its next line,
        // even with a piece joined after it.
        l("4日", shared),
        l("上班", [526.0, 380.0, 566.0, 400.0]),
        // Narrow parts sharing a rectangle are not joined into one word.
        l("i", [72.0, 500.0, 74.0, 520.0]),
        l("j", [72.0, 500.0, 74.0, 520.0]),
    ];
    let layer = build(read(vec![page(1, lines)], vec![])).unwrap();
    assert_eq!(
        texts(&layer),
        [("p1/b1", "一、元旦：1月\n4日上班"), ("p1/b2", "i\nj")]
    );
}

#[test]
fn text_that_fits_a_block_exactly_is_not_split() {
    let at = |i: f64| [72.0, 100.0 + 12.0 * i, 500.0, 110.0 + 12.0 * i];
    // Two lines and the break between them: exactly the cap.
    let half = MAX_BLOCK_BYTES / 2;
    let two = vec![
        l(&"a".repeat(half), at(0.0)),
        l(&"b".repeat(half - 1), at(1.0)),
    ];
    // One line of exactly the cap, and one that ends just past a cut.
    let one = vec![l(&"c".repeat(MAX_BLOCK_BYTES), at(0.0))];
    let stop = format!("{}STOP", "x".repeat(MAX_BLOCK_BYTES - 4));
    let layer = build(read(
        vec![
            page(1, two),
            page(2, one),
            page(3, vec![l(&stop, at(0.0))]),
            page(4, vec![l(&"d".repeat(MAX_BLOCK_BYTES + 5), at(0.0))]),
        ],
        vec![],
    ))
    .unwrap();
    let sizes: Vec<_> = layer
        .blocks
        .iter()
        .map(|b| (b.block_id.as_str(), b.text.len()))
        .collect();
    assert_eq!(
        sizes,
        [
            ("p1/b1", MAX_BLOCK_BYTES),
            ("p2/b1", MAX_BLOCK_BYTES),
            ("p3/b1", MAX_BLOCK_BYTES),
            ("p4/b1", MAX_BLOCK_BYTES),
            ("p4/b2", 5)
        ]
    );
}

/// `find`'s matching (ADR-DOC-02 §3.1), as far as these quotes need it:
/// a run of spaces in the query matches a run of whitespace in the text, a
/// break in the text may also be skipped (Chinese wraps without a space),
/// and ASCII letters match either case.
fn finds(text: &str, query: &str) -> bool {
    let (t, q): (Vec<char>, Vec<char>) = (text.chars().collect(), query.trim().chars().collect());
    (0..t.len()).any(|start| {
        let (mut i, mut j) = (start, 0);
        while j < q.len() {
            if q[j].is_whitespace() {
                if i >= t.len() || !t[i].is_whitespace() {
                    return false;
                }
                while i < t.len() && t[i].is_whitespace() {
                    i += 1;
                }
                while j < q.len() && q[j].is_whitespace() {
                    j += 1;
                }
                continue;
            }
            if i < t.len() && t[i] == '\n' && j > 0 {
                i += 1;
            }
            if i >= t.len() || !t[i].eq_ignore_ascii_case(&q[j]) {
                return false;
            }
            i += 1;
            j += 1;
        }
        true
    })
}

/// What the helper read from two S01 samples (macOS 26.6.2), built into
/// layers: every gold quote is found, as `find` matches (block by block),
/// in a block on the page the gold anchor names.
#[test]
fn real_helper_output_keeps_every_gold_quote_findable_on_its_page() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for sample in ["s01-02-en-epa-boil-water", "s01-03-zh-holiday-2026"] {
        let fixture = root.join(format!("tests/fixtures/pdfkit/{sample}.json"));
        let read: Read = serde_json::from_str(&std::fs::read_to_string(fixture).unwrap()).unwrap();
        let layer = build(read).unwrap();
        let gold = root.join(format!(
            "../../../docs/documenting/samples/s01/{sample}/gold.json"
        ));
        let gold: Value = serde_json::from_str(&std::fs::read_to_string(gold).unwrap()).unwrap();
        let mut anchors = Vec::new();
        collect_anchors(&gold, &mut anchors);
        assert!(!anchors.is_empty(), "{sample}");
        for (page, quote) in anchors {
            assert!(
                layer
                    .blocks
                    .iter()
                    .any(|b| b.page == page && finds(&b.text, &quote)),
                "{sample} p{page}: {quote}"
            );
        }
    }
}

#[test]
fn the_test_matcher_follows_the_rules() {
    assert!(finds("端午\n节", "端午节"));
    assert!(finds("Boil\nwater", "boil water"));
    assert!(!finds("Boilwater", "Boil water"));
    assert!(!finds("Boil water", "Boilwater"));
}

/// (page, quote) of every gold anchor.
fn collect_anchors(v: &Value, out: &mut Vec<(u32, String)>) {
    match v {
        Value::Object(m) => {
            if let (Some(Value::String(q)), Some(p)) =
                (m.get("quote"), m.get("page").and_then(Value::as_u64))
            {
                out.push((u32::try_from(p).unwrap(), q.clone()));
            }
            m.values().for_each(|v| collect_anchors(v, out));
        }
        Value::Array(a) => a.iter().for_each(|v| collect_anchors(v, out)),
        _ => {}
    }
}
