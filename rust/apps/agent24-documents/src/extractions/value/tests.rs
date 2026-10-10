#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::json;

use super::*;

const DOC: &str = "doc_01K74Z3QJ8V5N2W9RTX6YB4M00";
const SHA: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";

fn anchor() -> Anchor {
    Anchor {
        document_id: DOC.into(),
        revision: 1,
        content_sha256: SHA.into(),
        media_type: "application/pdf".into(),
        text_layer_sha256: SHA.into(),
        engine: Engine {
            id: "apple-pdfkit".into(),
            version: "26.6.2".into(),
        },
        block_id: "p1/b3".into(),
        page: Some(1),
        block_text_sha256: SHA.into(),
        text_range: TextRange {
            unit: "utf8".into(),
            start: 0,
            end: 4,
        },
        geometry: Some(Geometry {
            frame: "CropBox".into(),
            unit: "pt".into(),
            origin: "top-left-rotated".into(),
            rects: vec![[72.0, 100.0, 300.0, 112.0]],
        }),
        quote: "通告".into(),
    }
}

fn missing() -> ExtractedValue {
    ExtractedValue {
        key: "deadline".into(),
        status: Status::Missing,
        value: None,
        normalized: None,
        anchors: None,
        unsourced_reason: None,
        missing_reason: Some(MissingReason::NotInDocument),
        candidates: None,
    }
}

fn present() -> ExtractedValue {
    ExtractedValue {
        status: Status::Present,
        value: Some("10月12日".into()),
        anchors: Some(vec![anchor()]),
        missing_reason: None,
        ..missing()
    }
}

fn candidate(v: &str) -> Candidate {
    Candidate {
        value: v.into(),
        normalized: v.into(),
        anchors: vec![anchor()],
        unsourced_reason: None,
    }
}

fn conflict(candidates: Vec<Candidate>) -> ExtractedValue {
    ExtractedValue {
        status: Status::Conflict,
        missing_reason: None,
        candidates: Some(candidates),
        ..missing()
    }
}

#[test]
fn each_status_has_its_shape() {
    let unsourced = Candidate {
        anchors: vec![],
        unsourced_reason: Some("not in the block".into()),
        ..candidate("b")
    };
    let unread = ExtractedValue {
        missing_reason: Some(MissingReason::Unread),
        ..missing()
    };
    let docx = Anchor {
        media_type: "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
            .into(),
        page: None,
        geometry: None,
        ..anchor()
    };
    for (what, v) in [
        ("present", present()),
        (
            "present, 16 anchors",
            ExtractedValue {
                anchors: Some(vec![anchor(); 16]),
                ..present()
            },
        ),
        (
            "present, unsourced",
            ExtractedValue {
                anchors: Some(vec![]),
                unsourced_reason: Some("r".into()),
                ..present()
            },
        ),
        (
            "present, flowing",
            ExtractedValue {
                anchors: Some(vec![docx]),
                ..present()
            },
        ),
        ("missing", missing()),
        ("missing, unread", unread),
        ("conflict", conflict(vec![candidate("a"), candidate("b")])),
        (
            "conflict, one unsourced",
            conflict(vec![candidate("a"), unsourced]),
        ),
        (
            "conflict, 8",
            conflict((0..8).map(|n| candidate(&n.to_string())).collect()),
        ),
    ] {
        assert_eq!(v.check(), Ok(()), "{what}");
    }
}

