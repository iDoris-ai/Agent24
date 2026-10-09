#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::json;

use super::*;
use crate::state::AppState;

const CONTENT: &str = "sha256:4f26fc6d4e157f4c2eb6c72b4faa687467ea3331634a21c65fc4b1162c6008ce";

fn line(start: usize, end: usize) -> Line {
    Line {
        start,
        end,
        rect: [60.5, 612.0, 512.0, 626.5],
    }
}

/// Two blocks on page 1, one on page 3; page 2 not read.
fn layer() -> TextLayer {
    TextLayer {
        v: 1,
        content_sha256: CONTENT.into(),
        engine: EngineRef {
            id: "apple-pdfkit".into(),
            version: "26.6".into(),
        },
        config: json!({ "helper": 1, "blocks": "lines-v1" }),
        pages: 4,
        parse_status: ParseStatus::Partial,
        unparsed_regions: vec![Region {
            page: 2,
            rects: vec![[0.0, 0.0, 595.0, 842.0]],
            reason: "ocr_failed".into(),
        }],
        blocks: vec![
            Block {
                block_id: "p1/b1".into(),
                page: 1,
                text: "第一行\n第二行".into(),
                lines: vec![line(0, 10), line(10, 19)],
            },
            Block {
                block_id: "p1/b2".into(),
                page: 1,
                text: "Total: 42".into(),
                lines: vec![line(0, 9)],
            },
            Block {
                block_id: "p3/b1".into(),
                page: 3,
                text: "end".into(),
                lines: vec![line(0, 3)],
            },
        ],
    }
}

#[test]
fn a_well_formed_layer_passes_and_each_rule_is_checked() {
    assert_eq!(layer().check(), Ok(()));
    type Break = Box<dyn Fn(&mut TextLayer)>;
    let broken: Vec<(&str, Break)> = vec![
        ("v2", Box::new(|l| l.v = 2)),
        ("config not an object", Box::new(|l| l.config = json!([1]))),
        (
            "a float in config",
            Box::new(|l| l.config = json!({ "helper": 1.0 })),
        ),
        (
            "nesting in config",
            Box::new(|l| l.config = json!({ "helper": { "v": 1 } })),
        ),
        (
            "no source content",
            Box::new(|l| l.content_sha256 = "sha256:x".into()),
        ),
        ("no pages", Box::new(|l| l.pages = 0)),
        ("a block past the last page", Box::new(|l| l.pages = 2)),
        (
            "a region past the last page",
            Box::new(|l| {
                l.pages = 3;
                l.blocks.pop();
                l.unparsed_regions[0].page = 4
            }),
        ),
        (
            "a line break owned by the next line",
            Box::new(|l| {
                l.blocks[0].lines[0].end = 9;
                l.blocks[0].lines[1].start = 9
            }),
        ),
        (
            "partial without regions",
            Box::new(|l| l.unparsed_regions.clear()),
        ),
        (
            "complete with regions",
            Box::new(|l| l.parse_status = ParseStatus::Complete),
        ),
        (
            "two regions on a page",
            Box::new(|l| l.unparsed_regions.push(l.unparsed_regions[0].clone())),
        ),
        (
            "17 rects",
            Box::new(|l| l.unparsed_regions[0].rects = vec![[0.0, 0.0, 1.0, 1.0]; 17]),
        ),
        (
            "unknown reason",
            Box::new(|l| l.unparsed_regions[0].reason = "/Users/x/a.pdf".into()),
        ),
        (
            "ids out of order",
            Box::new(|l| l.blocks[1].block_id = "p1/b3".into()),
        ),
        ("pages out of order", Box::new(|l| l.blocks.swap(1, 2))),
        (
            "page 0",
            Box::new(|l| {
                l.blocks[0].page = 0;
                l.blocks[0].block_id = "p0/b1".into()
            }),
        ),
        (
            "empty text",
            Box::new(|l| {
                l.blocks[2].text.clear();
                l.blocks[2].lines.clear()
            }),
        ),
        (
            "text over the cap",
            Box::new(|l| {
                l.blocks[2].text = "a".repeat(MAX_BLOCK_BYTES + 1);
                l.blocks[2].lines = vec![line(0, MAX_BLOCK_BYTES + 1)];
            }),
        ),
        (
            "65 lines",
            Box::new(|l| {
                l.blocks[2].text = "a".repeat(65);
                l.blocks[2].lines = (0..65).map(|i| line(i, i + 1)).collect();
            }),
        ),
        (
            "a gap between lines",
            Box::new(|l| l.blocks[0].lines[1].start = 11),
        ),
        (
            "lines short of the text",
            Box::new(|l| l.blocks[0].lines[1].end = 16),
        ),
        (
            "a line inside a character",
            Box::new(|l| {
                l.blocks[0].lines[0].end = 2;
                l.blocks[0].lines[1].start = 2
            }),
        ),
        (
            "a NaN rect",
            Box::new(|l| l.blocks[1].lines[0].rect[0] = f64::NAN),
        ),
        (
            "an inverted rect",
            Box::new(|l| l.blocks[1].lines[0].rect = [10.0, 0.0, 5.0, 1.0]),
        ),
    ];
    for (why, change) in broken {
        let mut l = layer();
        change(&mut l);
        assert!(l.check().is_err(), "{why} passed");
    }
}

