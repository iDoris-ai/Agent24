#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Cursor, Read};

use zip::ZipArchive;

use super::*;

const FIXTURE: &[u8] = include_bytes!("../../tests/fixtures/docx-patch.docx");
const NOW: &str = "2026-11-19T12:00:00Z";

/// Each entry: name, CRC and its bytes as stored (compressed).
fn raw_entries(docx: &[u8]) -> Vec<(String, u32, Vec<u8>)> {
    let mut archive = ZipArchive::new(Cursor::new(docx)).unwrap();
    (0..archive.len())
        .map(|i| {
            let mut file = archive.by_index_raw(i).unwrap();
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).unwrap();
            (file.name().to_owned(), file.crc32(), bytes)
        })
        .collect()
}

fn entry(docx: &[u8], name: &str) -> String {
    let mut archive = ZipArchive::new(Cursor::new(docx)).unwrap();
    let mut s = String::new();
    archive
        .by_name(name)
        .unwrap()
        .read_to_string(&mut s)
        .unwrap();
    s
}

fn index_of(paras: &[ParaInfo], prefix: &str) -> usize {
    paras
        .iter()
        .find(|p| p.text.starts_with(prefix))
        .unwrap()
        .index
}

#[test]
fn lists_the_body_paragraphs_with_their_text_and_tables() {
    let paras = list_paragraphs(FIXTURE).unwrap();
    assert!(
        paras
            .iter()
            .any(|p| p.text == "房屋租赁合同" && !p.in_table)
    );
    let deposit = &paras[index_of(&paras, "押金为人民币 17,000 元")];
    assert!(!deposit.in_table);
    let utilities = paras.iter().find(|p| p.text == "水电燃气").unwrap();
    assert!(utilities.in_table);
    assert!(paras.windows(2).all(|w| w[0].index + 1 == w[1].index));
}

#[test]
fn edits_two_paragraphs_and_leaves_every_other_byte_as_it_was() {
    let paras = list_paragraphs(FIXTURE).unwrap();
    let plain = index_of(&paras, "押金为人民币");
    let tracked = index_of(&paras, "任何一方提前解除合同");
    let edits = [
        ParaEdit {
            index: plain,
            text: "押金为人民币 17,000 元；甲方应在 10 个工作日内退还 <全部> 押金。".into(),
            tracked: false,
            expect: Some(paras[plain].text.clone()),
        },
        ParaEdit {
            index: tracked,
            text: "任何一方提前解除合同，应提前 60 日书面通知对方。".into(),
            tracked: true,
            expect: None,
        },
    ];
    let out = apply(FIXTURE, &edits, "Agent24", NOW).unwrap();

    // Every entry but these two: the very bytes stored, in the same order.
    let before = raw_entries(FIXTURE);
    let after = raw_entries(&out);
    assert_eq!(
        before.iter().map(|e| &e.0).collect::<Vec<_>>(),
        after.iter().map(|e| &e.0).collect::<Vec<_>>()
    );
    for (b, a) in before.iter().zip(&after) {
        if b.0 == "word/document.xml" || b.0 == "docProps/core.xml" {
            continue;
        }
        assert_eq!(b, a, "{} changed", b.0);
    }

    // In document.xml only the two paragraphs changed.
    let old = entry(FIXTURE, "word/document.xml");
    let new = entry(&out, "word/document.xml");
    let old_paras = scan_paragraphs(&old).unwrap();
    let new_paras = scan_paragraphs(&new).unwrap();
    assert_eq!(old_paras.len(), new_paras.len());
    for (i, (o, n)) in old_paras.iter().zip(&new_paras).enumerate() {
        let (o, n) = (&old[o.start..o.end], &new[n.start..n.end]);
        if i == plain || i == tracked {
            assert_ne!(o, n);
        } else {
            assert_eq!(o, n, "paragraph {i} changed");
        }
    }
    let outside = |xml: &str, ps: &[Para]| {
        let mut s = String::new();
        let mut at = 0;
        for p in ps {
            s.push_str(&xml[at..p.start]);
            at = p.end;
        }
        s.push_str(&xml[at..]);
        s
    };
    assert_eq!(outside(&old, &old_paras), outside(&new, &new_paras));

    // The plain edit: the new text, the paragraph's own properties kept.
    let listed = list_paragraphs(&out).unwrap();
    assert_eq!(listed[plain].text, edits[0].text);
    let p = &new[new_paras[plain].start..new_paras[plain].end];
    assert!(p.contains("&lt;全部&gt;"));
    if let Some(at) = old[old_paras[plain].start..old_paras[plain].end].find("<w:pPr") {
        let o = &old[old_paras[plain].start..old_paras[plain].end];
        let ppr = &o[at..o.find("</w:pPr>").unwrap() + 8];
        assert!(p.contains(ppr));
    }

    // The tracked edit: the old text deleted, the new inserted, and the
    // comment on that paragraph still anchored there.
    let t = &new[new_paras[tracked].start..new_paras[tracked].end];
    assert!(t.contains("<w:del w:id=\""), "{t}");
    assert!(t.contains("<w:ins w:id=\""), "{t}");
    assert!(t.contains("w:author=\"Agent24\""));
    assert!(t.contains(&format!(
        "<w:delText xml:space=\"preserve\">{}</w:delText>",
        paras[tracked].text
    )));
    for marker in [
        "w:commentRangeStart",
        "w:commentRangeEnd",
        "w:commentReference",
    ] {
        assert!(t.contains(marker), "{marker} lost: {t}");
    }
    // The inserted text is the paragraph's text; the deleted one is not.
    assert_eq!(listed[tracked].text, edits[1].text);

    // The modified time, when core.xml has one.
    let core = entry(&out, "docProps/core.xml");
    if entry(FIXTURE, "docProps/core.xml").contains("<dcterms:modified") {
        assert!(
            core.contains(&format!(">{NOW}</dcterms:modified>")),
            "{core}"
        );
    }
}

