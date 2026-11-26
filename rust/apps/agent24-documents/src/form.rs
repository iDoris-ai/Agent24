//! L8-S02: a template field's kind, and which values a fill may write.
//! Identity is written only from the user's material or their own words:
//! a model must not invent a name, a phone number or an address.

use std::collections::{BTreeMap, HashSet};

use serde::Serialize;

/// How a field may be filled. Serialized as `manual`, `identity` or `text`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FieldKind {
    Manual,
    Identity,
    Text,
}

/// Checked first: 「本人签名」 is signed by hand, not treated as a name.
const MANUAL: &[&str] = &[
    "签名", "签字", "签章", "盖章", "照片", "相片", "头像", "附件", "指纹", "勾选", "☐", "□",
];
const IDENTITY: &[&str] = &[
    "姓名",
    "名字",
    "户名",
    "身份证",
    "证件",
    "护照",
    "电话",
    "手机",
    "联系",
    "邮箱",
    "电子邮件",
    "微信",
    "QQ",
    "地址",
    "住址",
    "户籍",
    "银行",
    "开户",
    "卡号",
    "账号",
    "收款",
    "持卡",
    "出生",
    "生日",
    "性别",
    "民族",
    "学号",
    "工号",
    "工作单位",
    "甲方",
    "乙方",
    "承租",
    "出租",
    "法定代表人",
    "法人",
    "申请人",
    "当事人",
    "委托人",
    "代理人",
    "担保人",
    "签约人",
];
/// English, as whole words, any case: 「Phone」「full_name」「EmailAddress」,
/// never 「valid」 or 「width」 for `id`.
const IDENTITY_WORDS: &[&str] = &[
    "name", "phone", "tel", "mobile", "email", "mail", "address", "id", "passport", "birthday",
];

/// The ASCII words of a name, lower case: split at anything not a letter or
/// digit and before an upper-case letter that follows a lower-case one.
fn words(name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut prev_lower = false;
    for c in name.chars() {
        let boundary = !c.is_ascii_alphanumeric() || (c.is_ascii_uppercase() && prev_lower);
        if boundary && !word.is_empty() {
            out.push(std::mem::take(&mut word));
        }
        if c.is_ascii_alphanumeric() {
            word.push(c.to_ascii_lowercase());
        }
        prev_lower = c.is_ascii_lowercase();
    }
    if !word.is_empty() {
        out.push(word);
    }
    out
}

/// `manual` when the name asks for a signature, photo, checkbox or attachment;
/// else `identity` for a name, id, phone, address or account; else `text`.
#[must_use]
pub fn field_kind(name: &str) -> FieldKind {
    if MANUAL.iter().any(|k| name.contains(k)) {
        FieldKind::Manual
    } else if IDENTITY.iter().any(|k| name.contains(k))
        || words(name)
            .iter()
            .any(|w| IDENTITY_WORDS.contains(&w.as_str()))
    {
        FieldKind::Identity
    } else {
        FieldKind::Text
    }
}

/// What a fill will write, and what it tells the model to pass on.
pub(crate) struct FillReport {
    pub(crate) write: BTreeMap<String, String>,
    pub(crate) filled: Vec<String>,
    pub(crate) ask_user: Vec<String>,
    pub(crate) manual: Vec<String>,
    pub(crate) unfilled: Vec<String>,
    pub(crate) note: String,
}

/// `found` is the identity fields whose value occurs in the user's material.
/// A name the template does not have stays in `write`, so fill still refuses it.
pub(crate) fn plan_fill(
    fields: &[(String, FieldKind)],
    values: &BTreeMap<String, String>,
    user_said: &[String],
    found: &HashSet<String>,
) -> FillReport {
    let known: HashSet<&str> = fields.iter().map(|(n, _)| n.as_str()).collect();
    let said = |name: &str| user_said.iter().any(|s| s == name);
    let mut write = BTreeMap::new();
    for (name, value) in values {
        if !known.contains(name.as_str()) {
            write.insert(name.clone(), value.clone());
            continue;
        }
        if value.trim().is_empty() {
            continue;
        }
        match field_kind(name) {
            FieldKind::Manual => {}
            FieldKind::Identity if said(name) || found.contains(name) => {
                write.insert(name.clone(), value.clone());
            }
            FieldKind::Identity => {}
            FieldKind::Text => {
                write.insert(name.clone(), value.clone());
            }
        }
    }
    let written: HashSet<&str> = write.keys().map(String::as_str).collect();
    let mut filled = Vec::new();
    let mut ask_user = Vec::new();
    let mut manual = Vec::new();
    let mut unfilled = Vec::new();
    for (name, kind) in fields {
        match kind {
            FieldKind::Manual => manual.push(name.clone()),
            FieldKind::Identity if written.contains(name.as_str()) => filled.push(name.clone()),
            FieldKind::Identity if values.get(name).is_some_and(|v| v.trim().is_empty()) => {
                unfilled.push(name.clone());
            }
            FieldKind::Identity => ask_user.push(name.clone()),
            FieldKind::Text if written.contains(name.as_str()) => filled.push(name.clone()),
            FieldKind::Text => unfilled.push(name.clone()),
        }
    }
    let note = fill_note(&filled, &ask_user, &manual, &unfilled);
    FillReport {
        write,
        filled,
        ask_user,
        manual,
        unfilled,
        note,
    }
}