#[test]
fn a_long_line_split_without_a_break_and_trailing_blank_pages_pass() {
    let mut l = layer();
    l.blocks[2].text = "endless".into();
    l.blocks[2].lines = vec![line(0, 3), line(3, 7)];
    l.pages = 9;
    assert_eq!(l.check(), Ok(()));
}

#[tokio::test]
async fn two_contents_that_read_the_same_get_layers_of_their_own() {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::open(dir.path()).await;
    let storage = state.storage().await.unwrap();
    let other = "sha256:5f26fc6d4e157f4c2eb6c72b4faa687467ea3331634a21c65fc4b1162c6008ce";
    let mut same = layer();
    same.content_sha256 = other.into();
    let a = pin(&storage, CONTENT, &layer()).await.unwrap();
    let b = pin(&storage, other, &same).await.unwrap();
    assert_ne!(a, b);
    assert_eq!(
        pinned(&storage, other, &same.engine, &same.config_sha256())
            .await
            .unwrap(),
        Some(b)
    );
    // A layer is pinned only for the content it was read from.
    assert!(matches!(
        pin(&storage, other, &layer()).await,
        Err(LayerError::Invalid(_))
    ));
}

#[tokio::test]
async fn a_layer_is_pinned_once_per_key_and_reads_back_as_stored() {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::open(dir.path()).await;
    let storage = state.storage().await.unwrap();
    let first = pin(&storage, CONTENT, &layer()).await.unwrap();
    assert_eq!(load(&storage, &first).await.unwrap(), layer());
    // The same key again, even with different blocks: the first one stays.
    let mut other = layer();
    other.blocks.truncate(1);
    assert_eq!(pin(&storage, CONTENT, &other).await.unwrap(), first);
    // Another config is another layer.
    other.config = json!({ "helper": 2 });
    let second = pin(&storage, CONTENT, &other).await.unwrap();
    assert_ne!(second, first);
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM text_layers")
        .fetch_one(storage.db.pool())
        .await
        .unwrap();
    assert_eq!(rows, 2);
    assert_eq!(
        pinned(&storage, CONTENT, &layer().engine, &layer().config_sha256())
            .await
            .unwrap(),
        Some(first)
    );
}

#[tokio::test]
async fn a_broken_layer_is_neither_stored_nor_read() {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::open(dir.path()).await;
    let storage = state.storage().await.unwrap();
    let mut bad = layer();
    bad.v = 2;
    assert!(matches!(
        pin(&storage, CONTENT, &bad).await,
        Err(LayerError::Invalid(_))
    ));
    // A blob that is not a valid layer does not load.
    let bytes = serde_json::to_vec(&bad).unwrap();
    let blob = storage.blobs.put_bytes(&bytes).unwrap();
    assert!(matches!(
        load(&storage, &blob.sha256).await,
        Err(LayerError::Invalid(_))
    ));
    let mut extra = serde_json::to_value(layer()).unwrap();
    extra["title"] = json!("通告");
    let blob = storage
        .blobs
        .put_bytes(extra.to_string().as_bytes())
        .unwrap();
    assert!(matches!(
        load(&storage, &blob.sha256).await,
        Err(LayerError::Invalid(_))
    ));
}

#[test]
fn the_config_hash_does_not_depend_on_key_order() {
    let (mut a, mut b) = (layer(), layer());
    a.config = serde_json::from_str(r#"{"helper":1,"blocks":"lines-v1"}"#).unwrap();
    b.config = serde_json::from_str(r#"{"blocks":"lines-v1","helper":1}"#).unwrap();
    assert_eq!(a.config_sha256(), b.config_sha256());
}

#[test]
fn lines_out_of_range_are_refused_never_a_panic() {
    for (start, end) in [(0, 99), (0, 0), (5, 3), (0, usize::MAX)] {
        let mut l = layer();
        l.blocks[2].lines = vec![line(start, end)];
        assert!(l.check().is_err(), "{start}..{end}");
    }
}