#[test]
fn a_changed_paragraph_an_index_past_the_end_and_a_non_zip_are_refused() {
    let paras = list_paragraphs(FIXTURE).unwrap();
    let edit = |index, expect: Option<&str>| ParaEdit {
        index,
        text: "x".into(),
        tracked: false,
        expect: expect.map(str::to_owned),
    };
    assert_eq!(
        apply(FIXTURE, &[edit(0, Some("not this text"))], "a", NOW),
        Err(PatchError::Stale(0))
    );
    assert_eq!(
        apply(FIXTURE, &[edit(paras.len(), None)], "a", NOW),
        Err(PatchError::IndexOutOfRange(paras.len()))
    );
    assert!(matches!(
        apply(FIXTURE, &[edit(0, None), edit(0, None)], "a", NOW),
        Err(PatchError::Invalid(_))
    ));
    assert!(matches!(
        apply(FIXTURE, &[], "a", NOW),
        Err(PatchError::Invalid(_))
    ));
    assert!(matches!(
        list_paragraphs(b"this is not a zip"),
        Err(PatchError::NotDocx(_))
    ));
}

#[test]
fn the_scanner_reads_entities_empty_paragraphs_and_nested_text_boxes() {
    let xml = "<w:document><w:body><w:p/><w:p><w:r><w:t>A &amp; B &#x4E2D;</w:t></w:r>\
               <w:r><w:pict><w:txbxContent><w:p><w:r><w:t>box</w:t></w:r></w:p></w:txbxContent></w:pict></w:r></w:p>\
               <w:tbl><w:tr><w:tc><w:p><w:r><w:t>cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl></w:body></w:document>";
    let paras = scan_paragraphs(xml).unwrap();
    assert_eq!(paras.len(), 3);
    assert_eq!(paragraph_text(&xml[paras[0].start..paras[0].end]), "");
    assert_eq!(
        paragraph_text(&xml[paras[1].start..paras[1].end]),
        "A & B 中"
    );
    assert!(paras[2].in_table && !paras[1].in_table);
    assert_eq!(max_revision_id("<a w:id=\"7\"/><b w:id=\"12\"/>"), 12);
}

// ── D5a: template fields ──

const TEMPLATE: &[u8] = include_bytes!("../../tests/fixtures/template-lease.docx");

fn values(pairs: &[(&str, &str)]) -> std::collections::BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