pub(crate) fn fill_note(
    filled: &[String],
    ask: &[String],
    manual: &[String],
    unfilled: &[String],
) -> String {
    let mut s = String::new();
    if !filled.is_empty() {
        s.push_str(&format!(
            "已填 {} 项：{}。",
            filled.len(),
            filled.join("、")
        ));
    }
    if !ask.is_empty() {
        s.push_str(&format!("资料里没有、需要问用户：{}。", ask.join("、")));
    }
    if !manual.is_empty() {
        s.push_str(&format!("需要你手工处理：{}。", manual.join("、")));
    }
    if !unfilled.is_empty() {
        s.push_str(&format!("还没填：{}。", unfilled.join("、")));
    }
    s.push_str("身份类信息只能用资料或用户亲口说的，不要自己补。");
    s
}

/// L8-S02 round 2 (监工 11-26 真机): what the user's material states for
/// the template's fields, read by the App, not the model. Precision first
/// (PR-Daemon #2095): a reading that is not certain gives no value, and the
/// user is asked; a wrong value would come with the App's authority.
#[derive(Debug, Default)]
pub(crate) struct Labelled {
    /// A field's one value.
    pub(crate) values: BTreeMap<String, String>,
    /// Fields the material states more than one way: asked, never guessed.
    pub(crate) ambiguous: Vec<String>,
}

/// Common labels of a form that are not values (a header cell 「年龄」).
const COMMON_LABELS: &[&str] = &[
    "年龄",
    "部门",
    "职位",
    "职务",
    "岗位",
    "关系",
    "日期",
    "备注",
    "学历",
    "专业",
    "婚姻",
    "籍贯",
    "政治面貌",
    "序号",
    "项目",
    "内容",
    "说明",
];

/// A piece of the material, in document order: a paragraph (or a PDF line)
/// outside a table, or a table row's cells.
#[derive(Debug, Clone)]
pub(crate) enum Block {
    Line(String),
    Row(Vec<String>),
}

/// [`labelled_blocks`] of `lines`, then `rows`.
pub(crate) fn labelled(lines: &[String], rows: &[Vec<String>], fields: &[String]) -> Labelled {
    let blocks: Vec<Block> = lines
        .iter()
        .flat_map(|l| l.lines())
        .map(|l| Block::Line(l.to_owned()))
        .chain(rows.iter().cloned().map(Block::Row))
        .collect();
    labelled_blocks(&blocks, fields)
}

/// Words that say a heading is the user's own.
const OWN_HEADINGS: &[&str] = &[
    "个人",
    "本人",
    "基本信息",
    "申请人",
    "入职",
    "简历",
    "员工信息",
];
/// Words that say a heading is someone else's, even with an own word in it
/// (「申请人家庭成员」).
const OTHERS: &[&str] = &[
    "紧急",
    "联系人",
    "联络",
    "配偶",
    "家属",
    "家庭",
    "亲属",
    "监护",
    "推荐",
    "介绍",
    "担保",
    "代理",
    "证明人",
    "父",
    "母",
    "子女",
    "法定代表",
    "委托",
];

/// A heading (a line or a one-cell row that is no field) is the user's own
/// only when it says so: anything else heads someone else's details (fails
/// closed, PR-Daemon #2095 round 4).
fn own_heading(t: &str) -> bool {
    OWN_HEADINGS.iter().any(|w| t.contains(w)) && !OTHERS.iter().any(|w| t.contains(w))
}

