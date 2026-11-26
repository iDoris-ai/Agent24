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

/// Only labels equal to one of `fields` (whitespace and a trailing colon
/// aside) are read. Lines: 「字段：值」, cut only where the next 「字段：」 starts;
/// a value with another colon (not a time's), another field's name, or a
/// blank (「____」「□」「（请填写）」) is none. Tables: a field's cell with the
/// value to its right (when that cell is not a label), or under it (when its
/// row is all labels); both possible and different is none. A field found
/// with two different values is ambiguous.
pub(crate) fn labelled(lines: &[String], rows: &[Vec<String>], fields: &[String]) -> Labelled {
    let names: Vec<String> = fields
        .iter()
        .map(|f| label_key(f))
        .filter(|k| !k.is_empty())
        .collect();
    let mut found: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut put = |name: &str, value: &str| {
        let value = value.trim();
        if usable(value, &names) {
            let seen = found.entry(name.to_owned()).or_default();
            if !seen.iter().any(|v| v == value) {
                seen.push(value.to_owned());
            }
        }
    };
    for line in lines.iter().flat_map(|l| l.lines()) {
        let marks = markers(line, &names);
        // PR-Daemon #2095 r2 F2: the first 「字段：」 opens the line; one after
        // other words (「紧急联系人 姓名：李四」「…（姓名：李四」) is someone else's.
        if marks.first().is_none_or(|m| !line[..m.0].trim().is_empty()) {
            continue;
        }
        for (i, (_, colon_end, name)) in marks.iter().enumerate() {
            let end = marks.get(i + 1).map_or(line.len(), |m| m.0);
            let value = line[*colon_end..end].trim_start();
            // F4: a value ends at two spaces, a tab or U+3000.
            let value = value
                .find(['\t', '\u{3000}'])
                .into_iter()
                .chain(value.find("  "))
                .min()
                .map_or(value, |cut| &value[..cut]);
            let value = value
                .trim()
                .trim_end_matches(['；', ';', '，', ',', '、', '|']);
            put(name, value);
        }
    }
    let is_field = |t: &str| names.contains(&label_key(t));
    let label_like = |t: &str| {
        let t = t.trim();
        is_field(t)
            || t.ends_with(['：', ':'])
            || field_kind(t) != FieldKind::Text
            || COMMON_LABELS.contains(&label_key(t).as_str())
    };
    // F2: a row of one cell that is not a field (「紧急联系人」) heads a section
    // of someone else's: its next row is not read.
    let section = |row: &[String]| {
        let filled: Vec<&String> = row.iter().filter(|c| !c.trim().is_empty()).collect();
        filled.len() == 1 && !is_field(filled[0])
    };
    for (r, row) in rows.iter().enumerate() {
        if r > 0 && section(&rows[r - 1]) {
            continue;
        }
        let header = row.iter().all(|c| c.trim().is_empty() || label_like(c));
        let below = rows.get(r + 1).filter(|n| n.len() == row.len());
        for (i, cell) in row.iter().enumerate() {
            if !is_field(cell) {
                continue;
            }
            // F2: 「配偶 | 姓名 | 李四」: a field is the user's only first in its
            // row, or right after another field's value (「姓名 | 陈晓明 | 性别 | 男」).
            if !header && i > 0 && !(i >= 2 && is_field(&row[i - 2])) {
                continue;
            }
            let right = row
                .get(i + 1)
                .filter(|c| !c.trim().is_empty() && !label_like(c));
            // F1: the cell below is always weighed, whatever the header words
            // are (no list knows them all); right and down disagreeing is ambiguous.
            let down = below
                .map(|n| &n[i])
                .filter(|c| !c.trim().is_empty() && !label_like(c));
            match (right, down) {
                (Some(a), Some(b)) if a.trim() != b.trim() => {
                    put(&label_key(cell), a);
                    put(&label_key(cell), b);
                }
                (Some(v), _) | (None, Some(v)) => put(&label_key(cell), v),
                (None, None) => {}
            }
        }
    }
    let mut out = Labelled::default();
    for (name, values) in found {
        if values.len() == 1 {
            out.values
                .insert(name, values.into_iter().next().unwrap_or_default());
        } else {
            out.ambiguous.push(name);
        }
    }
    out
}

/// Where each 「字段：」 of `line` starts and its colon ends, in order: a field
/// name at the start or after a space or separator, then a colon.
fn markers(line: &str, names: &[String]) -> Vec<(usize, usize, String)> {
    let mut marks: Vec<(usize, usize, String)> = Vec::new();
    for name in names {
        for (at, _) in line.match_indices(name.as_str()) {
            let before = line[..at].chars().next_back();
            if before.is_some_and(|c| !c.is_whitespace() && !"；;，,、|（(）)".contains(c)) {
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