#[test]
fn a_value_that_breaks_the_contract_is_refused() {
    let a = anchor;
    let anchored = |x: Anchor| ExtractedValue {
        anchors: Some(vec![x]),
        ..present()
    };
    for (what, v) in [
        (
            "key",
            ExtractedValue {
                key: "Bad Key".into(),
                ..present()
            },
        ),
        (
            "long key",
            ExtractedValue {
                key: "k".repeat(65),
                ..present()
            },
        ),
        (
            "present without value",
            ExtractedValue {
                value: None,
                ..present()
            },
        ),
        (
            "present without anchors",
            ExtractedValue {
                anchors: None,
                ..present()
            },
        ),
        (
            "no anchors, no reason",
            ExtractedValue {
                anchors: Some(vec![]),
                ..present()
            },
        ),
        (
            "anchors and a reason",
            ExtractedValue {
                unsourced_reason: Some("r".into()),
                ..present()
            },
        ),
        (
            "empty reason",
            ExtractedValue {
                anchors: Some(vec![]),
                unsourced_reason: Some(String::new()),
                ..present()
            },
        ),
        (
            "17 anchors",
            ExtractedValue {
                anchors: Some(vec![anchor(); 17]),
                ..present()
            },
        ),
        (
            "present with a missing reason",
            ExtractedValue {
                missing_reason: Some(MissingReason::Unread),
                ..present()
            },
        ),
        (
            "missing without reason",
            ExtractedValue {
                missing_reason: None,
                ..missing()
            },
        ),
        (
            "missing with a value",
            ExtractedValue {
                value: Some("v".into()),
                ..missing()
            },
        ),
        (
            "missing with normalized",
            ExtractedValue {
                normalized: Some("v".into()),
                ..missing()
            },
        ),
        (
            "missing with anchors",
            ExtractedValue {
                anchors: Some(vec![]),
                ..missing()
            },
        ),
        ("one candidate", conflict(vec![candidate("a")])),
        (
            "nine candidates",
            conflict((0..9).map(|n| candidate(&n.to_string())).collect()),
        ),
        (
            "conflict with a value",
            ExtractedValue {
                value: Some("a".into()),
                ..conflict(vec![candidate("a"), candidate("b")])
            },
        ),
        (
            "candidate, no anchors, no reason",
            conflict(vec![
                candidate("a"),
                Candidate {
                    anchors: vec![],
                    ..candidate("b")
                },
            ]),
        ),
        (
            "bad document id",
            anchored(Anchor {
                document_id: "doc_x".into(),
                ..a()
            }),
        ),
        ("revision 0", anchored(Anchor { revision: 0, ..a() })),
        (
            "bad hash",
            anchored(Anchor {
                block_text_sha256: "sha256:XYZ".into(),
                ..a()
            }),
        ),
        (
            "empty quote",
            anchored(Anchor {
                quote: String::new(),
                ..a()
            }),
        ),
        (
            "utf16",
            anchored(Anchor {
                text_range: TextRange {
                    unit: "utf16".into(),
                    start: 0,
                    end: 4,
                },
                ..a()
            }),
        ),
        (
            "start after end",
            anchored(Anchor {
                text_range: TextRange {
                    unit: "utf8".into(),
                    start: 5,
                    end: 4,
                },
                ..a()
            }),
        ),
        (
            "bad media type",
            anchored(Anchor {
                media_type: "Application/PDF".into(),
                page: None,
                geometry: None,
                ..a()
            }),
        ),
        ("pdf without page", anchored(Anchor { page: None, ..a() })),
        (
            "pdf without geometry",
            anchored(Anchor {
                geometry: None,
                ..a()
            }),
        ),
        (
            "page 0",
            anchored(Anchor {
                page: Some(0),
                ..a()
            }),
        ),
        (
            "flowing with a page",
            anchored(Anchor {
                media_type: "text/plain".into(),
                geometry: None,
                ..a()
            }),
        ),
        (
            "MediaBox",
            anchored(Anchor {
                geometry: Some(Geometry {
                    frame: "MediaBox".into(),
                    ..a().geometry.unwrap()
                }),
                ..a()
            }),
        ),
        (
            "no rects",
            anchored(Anchor {
                geometry: Some(Geometry {
                    rects: vec![],
                    ..a().geometry.unwrap()
                }),
                ..a()
            }),
        ),
        (
            "negative rect",
            anchored(Anchor {
                geometry: Some(Geometry {
                    rects: vec![[-1.0, 0.0, 1.0, 1.0]],
                    ..a().geometry.unwrap()
                }),
                ..a()
            }),
        ),
        (
            "inverted rect",
            anchored(Anchor {
                geometry: Some(Geometry {
                    rects: vec![[5.0, 0.0, 1.0, 1.0]],
                    ..a().geometry.unwrap()
                }),
                ..a()
            }),
        ),
    ] {
        assert!(v.check().is_err(), "{what}");
    }
}

#[test]
fn only_declared_properties_may_not_be_null() {
    let mut v = serde_json::to_value(present()).unwrap();
    assert!(!has_null(&v));
    // Extra properties of an anchor and its parts are the contract's to allow.
    v["anchors"][0]["note"] = json!(null);
    v["anchors"][0]["engine"]["build"] = json!(null);
    v["anchors"][0]["geometry"]["extra"] = json!({ "x": null });
    assert!(!has_null(&v));
    for (path, declared) in [
        ("/value", false),
        ("/anchors/0/page", true),
        ("/anchors/0/engine/id", true),
        ("/anchors/0/text_range/start", true),
        ("/anchors/0/geometry/rects/0/2", true),
    ] {
        let mut x = serde_json::to_value(present()).unwrap();
        *x.pointer_mut(path).unwrap() = json!(null);
        assert!(has_null(&x), "{path} (declared: {declared})");
    }
    // A value and a candidate allow no others: any null there is declared.
    let mut c = serde_json::to_value(conflict(vec![candidate("a"), candidate("b")])).unwrap();
    c["candidates"][1]["x"] = json!(null);
    assert!(has_null(&c));
    let mut m = serde_json::to_value(missing()).unwrap();
    m["value"] = json!(null);
    assert!(has_null(&m));
}

#[test]
fn values_round_trip_through_json_without_nulls() {
    let v = conflict(vec![
        candidate("a"),
        Candidate {
            anchors: vec![],
            unsourced_reason: Some("r".into()),
            ..candidate("b")
        },
    ]);
    let json = serde_json::to_value(&v).unwrap();
    assert!(!has_null(&json));
    assert!(json.get("value").is_none() && json["candidates"][0].get("unsourced_reason").is_none());
    assert_eq!(serde_json::from_value::<ExtractedValue>(json).unwrap(), v);
    // A value and a candidate allow no other properties.
    let mut extra = serde_json::to_value(missing()).unwrap();
    extra["confidence"] = json!(0.9);
    assert!(serde_json::from_value::<ExtractedValue>(extra).is_err());
}