#[test]
fn lists_the_fields_of_a_template_also_split_across_runs_tables_and_headers() {
    let names: Vec<String> = list_fields(TEMPLATE)
        .unwrap()
        .into_iter()
        .map(|f| f.name)
        .collect();
    for name in [
        "合同编号",
        "出租方",
        "承租方",
        "房屋地址",
        "面积",
        "月租金",
        "押金",
        "物业费",
        "签订日期",
    ] {
        assert!(
            names.contains(&name.to_owned()),
            "{name} missing from {names:?}"
        );
    }
    // The body's first, then headers.
    assert_eq!(names[0], "出租方");
}

#[test]
fn fills_fields_where_they_stand_keeping_bold_and_underline() {
    let filled = fill(
        TEMPLATE,
        &values(&[
            ("合同编号", "MY-2026-0137"),
            ("出租方", "上海明远置业有限公司"),
            ("房屋地址", "上海市普陀区真北路 1288 号"),
            ("月租金", "8,500"),
            ("押金", "17,000"),
            ("签订日期", "2025 年 12 月 20 日"),
        ]),
        NOW,
    )
    .unwrap();
    let doc = entry(&filled, "word/document.xml");
    let old = entry(TEMPLATE, "word/document.xml");
    // Split across runs: the value in the first char's run, the others emptied of it.
    let paras = scan_paragraphs(&doc).unwrap();
    let text = |prefix: &str| {
        paras
            .iter()
            .map(|p| paragraph_text(&doc[p.start..p.end]))
            .find(|t| t.starts_with(prefix))
            .unwrap()
    };
    assert_eq!(
        text("出租方（甲方）："),
        "出租方（甲方）：上海明远置业有限公司"
    );
    assert_eq!(
        text("第一条"),
        "第一条 租赁房屋位于 上海市普陀区真北路 1288 号，建筑面积 {{面积}} 平方米。"
    );
    assert_eq!(text("签订日期："), "签订日期：2025 年 12 月 20 日");
    // The bold label run and the underlined value run are still there, as they were.
    let lessor = paras
        .iter()
        .map(|p| &doc[p.start..p.end])
        .find(|p| p.contains("上海明远置业"))
        .unwrap();
    assert!(lessor.contains("<w:b/>"), "{lessor}");
    assert!(lessor.contains("<w:u w:val=\"single\"/>"), "{lessor}");
    // In a table cell too, and a field with no value stays.
    assert!(doc.contains("17,000"));
    assert!(doc.contains("{{物业费}}") || doc.contains("物业费}}"));
    // Paragraphs without a field: the very bytes they were.
    let old_paras = scan_paragraphs(&old).unwrap();
    for (o, n) in old_paras.iter().zip(&paras) {
        let (o, n) = (&old[o.start..o.end], &doc[n.start..n.end]);
        if !o.contains("{{") && !o.contains("}}") {
            assert_eq!(o, n);
        }
    }
    // The header's field, and every entry but the parts filled kept as stored.
    let header = (1..=3)
        .map(|i| format!("word/header{i}.xml"))
        .find(|n| raw_entries(&filled).iter().any(|e| &e.0 == n))
        .unwrap();
    assert!(entry(&filled, &header).contains("MY-2026-0137"));
    let (before, after) = (raw_entries(TEMPLATE), raw_entries(&filled));
    for (b, a) in before.iter().zip(&after) {
        if b.0 == "word/document.xml" || b.0 == header || b.0 == "docProps/core.xml" {
            continue;
        }
        assert_eq!(b, a, "{} changed", b.0);
    }
}

#[test]
fn a_field_the_template_does_not_have_and_no_values_are_refused() {
    assert!(matches!(
        fill(TEMPLATE, &values(&[("不存在", "x")]), NOW),
        Err(PatchError::Invalid(_))
    ));
    assert!(matches!(
        fill(TEMPLATE, &values(&[]), NOW),
        Err(PatchError::Invalid(_))
    ));
    assert_eq!(
        placeholders("a{{ x }}b{{}}c{{y}}"),
        vec![(1, 8, "x".into()), (14, 19, "y".into())]
    );
}