/// Only labels equal to one of `fields` (whitespace and a trailing colon
/// aside) are read, by the most cautious rules (监工 11-26, PR-Daemon #2095
/// rounds 3–4), so that someone else's details are never read as the user's:
/// - Headings: a short line with no 「字段：」 and no colon, or a row of one
///   filled cell that is no field. What follows a heading is read only if
///   the heading is plainly the user's own (「个人信息」「基本信息」「申请人」);
///   any other (「紧急联络人」「监护人」) closes it, in paragraphs and tables
///   alike, until the next heading. Before the first heading, read.
/// - A line: exactly one 「字段：」 in it, opening it (after leading space and a
///   number 「1.」「（一）」); any other line is not read.
/// - A table row: strictly 「字段 | 值 | 字段 | 值…」 from its first cell; any
///   other cell and the row is not read. A row of fields only is a header,
///   read down to the next row. A value that the next row could also give
///   (F1) and differs is ambiguous.
/// - A value with two or more spaces in a row, a tab or U+3000 is ambiguous;
///   one with another colon (not a time's), another field's name, or a blank
///   (「____」「□」「（请填写）」) is none; one in brackets loses them. A field
///   found with two different values is ambiguous.
pub(crate) fn labelled_blocks(blocks: &[Block], fields: &[String]) -> Labelled {
    let names: Vec<String> = fields
        .iter()
        .map(|f| label_key(f))
        .filter(|k| !k.is_empty())
        .collect();
    let mut found: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut unsure: Vec<String> = Vec::new();
    let mut put = |name: &str, value: &str| {
        let value = unbracket(value.trim());
        if spread(value) {
            if !unsure.iter().any(|n| n == name) {
                unsure.push(name.to_owned());
            }
            return;
        }
        if usable(value, &names) {
            let seen = found.entry(name.to_owned()).or_default();
            if !seen.iter().any(|v| v == value) {
                seen.push(value.to_owned());
            }
        }
    };
    let is_field = |t: &str| names.contains(&label_key(t));
    let label_like = |t: &str| {
        let t = t.trim();
        is_field(t)
            || t.ends_with(['：', ':'])
            || field_kind(t) != FieldKind::Text
            || COMMON_LABELS.contains(&label_key(t).as_str())
    };
    let mut open = true;
    for (b, block) in blocks.iter().enumerate() {
        match block {
            Block::Line(line) => {
                let t = line.trim();
                if t.is_empty() {
                    continue;
                }
                let marks = markers(line, &names);
                if marks.is_empty() {
                    if heading_line(t) && !is_field(t) {
                        open = own_heading(t);
                    }
                    continue;
                }
                if !open {
                    continue;
                }
                let [(at, colon_end, name)] = marks.as_slice() else {
                    continue;
                };
                if !numbering_only(&line[..*at]) {
                    continue;
                }
                let value = line[*colon_end..]
                    .trim()
                    .trim_end_matches(['；', ';', '，', ',', '、', '|', '。']);
                put(name, value);
            }
            Block::Row(row) => {
                let filled: Vec<&String> = row.iter().filter(|c| !c.trim().is_empty()).collect();
                if filled.len() == 1 && !is_field(filled[0]) {
                    open = own_heading(filled[0].trim());
                    continue;
                }
                if !open {
                    continue;
                }
                let below = match blocks.get(b + 1) {
                    Some(Block::Row(n)) if n.len() == row.len() => Some(n),
                    _ => None,
                };
                // A header: every filled cell a field, read down.
                if filled.len() >= 2 && filled.iter().all(|c| is_field(c)) {
                    if let Some(below) = below {
                        for (h, v) in row.iter().zip(below) {
                            if is_field(h) && !v.trim().is_empty() && !label_like(v) {
                                put(&label_key(h), v);
                            }
                        }
                    }
                    continue;
                }
                // Strictly 「字段 | 值 | 字段 | 值…」 from the first cell.
                let strict = row.iter().enumerate().all(|(i, c)| {
                    if i % 2 == 0 {
                        // A trailing blank cell (merged or unused) is allowed.
                        is_field(c)
                            || (c.trim().is_empty() && row[i..].iter().all(|c| c.trim().is_empty()))
                    } else {
                        c.trim().is_empty() || !label_like(c)
                    }
                });
                if !strict {
                    continue;
                }
                for i in (0..row.len()).step_by(2) {
                    let Some(right) = row.get(i + 1).filter(|c| !c.trim().is_empty()) else {
                        continue;
                    };
                    put(&label_key(&row[i]), right);
                    // F1: the next row could give a value here too; if it
                    // differs, which is meant cannot be told.
                    if let Some(down) = below
                        .map(|n| &n[i])
                        .filter(|c| !c.trim().is_empty() && !label_like(c))
                    {
                        put(&label_key(&row[i]), down);
                    }
                }
            }
        }
    }
    let mut out = Labelled::default();
    for (name, values) in found {
        if values.len() == 1 && !unsure.contains(&name) {
            out.values
                .insert(name, values.into_iter().next().unwrap_or_default());
        } else {
            out.ambiguous.push(name);
        }
    }
    for name in unsure {
        if !out.ambiguous.contains(&name) {
            out.ambiguous.push(name);
        }
    }
    out
}

