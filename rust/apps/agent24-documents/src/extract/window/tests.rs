#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::json;

use super::*;
use crate::text_layer::{Block, EngineRef, Line, ParseStatus};

/// A layer whose page `p` holds blocks of the given sizes.
fn layer(pages: &[&[usize]]) -> TextLayer {
    let mut blocks = Vec::new();
    for (p, sizes) in pages.iter().enumerate() {
        for (n, &size) in sizes.iter().enumerate() {
            blocks.push(Block {
                block_id: format!("p{}/b{}", p + 1, n + 1),
                page: u32::try_from(p + 1).unwrap(),
                text: "x".repeat(size),
                lines: vec![Line {
                    start: 0,
                    end: size,
                    rect: [0.0, 0.0, 1.0, 1.0],
                }],
            });
        }
    }
    TextLayer {
        v: 1,
        content_sha256: format!("sha256:{}", "0".repeat(64)),
        engine: EngineRef {
            id: "e".into(),
            version: "1".into(),
        },
        config: json!({}),
        pages: u32::try_from(pages.len()).unwrap(),
        parse_status: ParseStatus::Complete,
        unparsed_regions: vec![],
        blocks,
    }
}

fn ids(l: &TextLayer, ws: &[Window]) -> Vec<Vec<String>> {
    ws.iter()
        .map(|w| w.0.iter().map(|&i| l.blocks[i].block_id.clone()).collect())
        .collect()
}

/// What blocks `of` cost as sent.
fn cost(l: &TextLayer, of: &[usize]) -> usize {
    Window(of.to_vec()).bytes(l)
}

#[test]
fn a_layer_within_the_budget_is_one_window() {
    let l = layer(&[&[10, 20], &[30]]);
    assert_eq!(
        windows(&l, cost(&l, &[0, 1, 2])),
        vec![Window(vec![0, 1, 2])]
    );
    assert_eq!(windows(&layer(&[]), 60), vec![Window(vec![])]);
    // A block counts as sent: its id and page too, not only its text.
    assert!(cost(&l, &[0]) > 10 + "p1/b1".len());
}

#[test]
fn a_window_starting_a_page_carries_the_end_of_the_page_before() {
    // Codex R1's case, a page of 7 KiB + 16 KiB, then one of 4 KiB: the
    // second window has room for the 16 KiB block before it, and carries it.
    let l = layer(&[&[7 * 1024, 16 * 1024], &[4 * 1024]]);
    let ws = windows(&l, BUDGET);
    assert_eq!(
        ids(&l, &ws),
        [vec!["p1/b1", "p1/b2"], vec!["p1/b2", "p2/b1"]]
    );
    // Only the last blocks that fit the room left come along.
    let l = layer(&[&[200, 200, 200], &[600]]);
    let budget = cost(&l, &[3]) + cost(&l, &[2]) + 10;
    assert_eq!(
        ids(&l, &windows(&l, budget)),
        [vec!["p1/b1", "p1/b2", "p1/b3"], vec!["p1/b3", "p2/b1"]]
    );
    // No room at all: nothing carried.
    let l = layer(&[&[500], &[1000]]);
    let budget = cost(&l, &[1]);
    assert_eq!(
        ids(&l, &windows(&l, budget)),
        [vec!["p1/b1"], vec!["p2/b1"]]
    );
}

#[test]
fn a_page_over_the_budget_is_cut_by_blocks_and_only_its_first_part_carries() {
    let l = layer(&[&[600, 100], &[600, 600, 600], &[100]]);
    let budget = cost(&l, &[0]) + cost(&l, &[1]) + 10;
    assert_eq!(
        ids(&l, &windows(&l, budget)),
        [
            vec!["p1/b1", "p1/b2"],
            vec!["p1/b2", "p2/b1"],
            vec!["p2/b2"],
            vec!["p2/b3", "p3/b1"]
        ]
    );
}

#[test]
fn many_small_blocks_still_fit_the_request() {
    // Codex R1: 100 pages of 64 one-byte blocks are 6,400 bytes of text but
    // far more as sent; windows hold them to the budget.
    let pages = vec![vec![1; 64]; 100];
    let pages: Vec<&[usize]> = pages.iter().map(Vec::as_slice).collect();
    let l = layer(&pages);
    let ws = windows(&l, BUDGET);
    assert!(ws.len() > 1);
    assert!(ws.iter().all(|w| w.bytes(&l) <= BUDGET));
}

#[test]
fn a_block_over_the_budget_goes_alone_and_the_windows_move_on() {
    let mut l = layer(&[&[10], &[10], &[10]]);
    // Escaped six times over as JSON.
    l.blocks[1].text = "\u{1}".repeat(16 * 1024);
    let ws = windows(&l, BUDGET);
    assert_eq!(ids(&l, &ws), [vec!["p1/b1"], vec!["p2/b1"], vec!["p3/b1"]]);
}

#[test]
fn every_block_is_read_and_windows_follow_the_text() {
    let sizes: Vec<Vec<usize>> = (0..30)
        .map(|p| {
            (0..(p % 4 + 1))
                .map(|b| 500 + 977 * (p + b) % 9000)
                .collect()
        })
        .collect();
    let pages: Vec<&[usize]> = sizes.iter().map(Vec::as_slice).collect();
    let l = layer(&pages);
    let ws = windows(&l, BUDGET);
    assert!(ws.len() > 1);
    assert!(
        ws.iter()
            .all(|w| w.bytes(&l) <= BUDGET && w.0.windows(2).all(|p| p[0] < p[1]))
    );
    let mut seen: Vec<usize> = ws.iter().flat_map(|w| w.0.clone()).collect();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen, (0..l.blocks.len()).collect::<Vec<_>>());
}

#[test]
fn halves_and_batches() {
    assert_eq!(
        Window(vec![1, 2, 3]).halves(),
        Some((Window(vec![1]), Window(vec![2, 3])))
    );
    assert_eq!(Window(vec![4]).halves(), None);
    let fields: Vec<u32> = (0..45).collect();
    let b = batches(&fields, BATCH);
    assert_eq!(b.iter().map(Vec::len).collect::<Vec<_>>(), [20, 20, 5]);
    assert_eq!(b.concat(), fields);
}
