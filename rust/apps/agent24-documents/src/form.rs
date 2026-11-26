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

/// L8-S02 round 2 (监工 11-26 真机): the values the user's material states
/// by label, 「姓名：陈晓明」 in a paragraph or a line, or 「姓名 | 陈晓明」 in two
/// cells side by side. A label is at most 32 characters; a value that is
/// empty or still a `{{field}}` is not one. First found wins.
pub(crate) fn labelled(lines: &[String], rows: &[Vec<String>]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut put = |label: &str, value: &str| {
        let label = label_key(label);
        let value = value.trim();
        if label.is_empty()
            || label.chars().count() > 32
            || value.is_empty()
            || value.contains("{{")
            || out.iter().any(|(l, _)| *l == label)
        {
            return;
        }
        out.push((label, value.to_owned()));
    };
    for line in lines.iter().flat_map(|l| l.lines()) {
        if let Some(at) = line.find(['：', ':']) {
            let sep = line[at..].chars().next().map_or(1, char::len_utf8);
            put(&line[..at], &line[at + sep..]);
        }
    }
    for row in rows {
        for pair in row.windows(2) {
            // A label cell holds no 「：值」 of its own (that is a line, above).
            if !pair[0].trim_end_matches(['：', ':']).contains(['：', ':']) {
                put(&pair[0], &pair[1]);
            }
        }
    }
    out
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

/// The material's value for `field`: the label equal to its name (whitespace
/// aside). A manual field never has one.
pub(crate) fn from_source<'a>(field: &str, pairs: &'a [(String, String)]) -> Option<&'a str> {
    if field_kind(field) == FieldKind::Manual {
        return None;
    }
    let key = label_key(field);
    pairs
        .iter()
        .find(|(l, _)| *l == key)
        .map(|(_, v)| v.as_str())
}