/// A heading line: short, no colon, not a sentence.
fn heading_line(t: &str) -> bool {
    t.chars().count() <= 20 && !t.contains(['：', ':', '。', '，', ','])
}

/// A value without the brackets around it (「（李四）」 → 「李四」), or without a
/// closing one it does not open (「李四）」 → 「李四」).
fn unbracket(value: &str) -> &str {
    let v = value
        .strip_prefix(['（', '('])
        .and_then(|v| v.strip_suffix(['）', ')']))
        .unwrap_or(value);
    let opens = v.matches(['（', '(']).count();
    let closes = v.matches(['）', ')']).count();
    if closes > opens {
        v.trim_end_matches(['）', ')']).trim()
    } else {
        v.trim()
    }
}

/// A value spread by two or more spaces, a tab or U+3000: 「陈  晓明」 is one
/// name or 「张三  部门 研发」 is a value and other words; which, cannot be told.
fn spread(value: &str) -> bool {
    value.contains("  ") || value.contains(['\t', '\u{3000}'])
}

/// Text before a line's field that is only space and a number: 「1.」「1、」
/// 「(1)」「（一）」「一、」.
fn numbering_only(before: &str) -> bool {
    let t = before.trim();
    if t.is_empty() {
        return true;
    }
    // r4 N2: 「（姓名：…」 is not a number (no digit); 「（一）」「(1)」 are.
    let digits = t
        .chars()
        .any(|c| c.is_ascii_digit() || "一二三四五六七八九十".contains(c));
    digits
        && t.chars().all(|c| {
            c.is_ascii_digit()
                || "一二三四五六七八九十".contains(c)
                || ".、．()（）".contains(c)
                || c.is_whitespace()
        })
}

/// Where each 「字段：」 of `line` starts and its colon ends, in order: a field
/// name at the start or after a space or separator, then a colon.
fn markers(line: &str, names: &[String]) -> Vec<(usize, usize, String)> {
    let mut marks: Vec<(usize, usize, String)> = Vec::new();
    for name in names {
        for (at, _) in line.match_indices(name.as_str()) {
            let before = line[..at].chars().next_back();
            if before.is_some_and(|c| !c.is_whitespace() && !"；;，,、|（(）).．".contains(c))
            {
                continue;
            }
            let after = &line[at + name.len()..];
            let gap = after.len() - after.trim_start().len();
            let rest = &after[gap..];
            if let Some(c) = rest.chars().next().filter(|c| *c == '：' || *c == ':') {
                marks.push((at, at + name.len() + gap + c.len_utf8(), name.clone()));
            }
        }
    }
    marks.sort_by_key(|m| m.0);
    // F3: a marker starting inside the one before (「联系人、电话：」 and
    // 「电话：」) is dropped, so a value's range never runs backwards.
    let mut kept: Vec<(usize, usize, String)> = Vec::new();
    for m in marks {
        if kept.last().is_none_or(|k| m.0 >= k.1) {
            kept.push(m);
        }
    }
    kept
}

/// A value worth giving: not empty, not long, not a blank to fill in, no
/// colon but a time's (「9:30」), no other field's name.
fn usable(value: &str, names: &[String]) -> bool {
    if value.is_empty() || value.chars().count() > 200 || value.contains("{{") {
        return false;
    }
    let blank = |c: char| c.is_whitespace() || "_＿-－—…·.．□☐■○●（）()".contains(c);
    if value.chars().all(blank)
        || ["请填写", "待填写", "请填入", "待补充"]
            .iter()
            .any(|w| value.contains(w))
    {
        return false;
    }
    let chars: Vec<char> = value.chars().collect();
    let odd_colon = chars.iter().enumerate().any(|(i, c)| {
        (*c == '：' || *c == ':')
            && !(*c == ':'
                && i > 0
                && chars[i - 1].is_ascii_digit()
                && chars.get(i + 1).is_some_and(char::is_ascii_digit))
    });
    let compact: String = value.chars().filter(|c| !c.is_whitespace()).collect();
    !odd_colon && !names.iter().any(|n| compact.contains(n.as_str()))
}

/// A label as compared with a field name: whitespace and a trailing colon
/// removed.
fn label_key(label: &str) -> String {
    label
        .trim()
        .trim_end_matches(['：', ':'])
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect()
}

/// The material's value for `field`. A manual field never has one.
pub(crate) fn from_source<'a>(field: &str, read: &'a Labelled) -> Option<&'a str> {
    if field_kind(field) == FieldKind::Manual {
        return None;
    }
    read.values.get(&label_key(field)).map(String::as_str)
}
