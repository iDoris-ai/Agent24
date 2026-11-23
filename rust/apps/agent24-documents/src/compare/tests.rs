#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

fn paras(texts: &[&str]) -> Vec<Unit> {
    texts
        .iter()
        .enumerate()
        .map(|(i, t)| Unit {
            block_ref: BlockRef::Paragraph { paragraph_index: i },
            text: (*t).to_owned(),
        })
        .collect()
}

fn marked(c: &Change, op: Op) -> String {
    c.spans
        .iter()
        .filter(|s| s.op == op)
        .map(|s| s.text.as_str())
        .collect()
}

const RENT: &str = "乙方每月向甲方支付租金人民币 5 万元，按季度支付。";
const RENT8: &str = "乙方每月向甲方支付租金人民币 8 万元，按季度支付。";
const ART1: &str = "第一条 租赁标的物为上海市浦东新区某路一号";
const ART3: &str = "第三条 违约责任按法律规定处理";

#[test]
fn identical_text_has_no_changes() {
    let a = paras(&["甲方：张三", RENT]);
    let c = compare(&a, &a.clone());
    assert!(c.identical);
    assert!(c.changes.is_empty());
    assert!(c.only_text);
    assert_eq!(c.summary.total, 0);
}

#[test]
fn whitespace_and_empty_paragraphs_are_not_changes() {
    let a = paras(&["甲方：张三", "", RENT]);
    let b = paras(&["甲方：张三  ", RENT, "  "]);
    assert!(compare(&a, &b).identical);
}

#[test]
fn one_changed_number_is_one_change_with_only_the_number_marked() {
    let a = paras(&["甲方：张三", RENT, "乙方：李四"]);
    let b = paras(&["甲方：张三", RENT8, "乙方：李四"]);
    let c = compare(&a, &b);
    assert!(!c.identical);
    assert_eq!(c.changes.len(), 1);
    let ch = &c.changes[0];
    assert_eq!(ch.kind, Kind::Changed);
    assert_eq!(marked(ch, Op::Delete), "5");
    assert_eq!(marked(ch, Op::Insert), "8");
    assert_eq!(
        ch.left.as_ref().unwrap().block_ref,
        BlockRef::Paragraph { paragraph_index: 1 }
    );
    assert_eq!(
        ch.right.as_ref().unwrap().block_ref,
        BlockRef::Paragraph { paragraph_index: 1 }
    );
    assert_eq!(
        c.summary,
        Summary {
            total: 1,
            added: 0,
            removed: 0,
            changed: 1,
            unaligned: 0
        }
    );
}

#[test]
fn a_deleted_paragraph_is_one_removed() {
    let a = paras(&["甲方：张三", RENT, "乙方：李四"]);
    let b = paras(&["甲方：张三", "乙方：李四"]);
    let c = compare(&a, &b);
    assert_eq!(c.changes.len(), 1);
    assert_eq!(c.changes[0].kind, Kind::Removed);
    assert!(c.changes[0].right.is_none());
    assert_eq!(c.changes[0].left.as_ref().unwrap().text, RENT);
    assert_eq!(c.summary.removed, 1);
}

#[test]
fn an_added_paragraph_is_one_added() {
    let a = paras(&["甲方：张三", "乙方：李四"]);
    let b = paras(&["甲方：张三", RENT, "乙方：李四"]);
    let c = compare(&a, &b);
    assert_eq!(c.changes.len(), 1);
    assert_eq!(c.changes[0].kind, Kind::Added);
    assert!(c.changes[0].left.is_none());
    assert_eq!(c.summary.added, 1);
}

#[test]
fn a_moved_paragraph_is_unaligned_never_changed() {
    let a = paras(&[ART1, RENT, ART3]);
    let b = paras(&[ART1, ART3, RENT]);
    let c = compare(&a, &b);
    assert!(!c.identical);
    assert_eq!(c.summary.changed, 0);
    assert!(c.summary.unaligned >= 2);
    assert!(c.changes.iter().all(|x| x.kind == Kind::Unaligned));
}

#[test]
fn an_edit_and_a_deletion_together_are_counted_apart() {
    let a = paras(&["甲方：张三", RENT, "附：无", "乙方：李四"]);
    let b = paras(&["甲方：张三", RENT8, "乙方：李四"]);
    let c = compare(&a, &b);
    assert_eq!(c.summary.changed, 1);
    assert_eq!(c.summary.removed, 1);
    assert_eq!(c.summary.total, 2);
}

#[test]
fn empty_and_single_paragraph_documents() {
    assert!(compare(&[], &[]).identical);
    let one = paras(&["甲方：张三"]);
    assert_eq!(compare(&[], &one).summary.added, 1);
    assert_eq!(compare(&one, &[]).summary.removed, 1);
}

#[test]
fn ids_are_unique_and_numbered_in_order() {
    let a = paras(&["甲", "乙乙乙乙乙", "丙"]);
    let b = paras(&["丁", "乙乙乙乙乙", "戊"]);
    let c = compare(&a, &b);
    assert!(c.changes.len() >= 2);
    for (i, ch) in c.changes.iter().enumerate() {
        assert_eq!(ch.id, format!("c{}", i + 1));
    }
}

#[test]
fn a_long_contract_with_three_edits_has_exactly_three_changes() {
    let base: Vec<String> = (0..293)
        .map(|i| format!("第 {i} 段：双方就本合同第 {i} 项约定的内容与履行方式达成一致意见。"))
        .collect();
    let mut edited = base.clone();
    edited[10] = edited[10].replace("一致意见", "补充意见");
    edited[150] = edited[150].replace("第 150 项", "第 151 项");
    edited[290] = format!("{}另见附件。", edited[290]);
    let a: Vec<&str> = base.iter().map(String::as_str).collect();
    let b: Vec<&str> = edited.iter().map(String::as_str).collect();
    let c = compare(&paras(&a), &paras(&b));
    assert_eq!(c.summary.changed, 3);
    assert_eq!(c.summary.total, 3);
}
