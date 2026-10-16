#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::json;

use super::*;
use crate::engine::Layers;
use crate::events::Events;
use crate::text_layer::{EngineRef, ParseStatus, Region};

const DOC: &str = "doc_01K74Z3QJ8V5N2W9RTX6YB4M00";
const LAYER: &str = "sha256:4f26fc6d4e157f4c2eb6c72b4faa687467ea3331634a21c65fc4b1162c6008ce";

fn rev(doc: &str, revision: i64) -> Revision {
    Revision {
        document_id: doc.into(),
        revision,
        content_sha256: LAYER.into(),
        media_type: "application/pdf".into(),
    }
}

#[test]
fn a_cursor_round_trips_and_only_for_what_it_was_issued_for() {
    let at = Position {
        page: 3,
        block: 7,
        hit: 2,
    };
    let c = encode_cursor("f", &rev(DOC, 1), LAYER, at, "boil water");
    assert_eq!(
        decode_cursor("f", &rev(DOC, 1), &c, "boil water"),
        Some((LAYER.into(), at))
    );
    for (kind, r, query) in [
        ("t", rev(DOC, 1), "boil water"),
        ("f", rev("doc_01K74Z3QJ8V5N2W9RTX6YB4M01", 1), "boil water"),
        ("f", rev(DOC, 2), "boil water"),
        ("f", rev(DOC, 1), "boil"),
    ] {
        assert_eq!(
            decode_cursor(kind, &r, &c, query),
            None,
            "{kind} {} {query}",
            r.revision
        );
    }
    let hex = |s: &str| s.bytes().map(|b| format!("{b:02x}")).collect::<String>();
    for bad in [
        "zz".to_owned(),
        "7".to_owned(),
        "ab".repeat(300),
        hex(&format!("f {DOC} 1 sha256:x 3 7 2 ")),
        hex(&format!("f {DOC} 1 {LAYER} 0 7 2 ")),
        hex(&format!("f {DOC} 1 {LAYER} 3 -1 2 ")),
    ] {
        assert_eq!(decode_cursor("f", &rev(DOC, 1), &bad, ""), None, "{bad}");
    }
}

#[test]
fn coverage_reports_only_the_pages_asked_for() {
    let layer = TextLayer {
        v: 1,
        content_sha256: LAYER.into(),
        engine: EngineRef {
            id: "x".into(),
            version: "1".into(),
        },
        config: json!({}),
        pages: 5,
        parse_status: ParseStatus::Partial,
        unparsed_regions: [2, 4]
            .map(|page| Region {
                page,
                rects: vec![[0.0, 0.0, 1.0, 1.0]],
                reason: "ocr_failed".into(),
            })
            .to_vec(),
        blocks: vec![],
    };
    let (status, regions) = coverage(&layer, 1, 3);
    assert_eq!(
        (status, regions.len(), regions[0]["page"].clone()),
        (ParseStatus::Partial, 1, json!(2))
    );
    assert_eq!(regions[0]["geometry"]["box"], "CropBox");
    assert_eq!(coverage(&layer, 5, 5), (ParseStatus::Complete, vec![]));
}

#[tokio::test]
async fn a_read_needs_its_revision_an_engine_and_a_format_the_engine_reads() {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::open_serving(dir.path(), Events::default(), Layers::new(None)).await;
    let storage = state.storage().await.unwrap();
    assert_eq!(
        revision(&storage, "doc_x", 1).await.err().map(|e| e.code()),
        Some("invalid_request")
    );
    assert_eq!(
        revision(&storage, DOC, 0).await.err().map(|e| e.code()),
        Some("invalid_request")
    );
    assert_eq!(
        revision(&storage, DOC, 1).await.err().map(|e| e.code()),
        Some("not_found")
    );
    let docx = Revision {
        media_type: "application/msword".into(),
        ..rev(DOC, 1)
    };
    let err = current_layer(&state, &storage, &docx).await.err().unwrap();
    assert_eq!(err.code(), "unsupported_format");
    let err = current_layer(&state, &storage, &rev(DOC, 1))
        .await
        .err()
        .unwrap();
    assert_eq!(
        (err.code(), err.status().as_u16()),
        ("engine_unavailable", 503)
    );
    // A layer named by a cursor must have been read from this revision's bytes.
    assert!(cursor_layer(&storage, &rev(DOC, 1), LAYER).await.is_err());
}

#[test]
fn an_edited_cursor_is_refused() {
    let at = Position {
        page: 1,
        block: 0,
        hit: 0,
    };
    let c = encode_cursor("t", &rev(DOC, 1), LAYER, at, "");
    // Flip one hex digit anywhere: payload or tag.
    for i in [0, c.len() / 2, c.len() - 1] {
        let mut b = c.clone().into_bytes();
        b[i] = if b[i] == b'0' { b'1' } else { b'0' };
        let edited = String::from_utf8(b).unwrap();
        assert_eq!(decode_cursor("t", &rev(DOC, 1), &edited, ""), None, "{i}");
    }
    // The same fields, signed by someone else (no key): refused.
    let forged = encode_cursor("t", &rev(DOC, 1), LAYER, at, "");
    assert_eq!(forged, c, "same fields, same process: same cursor");
    let unsigned: String = c[..c.len() - 32].to_owned() + &"0".repeat(32);
    assert_eq!(decode_cursor("t", &rev(DOC, 1), &unsigned, ""), None);
}

#[test]
fn layer_failures_map_to_the_contract_codes() {
    use crate::engine::{EngineError, LayerFailure};
    use crate::error::StorageCause;
    let cases = [
        (LayerFailure::NoEngine, "engine_unavailable", 503),
        (LayerFailure::Busy, "engine_unavailable", 503),
        (LayerFailure::Pending, "engine_unavailable", 503),
        (
            LayerFailure::Engine(EngineError::Unsupported),
            "unsupported_format",
            422,
        ),
        (
            LayerFailure::Engine(EngineError::Failed("x".into())),
            "parse_failed",
            422,
        ),
        (
            LayerFailure::Storage(Some(StorageCause::DiskFull), "x".into()),
            "storage_unavailable",
            503,
        ),
        (LayerFailure::Storage(None, "x".into()), "internal", 500),
    ];
    for (f, code, status) in cases {
        let e = layer_error(f);
        assert_eq!((e.code(), e.status().as_u16()), (code, status));
    }
}

#[tokio::test]
async fn a_lost_layer_blob_is_storage_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::open_serving(dir.path(), Events::default(), Layers::new(None)).await;
    let storage = state.storage().await.unwrap();
    let err = load(&storage, LAYER).await.err().unwrap();
    assert_eq!(
        (err.code(), err.status().as_u16()),
        ("storage_unavailable", 503)
    );
}
