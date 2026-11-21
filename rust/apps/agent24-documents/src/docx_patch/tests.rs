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
    // The deleted runs are the original ones: rejecting gives back the text.
    let del = &t[t.find("<w:del ").unwrap()..t.find("</w:del>").unwrap()];
    assert_eq!(
        paragraph_text(&del.replace("w:delText", "w:t")),
        paras[tracked].text
    );
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

// ── PR-Daemon #1930 B1/B2 ──

/// The template with `extra` entries added (deflated).
fn with_entries(extra: &[(String, Vec<u8>)]) -> Vec<u8> {
    use std::io::Write;
    let mut archive = ZipArchive::new(Cursor::new(TEMPLATE)).unwrap();
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for i in 0..archive.len() {
        writer
            .raw_copy_file(archive.by_index_raw(i).unwrap())
            .unwrap();
    }
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, bytes) in extra {
        writer.start_file(name.as_str(), options).unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

/// Sets the uncompressed size the zip declares for `name` (local header and
/// central directory) to `size`, whatever the entry really holds.
fn declare_size(zip: &mut [u8], name: &str, size: u32) {
    let name = name.as_bytes();
    let mut i = 0;
    while i + 46 < zip.len() {
        let (sig, at, name_len_at, name_at) = match &zip[i..i + 4] {
            b"PK\x03\x04" => (true, i + 22, i + 26, i + 30),
            b"PK\x01\x02" => (true, i + 24, i + 28, i + 46),
            _ => (false, 0, 0, 0),
        };
        if sig {
            let len = u16::from_le_bytes([zip[name_len_at], zip[name_len_at + 1]]) as usize;
            if zip.get(name_at..name_at + len) == Some(name) {
                zip[at..at + 4].copy_from_slice(&size.to_le_bytes());
            }
        }
        i += 1;
    }
}

fn header(text_len: usize) -> Vec<u8> {
    let mut xml = String::from(
        r#"<?xml version="1.0"?><w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:p><w:r><w:t>"#,
    );
    xml.push_str(&" ".repeat(text_len));
    xml.push_str("</w:t></w:r></w:p></w:hdr>");
    xml.into_bytes()
}

#[test]
fn a_header_larger_than_its_zip_entry_says_is_read_only_up_to_the_limit() {
    let big = (MAX_DOCUMENT_XML_BYTES + 1024) as usize;
    let mut docx = with_entries(&[("word/header9.xml".into(), header(big))]);
    declare_size(&mut docx, "word/header9.xml", 100);
    assert!(matches!(list_fields(&docx), Err(PatchError::TooLarge)));
}

#[test]
fn many_headers_or_too_many_bytes_of_them_are_refused() {
    let many: Vec<(String, Vec<u8>)> = (0..MAX_TEXT_PARTS)
        .map(|i| (format!("word/header{}.xml", 100 + i), header(10)))
        .collect();
    assert!(matches!(
        list_fields(&with_entries(&many)),
        Err(PatchError::TooLarge)
    ));
    // Each under the per-part limit, together over the budget.
    let each = (MAX_TEXT_PARTS_BYTES / 3) as usize;
    let heavy: Vec<(String, Vec<u8>)> = (0..4)
        .map(|i| (format!("word/footer{}.xml", 100 + i), header(each)))
        .collect();
    assert!(matches!(
        list_fields(&with_entries(&heavy)),
        Err(PatchError::TooLarge)
    ));
    // A few small ones are fine.
    assert!(list_fields(&with_entries(&many[..3])).is_ok());
}

/// The fixture with paragraph `index`'s first run replaced by `run`.
fn with_run_in(index: usize, run: &str) -> (Vec<u8>, String) {
    let xml = entry(FIXTURE, "word/document.xml");
    let paras = scan_paragraphs(&xml).unwrap();
    let p = &paras[index];
    let para = &xml[p.start..p.end];
    let at = para.find("<w:r>").or_else(|| para.find("<w:r ")).unwrap();
    let new_para = format!("{}{run}{}", &para[..at], &para[at..]);
    let new_xml = format!("{}{new_para}{}", &xml[..p.start], &xml[p.end..]);
    let parts = std::collections::BTreeMap::from([("word/document.xml".to_owned(), new_xml)]);
    let docx = splice_parts(FIXTURE, &parts, NOW).unwrap();
    let text = list_paragraphs(&docx).unwrap()[index].text.clone();
    (docx, text)
}

#[test]
fn a_paragraph_holding_a_picture_or_link_is_refused_and_says_what() {
    let index = list_paragraphs(FIXTURE)
        .unwrap()
        .iter()
        .find(|p| p.text.chars().count() > 4 && !p.in_table)
        .unwrap()
        .index;
    for (run, what) in [
        (
            "<w:r><w:drawing><wp:inline/></w:drawing></w:r>",
            "w:drawing",
        ),
        (
            r#"<w:hyperlink r:id="rId99"><w:r><w:t>链接</w:t></w:r></w:hyperlink>"#,
            "w:hyperlink",
        ),
        (r#"<w:r><w:footnoteReference w:id="1"/></w:r>"#, "footnote"),
    ] {
        let (docx, text) = with_run_in(index, run);
        for tracked in [false, true] {
            let edit = ParaEdit {
                index,
                text: "新".into(),
                tracked,
                expect: Some(text.clone()),
            };
            match apply(&docx, &[edit], "A", NOW) {
                Err(PatchError::Invalid(why)) => assert!(why.contains(what), "{why}"),
                other => panic!("{run}: {other:?}"),
            }
        }
    }
}

#[test]
fn a_comment_reference_sharing_a_run_with_text_is_refused() {
    let index = list_paragraphs(FIXTURE)
        .unwrap()
        .iter()
        .find(|p| p.text.chars().count() > 4 && !p.in_table)
        .unwrap()
        .index;
    let (docx, text) = with_run_in(
        index,
        r#"<w:r><w:t>旧字</w:t><w:commentReference w:id="7"/></w:r>"#,
    );
    let edit = ParaEdit {
        index,
        text: "新".into(),
        tracked: false,
        expect: Some(text),
    };
    match apply(&docx, &[edit], "A", NOW) {
        Err(PatchError::Invalid(why)) => assert!(why.contains("comment reference"), "{why}"),
        other => panic!("{other:?}"),
    }
}

/// PR-Daemon #1930 B2 (Loop 4): a whitelist. A formula, a pending tracked
/// deletion, a page break or a symbol in the paragraph would be lost by a
/// rewrite: refused, named. A tab or a line break in a text run is text.
#[test]
fn a_paragraph_holding_a_formula_or_a_pending_deletion_is_refused_and_says_what() {
    let index = list_paragraphs(FIXTURE)
        .unwrap()
        .iter()
        .find(|p| p.text.chars().count() > 4 && !p.in_table)
        .unwrap()
        .index;
    for (run, what) in [
        (r#"<m:oMath><m:r><m:t>x=1</m:t></m:r></m:oMath>"#, "m:oMath"),
        (
            r#"<w:del w:id="91" w:author="A" w:date="2026-01-01T00:00:00Z"><w:r><w:delText>旧</w:delText></w:r></w:del>"#,
            "w:del",
        ),
        (
            r#"<w:r><w:br w:type="page"/></w:r>"#,
            "page or column break",
        ),
        (
            r#"<w:r><w:sym w:font="Wingdings" w:char="F0FC"/></w:r>"#,
            "w:sym",
        ),
    ] {
        let (docx, text) = with_run_in(index, run);
        for tracked in [false, true] {
            let edit = ParaEdit {
                index,
                text: "新".into(),
                tracked,
                expect: Some(text.clone()),
            };
            match apply(&docx, &[edit], "A", NOW) {
                Err(PatchError::Invalid(why)) => assert!(why.contains(what), "{why}"),
                other => panic!("{run}: {other:?}"),
            }
        }
    }
}

#[test]
fn a_paragraph_with_a_tab_or_a_line_break_is_still_rewritten() {
    let index = list_paragraphs(FIXTURE)
        .unwrap()
        .iter()
        .find(|p| p.text.chars().count() > 4 && !p.in_table)
        .unwrap()
        .index;
    let (docx, text) = with_run_in(
        index,
        r#"<w:r><w:t>甲</w:t><w:tab/><w:t>乙</w:t><w:br/></w:r>"#,
    );
    let edit = ParaEdit {
        index,
        text: "新的一段".into(),
        tracked: false,
        expect: Some(text),
    };
    let out = apply(&docx, &[edit], "A", NOW).unwrap();
    assert_eq!(list_paragraphs(&out).unwrap()[index].text, "新的一段");
}

/// General 11-21 on 0.6.66: a tracked rewrite of a paragraph with tabs and a
/// line break deletes the original runs as they were (tabs and the break
/// kept), so rejecting the change gives back the paragraph as it was; tabs
/// and line breaks in the new text are written as w:tab and w:br.
#[test]
fn rejecting_a_tracked_rewrite_gives_back_tabs_and_line_breaks() {
    let index = list_paragraphs(FIXTURE)
        .unwrap()
        .iter()
        .find(|p| p.text.chars().count() > 4 && !p.in_table)
        .unwrap()
        .index;
    let original = r#"<w:r><w:t>甲方</w:t><w:tab/><w:t>010</w:t><w:br/><w:t>乙方</w:t></w:r>"#;
    let (docx, text) = with_run_in(index, original);
    let before = entry(&docx, "word/document.xml");
    let before_paras = scan_paragraphs(&before).unwrap();
    let old_para = &before[before_paras[index].start..before_paras[index].end];
    let edit = ParaEdit {
        index,
        text: "联系人\t张三\n电话".into(),
        tracked: true,
        expect: Some(text),
    };
    let out = apply(&docx, &[edit], "A", NOW).unwrap();
    let xml = entry(&out, "word/document.xml");
    let paras = scan_paragraphs(&xml).unwrap();
    let para = &xml[paras[index].start..paras[index].end];
    let del = &para[para.find("<w:del ").unwrap()..para.find("</w:del>").unwrap()];
    let ins = &para[para.find("<w:ins ").unwrap()..para.find("</w:ins>").unwrap()];
    // Rejecting: the deleted runs, delText back to t, are the original runs.
    let rejected = del[del.find('>').unwrap() + 1..].replace("w:delText", "w:t");
    for (_, s, e) in children(old_para)
        .into_iter()
        .filter(|(n, _, _)| *n == "w:r")
    {
        assert!(
            rejected.contains(&old_para[s..e]),
            "{} not in {rejected}",
            &old_para[s..e]
        );
    }
    // Accepting: the new text, a tab and a line break where \t and \n were,
    // in that order, written as Word does.
    let expected = r#"<w:t xml:space="preserve">联系人</w:t><w:tab/><w:t xml:space="preserve">张三</w:t><w:br/><w:t xml:space="preserve">电话</w:t>"#;
    assert!(ins.contains(expected), "{ins}");
    assert!(!ins.contains('\t') && !ins.contains('\n'), "{ins}");
    // Rejecting keeps the order too: the tab before 010, the break before 乙方.
    let order = |hay: &str, a: &str, b: &str| hay.find(a).unwrap() < hay.find(b).unwrap();
    assert!(
        order(&rejected, "<w:tab/>", "010") && order(&rejected, "<w:br/>", "乙方"),
        "{rejected}"
    );
}

/// PM 11-21: a plain (untracked) rewrite writes \t and \n as w:tab and w:br too.
#[test]
fn a_plain_rewrite_writes_tabs_and_line_breaks_as_word_does() {
    let index = list_paragraphs(FIXTURE)
        .unwrap()
        .iter()
        .find(|p| p.text.chars().count() > 4 && !p.in_table)
        .unwrap()
        .index;
    let text = list_paragraphs(FIXTURE).unwrap()[index].text.clone();
    let edit = ParaEdit {
        index,
        text: "甲\t乙\n丙".into(),
        tracked: false,
        expect: Some(text),
    };
    let out = apply(FIXTURE, &[edit], "A", NOW).unwrap();
    let xml = entry(&out, "word/document.xml");
    let paras = scan_paragraphs(&xml).unwrap();
    let para = &xml[paras[index].start..paras[index].end];
    assert!(
        para.contains(r#"<w:t xml:space="preserve">甲</w:t><w:tab/><w:t xml:space="preserve">乙</w:t><w:br/><w:t xml:space="preserve">丙</w:t>"#),
        "{para}"
    );
    assert!(!para.contains("<w:del ") && !para.contains('\t'), "{para}");
}

/// PR-Daemon #1954 B1: a tab and a line break are read as \t and \n, so a
/// paragraph read and written back as it is keeps them, and reads the same.
#[test]
fn reading_and_writing_back_keeps_tabs_and_line_breaks() {
    let index = list_paragraphs(FIXTURE)
        .unwrap()
        .iter()
        .find(|p| p.text.chars().count() > 4 && !p.in_table)
        .unwrap()
        .index;
    let (docx, text) = with_run_in(
        index,
        r#"<w:r><w:t>甲方</w:t><w:tab/><w:t>010</w:t><w:br/><w:t>乙方</w:t></w:r>"#,
    );
    assert!(text.starts_with("甲方\t010\n乙方"), "{text:?}");
    let edit = ParaEdit {
        index,
        text: text.clone(),
        tracked: false,
        expect: Some(text.clone()),
    };
    let out = apply(&docx, &[edit], "A", NOW).unwrap();
    let xml = entry(&out, "word/document.xml");
    let paras = scan_paragraphs(&xml).unwrap();
    let para = &xml[paras[index].start..paras[index].end];
    assert!(
        para.contains("<w:tab/>") && para.contains("<w:br/>"),
        "{para}"
    );
    assert_eq!(list_paragraphs(&out).unwrap()[index].text, text);
    // A page break is not text: it is never read as a line break.
    assert_eq!(
        paragraph_text(r#"<w:p><w:r><w:t>a</w:t><w:br w:type='page'/></w:r></w:p>"#),
        "a"
    );
}

/// PR-Daemon #1954 L2/L3: `<w:t` followed by any whitespace becomes a valid
/// w:delText, never touching w:tab; a page break in single quotes is refused.
#[test]
fn deleted_runs_stay_valid_and_single_quoted_page_breaks_are_refused() {
    let run = "<w:r><w:t\nxml:space=\"preserve\">a</w:t\n><w:tab/><w:t\txml:space='preserve'>b</w:t></w:r>";
    assert_eq!(
        as_deleted(run),
        "<w:r><w:delText\nxml:space=\"preserve\">a</w:delText\n><w:tab/><w:delText\txml:space='preserve'>b</w:delText></w:r>"
    );
    assert!(is_page_or_column_break(r#"<w:br w:type='page'/>"#));
    assert!(is_page_or_column_break(r#"<w:br w:type = "column"/>"#));
    assert!(!is_page_or_column_break(r#"<w:br/>"#));
    assert!(!is_page_or_column_break(r#"<w:br w:type="textWrapping"/>"#));
    assert!(not_plain_text(r#"<w:p><w:r><w:br w:type='page'/></w:r></w:p>"#).is_some());
}

/// PR-Daemon #1954 L2 (PM: must fix): a tracked rewrite of runs whose `<w:t`
/// is followed by a line break or a tab writes a document.xml an XML parser
/// reads; and a part that would not parse is an error, never Ok.
#[test]
fn a_tracked_rewrite_writes_xml_that_parses_and_broken_xml_is_refused() {
    let index = list_paragraphs(FIXTURE)
        .unwrap()
        .iter()
        .find(|p| p.text.chars().count() > 4 && !p.in_table)
        .unwrap()
        .index;
    let run = "<w:r><w:t\nxml:space=\"preserve\">甲</w:t\n><w:tab/><w:t\txml:space=\"preserve\">乙</w:t></w:r>";
    let (docx, text) = with_run_in(index, run);
    for tracked in [true, false] {
        let edit = ParaEdit {
            index,
            text: "新".into(),
            tracked,
            expect: Some(text.clone()),
        };
        let out = apply(&docx, &[edit], "A", NOW).unwrap();
        let xml = entry(&out, "word/document.xml");
        roxmltree::Document::parse(&xml).unwrap_or_else(|e| panic!("tracked={tracked}: {e}"));
    }
    let broken = std::collections::BTreeMap::from([(
        "word/document.xml".to_owned(),
        "<w:document><w:body><w:p><w:delText>a</w:t></w:p></w:body></w:document>".to_owned(),
    )]);
    assert!(matches!(
        splice_parts(FIXTURE, &broken, NOW),
        Err(PatchError::Invalid(_))
    ));
}

/// PR-Daemon #1954 B1': tab stops in the paragraph's properties
/// (`w:pPr/w:tabs/w:tab`) are not text; a read → write back adds no tab.
#[test]
fn tab_stops_in_properties_are_not_read_as_tabs() {
    assert_eq!(
        paragraph_text(
            r#"<w:p><w:pPr><w:tabs><w:tab w:val="left" w:pos="720"/></w:tabs></w:pPr><w:r><w:rPr><w:b/></w:rPr><w:t>a</w:t><w:tab/><w:t>b</w:t></w:r></w:p>"#
        ),
        "a\tb"
    );
    let index = list_paragraphs(FIXTURE)
        .unwrap()
        .iter()
        .find(|p| p.text.chars().count() > 4 && !p.in_table)
        .unwrap()
        .index;
    // The paragraph gets tab stops in its properties, and no tab in its text.
    let xml = entry(FIXTURE, "word/document.xml");
    let paras = scan_paragraphs(&xml).unwrap();
    let para = &xml[paras[index].start..paras[index].end];
    let stops =
        r#"<w:tabs><w:tab w:val="left" w:pos="720"/><w:tab w:val="right" w:pos="9000"/></w:tabs>"#;
    let new_para = match para.find("<w:pPr>") {
        Some(at) => format!("{}{stops}{}", &para[..at + 7], &para[at + 7..]),
        None => {
            let open_end = para.find('>').unwrap() + 1;
            format!(
                "{}<w:pPr>{stops}</w:pPr>{}",
                &para[..open_end],
                &para[open_end..]
            )
        }
    };
    let new_xml = format!(
        "{}{new_para}{}",
        &xml[..paras[index].start],
        &xml[paras[index].end..]
    );
    let parts = std::collections::BTreeMap::from([("word/document.xml".to_owned(), new_xml)]);
    let docx = splice_parts(FIXTURE, &parts, NOW).unwrap();
    let text = list_paragraphs(&docx).unwrap()[index].text.clone();
    assert!(!text.contains('\t'), "{text:?}");
    let edit = ParaEdit {
        index,
        text: text.clone(),
        tracked: false,
        expect: Some(text.clone()),
    };
    let out = apply(&docx, &[edit], "A", NOW).unwrap();
    let out_xml = entry(&out, "word/document.xml");
    let out_paras = scan_paragraphs(&out_xml).unwrap();
    let out_para = &out_xml[out_paras[index].start..out_paras[index].end];
    let count = |p: &str| p.matches("<w:tab ").count() + p.matches("<w:tab/>").count();
    assert_eq!(count(out_para), count(&new_para), "{out_para}");
    assert_eq!(list_paragraphs(&out).unwrap()[index].text, text);
    // Not refused for them either: properties are not checked as content.
    assert_eq!(not_plain_text(&new_para), None);
}

/// PM 11-21: public model contracts saved by Microsoft Word, from the
/// SAMR model contract library (htsfwb.samr.gov.cn, government public texts):
/// 黑龙江省家居装饰装修工程施工合同（试行）（2025 版）— tab stops, run tabs
/// (signature lines), underlines, tables, headers and footers; 山东省体育
/// 健身服务合同（2025 版）— tab stops, automatic numbering (numPr), tables,
/// headers and footers. Plus the LibreOffice-saved one made for this.
const REAL_CONTRACTS: [(&str, &[u8]); 3] = [
    (
        "samr-heilongjiang-decoration-2025",
        include_bytes!("../../tests/fixtures/samr-heilongjiang-decoration-2025.docx"),
    ),
    (
        "samr-shandong-fitness-2025",
        include_bytes!("../../tests/fixtures/samr-shandong-fitness-2025.docx"),
    ),
    (
        "contract-realistic",
        include_bytes!("../../tests/fixtures/contract-realistic.docx"),
    ),
];

/// Every paragraph of each real contract reads with as many \t as its runs
/// have w:tab, and written back as read (plain, and tracked) leaves
/// document.xml byte for byte as it was.
#[test]
fn real_contracts_read_and_write_back_unchanged() {
    let mut seen = std::collections::BTreeMap::new();
    for (name, docx) in REAL_CONTRACTS {
        let xml = entry(docx, "word/document.xml");
        let paras = list_paragraphs(docx).unwrap();
        let spans = scan_paragraphs(&xml).unwrap();
        for (p, span) in paras.iter().zip(&spans) {
            let para = &xml[span.start..span.end];
            let run_tabs = children(para)
                .into_iter()
                .filter(|(n, _, _)| *n == "w:r")
                .map(|(_, s, e)| para[s..e].matches("<w:tab/>").count())
                .sum::<usize>();
            assert_eq!(
                p.text.matches('\t').count(),
                run_tabs,
                "{name} paragraph {}: {:?}",
                p.index,
                p.text
            );
        }
        // Every paragraph written back as read, at most MAX_EDITS a patch.
        for tracked in [false, true] {
            for chunk in paras.chunks(MAX_EDITS) {
                let edits: Vec<ParaEdit> = chunk
                    .iter()
                    .map(|p| ParaEdit {
                        index: p.index,
                        text: p.text.clone(),
                        tracked,
                        expect: Some(p.text.clone()),
                    })
                    .collect();
                let out = apply(docx, &edits, "A", NOW).unwrap();
                assert_eq!(
                    entry(&out, "word/document.xml"),
                    xml,
                    "{name} paragraphs {}.. tracked={tracked}",
                    chunk[0].index
                );
            }
        }
        for needle in ["<w:tabs>", "<w:tab/>", "<w:numPr>", "<w:tbl>", "<w:u "] {
            *seen.entry(needle).or_insert(0) += xml.matches(needle).count();
        }
    }
    // Across them, every structure the reviews asked for is there.
    for (needle, count) in &seen {
        assert!(*count > 0, "no {needle} in the real contracts: {seen:?}");
    }
}

/// General 11-21: the write path, not only the no-op. On the real contracts
/// every paragraph that can be rewritten gets one character more (「。」),
/// plain and tracked: its tabs and line breaks stay as many and where they
/// were (its text reads back as the old text plus 「。」), every other
/// paragraph is byte for byte as it was, and document.xml parses.
#[test]
fn real_contracts_keep_tabs_and_breaks_when_a_character_is_added() {
    let count = |p: &str, tag: &str| {
        children(p)
            .into_iter()
            .filter(|(n, _, _)| *n == "w:r")
            .map(|(_, s, e)| {
                children(&p[s..e])
                    .into_iter()
                    .filter(|(n, _, _)| *n == tag)
                    .count()
            })
            .sum::<usize>()
    };
    let mut edited_total = 0;
    for (name, docx) in REAL_CONTRACTS {
        let xml = entry(docx, "word/document.xml");
        let spans = scan_paragraphs(&xml).unwrap();
        let paras = list_paragraphs(docx).unwrap();
        let editable: Vec<&ParaInfo> = paras
            .iter()
            .filter(|p| !p.text.trim().is_empty())
            .filter(|p| not_plain_text(&xml[spans[p.index].start..spans[p.index].end]).is_none())
            .collect();
        edited_total += editable.len();
        for tracked in [false, true] {
            for chunk in editable.chunks(MAX_EDITS) {
                let edits: Vec<ParaEdit> = chunk
                    .iter()
                    .map(|p| ParaEdit {
                        index: p.index,
                        text: format!("{}。", p.text),
                        tracked,
                        expect: Some(p.text.clone()),
                    })
                    .collect();
                let out = apply(docx, &edits, "A", NOW).unwrap();
                let new_xml = entry(&out, "word/document.xml");
                roxmltree::Document::parse(&new_xml).unwrap_or_else(|e| panic!("{name}: {e}"));
                let new_spans = scan_paragraphs(&new_xml).unwrap();
                assert_eq!(new_spans.len(), spans.len(), "{name}");
                let edited: std::collections::HashSet<usize> =
                    chunk.iter().map(|p| p.index).collect();
                for (i, (old, new)) in spans.iter().zip(&new_spans).enumerate() {
                    let (o, n) = (&xml[old.start..old.end], &new_xml[new.start..new.end]);
                    if !edited.contains(&i) {
                        assert_eq!(o, n, "{name} paragraph {i} changed though not edited");
                        continue;
                    }
                    let inserted = if tracked {
                        &n[n.find("<w:ins ").unwrap()..n.find("</w:ins>").unwrap()]
                    } else {
                        n
                    };
                    let inserted = format!(
                        "<w:p>{}</w:p>",
                        inserted
                            .trim_start_matches(|c| c != '>')
                            .trim_start_matches('>')
                    );
                    let wanted = format!("{}。", paras[i].text);
                    if tracked {
                        assert_eq!(
                            paragraph_text(&inserted),
                            wanted,
                            "{name} paragraph {i} tracked"
                        );
                    } else {
                        assert_eq!(paragraph_text(n), wanted, "{name} paragraph {i}");
                    }
                    for tag in ["w:tab", "w:br"] {
                        let new_count = if tracked {
                            count(&inserted, tag)
                        } else {
                            count(n, tag)
                        };
                        assert_eq!(
                            new_count,
                            count(o, tag),
                            "{name} paragraph {i} {tag} tracked={tracked}"
                        );
                    }
                }
            }
        }
    }
    assert!(edited_total > 50, "{edited_total}");
    // A paragraph the whitelist refuses (a picture, a field, a page break, ...)
    // is refused with a character more too, and says what it holds.
    let mut refused_total = 0;
    for (name, docx) in REAL_CONTRACTS {
        let xml = entry(docx, "word/document.xml");
        let spans = scan_paragraphs(&xml).unwrap();
        for p in list_paragraphs(docx).unwrap() {
            let Some(what) = not_plain_text(&xml[spans[p.index].start..spans[p.index].end]) else {
                continue;
            };
            refused_total += 1;
            for tracked in [false, true] {
                let edit = ParaEdit {
                    index: p.index,
                    text: format!("{}。", p.text),
                    tracked,
                    expect: Some(p.text.clone()),
                };
                match apply(docx, &[edit], "A", NOW) {
                    Err(PatchError::Invalid(why)) => {
                        assert!(
                            why.contains(&what) && why.contains("edit it in Word or WPS"),
                            "{name} paragraph {}: {why}",
                            p.index
                        );
                    }
                    other => panic!(
                        "{name} paragraph {} ({what}) not refused: {other:?}",
                        p.index
                    ),
                }
            }
        }
    }
    assert!(
        refused_total > 0,
        "no refused paragraph in the real contracts"
    );
}

/// M1 (0.6.75): the model's `expect` without 「第三条 」 or half of it still
/// names that paragraph; another paragraph's text does not.
#[test]
fn expect_is_the_paragraph_without_numbering_or_a_long_enough_part_of_it() {
    let para = "第三条 押金为两个月租金，合同期满无违约时全额退还。";
    for ok in [
        para,
        "押金为两个月租金，合同期满无违约时全额退还。",
        "押金为两个月租金，合同期满无违约时全额退还",
        "  第三条押金为两个月租金，合同期满无违约时全额退还 ",
        "押金为两个月租金",
    ] {
        assert!(super::expect_matches(para, ok), "{ok}");
    }
    for wrong in ["乙方：张三", "押金", "第四条 租金每月 8,500 元。", ""] {
        assert!(!super::expect_matches(para, wrong), "{wrong}");
    }
    assert!(super::expect_matches("（一）租金", "租金"));
    assert!(super::expect_matches("1. Scope", "Scope"));
    assert!(super::expect_matches("一、总则", "总则"));
    assert!(super::expect_matches("", ""));
}
