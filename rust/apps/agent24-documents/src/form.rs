//! L8-S02: a template field's kind, and which values a fill may write.
//! The material gives `from_source` only for text fields; an identity value
//! is never taken from the material, only from what the user said
//! (`user_said`): a model must not invent or copy a name, a phone number or
//! an address.

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
    "身份号码",
    "户口所在地",
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
    // r11 B1‴: the label as the material is read (whitespace out, 「姓 名」
    // 「电　话」 as forms align them); English words from the name as it is.
    let key = label_key(name);
    if MANUAL.iter().any(|k| key.contains(k)) {
        FieldKind::Manual
    } else if IDENTITY.iter().any(|k| key.contains(k))
        || words(name)
            .iter()
            .any(|w| IDENTITY_WORDS.contains(&w.as_str()))
    {
        FieldKind::Identity
    } else {
        FieldKind::Text
    }
}

/// Safe text fields of a form (R0 一致裁决, 监工 11-26): an HR form's facts
/// about the job, never about a person's identity or contact. Only these
/// come from the material (`from_source`) and are written without
/// `user_said`; any other field, whatever its spelling, is asked.
const SAFE_TEXT: &[&str] = &[
    "部门",
    "所属部门",
    "部门名称",
    "入职部门",
    "岗位",
    "岗位名称",
    "职位",
    "职务",
    "职级",
    "职称",
    "入职日期",
    "入职时间",
    "到岗日期",
    "报到日期",
    "试用期",
    "合同期限",
    "工作地点",
    "用工形式",
    "学历",
    "最高学历",
    "学位",
    "专业",
    "所学专业",
    "毕业院校",
    "毕业学校",
    "毕业时间",
    "毕业日期",
];

/// A label as the allowlist is matched: whitespace and a trailing colon out
/// (`label_key`), NFKC (「ＱＱ」→「QQ」), lower case, and no format character
/// (Cf: a zero-width space in 「姓\u{200b}名」).
pub(crate) fn safe_key(name: &str) -> String {
    use unicode_general_category::{GeneralCategory, get_general_category};
    use unicode_normalization::UnicodeNormalization;
    let k: String = label_key(name).nfkc().collect();
    label_key(&k)
        .to_lowercase()
        .chars()
        .filter(|c| get_general_category(*c) != GeneralCategory::Format)
        .collect()
}

/// The field is on the allowlist of safe text fields: read from the user's
/// material, and written without `user_said`.
pub(crate) fn from_material(name: &str) -> bool {
    field_kind(name) == FieldKind::Text && SAFE_TEXT.contains(&safe_key(name).as_str())
}

/// PR-Daemon #2100 r2 (监工): the material is plainly the user's own record:
/// read whole (no early stop, nothing ambiguous), it states a 姓名, and that
/// is the 姓名 the user said (safe_key equal). Only the name is compared: a
/// job field the user said otherwise vetoes nothing (C4).
pub(crate) fn plainly_users(
    read: &Labelled,
    values: &BTreeMap<String, String>,
    user_said: &[String],
) -> bool {
    if read.cut || read.layout_unknown || !read.ambiguous.is_empty() {
        return false;
    }
    // r3 D4: a name field of the user's own (姓名, 员工姓名, 申请人姓名…),
    // compared by the same field in the material and in what was said.
    user_said.iter().filter(|n| own_name_field(n)).any(|n| {
        let key = safe_key(n);
        let stated = read.values.iter().find(|(k, _)| safe_key(k) == key);
        match (stated, values.get(n)) {
            (Some((_, stated)), Some(said)) => safe_key(stated) == safe_key(said),
            _ => false,
        }
    })
}

/// A field for the user's own name: 「姓名」 after one of these (never
/// 紧急联系人姓名, 配偶姓名).
fn own_name_field(name: &str) -> bool {
    safe_key(name).strip_suffix("姓名").is_some_and(|who| {
        ["", "员工", "申请人", "本人", "职工", "应聘人", "入职人员"].contains(&who)
    })
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

/// A field on the safe allowlist ([`from_material`]) is written as given;
/// any other (identity, or a spelling not on the list) only when the user
/// said it (`user_said`), never because it occurs in the material (r11 B2,
/// R0 一致裁决). A name the template does not have stays in `write`, so fill
/// still refuses it.
/// `material`: with a material, whether it is plainly the user's own
/// ([`plainly_users`]); when it is not, an allowlisted field is written only
/// as said, like any other (PR-Daemon #2100 r2 C2).
pub(crate) fn plan_fill(
    fields: &[(String, FieldKind)],
    values: &BTreeMap<String, String>,
    user_said: &[String],
    material: Option<bool>,
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
        let unsaid_ok = from_material(name) && material != Some(false);
        if field_kind(name) != FieldKind::Manual && (unsaid_ok || said(name)) {
            write.insert(name.clone(), value.clone());
        }
    }
    let written: HashSet<&str> = write.keys().map(String::as_str).collect();
    let mut filled = Vec::new();
    let mut ask_user = Vec::new();
    let mut manual = Vec::new();
    let mut unfilled = Vec::new();
    for (name, kind) in fields {
        let given = values.get(name).map(|v| !v.trim().is_empty());
        match kind {
            FieldKind::Manual => manual.push(name.clone()),
            _ if written.contains(name.as_str()) => filled.push(name.clone()),
            // 「先空着」.
            _ if given == Some(false) => unfilled.push(name.clone()),
            FieldKind::Identity => ask_user.push(name.clone()),
            // A value the user did not say, for a field not on the list.
            FieldKind::Text if given == Some(true) => ask_user.push(name.clone()),
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
        s.push_str(&format!("要问用户：{}。", ask.join("、")));
    }
    if !manual.is_empty() {
        s.push_str(&format!("需要你手工处理：{}。", manual.join("、")));
    }
    if !unfilled.is_empty() {
        s.push_str(&format!("还没填：{}。", unfilled.join("、")));
    }
    s.push_str("身份类和不在常见岗位字段里的值只能用用户亲口说的（字段名放进 user_said），不要自己补，也不要从资料里抄。");
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
    /// The material does not open with the user's own fields (a heading
    /// that is not exactly the user's, or something else first): nothing is
    /// read, and the user is asked to say them.
    pub(crate) layout_unknown: bool,
    /// The reading stopped early, at a sign of another record (a header, a
    /// relation, a label stated again), not at the end of the field run:
    /// what was read may not be all of the user's (PR-Daemon #2100 r2).
    pub(crate) cut: bool,
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

/// A value as [`labelled_blocks`] reads it.
enum Take {
    Value(String),
    Spread,
    None,
}

/// The plain value read, if it is one.
fn plain(t: &Take) -> Option<String> {
    match t {
        Take::Value(v) => Some(v.clone()),
        _ => None,
    }
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

/// What a heading before the user's fields may be, once its whitespace and
/// a suffix 「信息」「资料」「情况」「表」「登记表」 are taken off: exactly one of these.
const OWN_HEADINGS: &[&str] = &[
    "个人",
    "本人",
    "基本",
    "申请人",
    "简历",
    "员工",
    "入职",
    "个人基本",
];

/// The heading is exactly the user's own (「个人信息」「个人基本信息」「入职登记表」),
/// never one with any other word (「共同申请人」「本人前雇主」).
fn own_heading(t: &str) -> bool {
    let mut k: String = t.chars().filter(|c| !c.is_whitespace()).collect();
    // r6 N1: until none is left (「个人基本情况表」→「个人基本情况」→「个人基本」).
    while let Some(stem) = ["登记表", "信息", "资料", "情况", "表"]
        .iter()
        .find_map(|suffix| k.strip_suffix(suffix))
    {
        k = stem.to_owned();
    }
    OWN_HEADINGS.contains(&k.as_str())
}

/// L8-S02, the rule 监工 set after PR-Daemon #2095 round 5 (independent of
/// any word list for others and of how a heading looks): **only the run of
/// field blocks that opens the material is read.**
/// 1. Empty blocks aside, at most one heading may come before the first
///    field block, and it must be exactly the user's own ([`own_heading`]);
///    anything else first and nothing is read (`layout_unknown`).
/// 2. From the first field block on, field blocks are read; the first block
///    that is not one ends the reading, whatever it is.
/// 3. Only the first person's block is read: the material must be one
///    record (r8, r9, 监工). A block with a sign of a second person (a
///    label stated before, unless with the same one plain value; a label
///    twice in the block; a relation field not exactly 「本人」「自己」) is
///    not read, and the reading stops there.
///
/// A field block: a line with exactly one 「字段：」 opening it (after leading
/// space and a number 「1.」「（一）」); a table row strictly 「字段 | 值 | 字段 |
/// 值…」 from its first cell. A header row (every filled cell a field) is
/// never read: its fields are ambiguous and the reading stops there (r7,
/// 监工). Only labels equal to one of `fields` (whitespace and a trailing
/// colon aside) count.
///
/// A value: with two or more spaces in a row, a tab or U+3000, ambiguous;
/// with another colon (not a time's), another field's name, or a blank
/// (「____」「□」「（请填写）」), none; in brackets, without them. A field with
/// two different values, or a strict row's value that the row under it
/// could give differently (F1), is ambiguous.
pub(crate) fn labelled_blocks(blocks: &[Block], fields: &[String]) -> Labelled {
    let names: Vec<String> = fields
        .iter()
        .map(|f| label_key(f))
        .filter(|k| !k.is_empty())
        .collect();
    let mut found: BTreeMap<String, Vec<String>> = BTreeMap::new();
    // Labels stated by the blocks read so far (r9).
    let mut seen: Vec<String> = Vec::new();
    let mut unsure: Vec<String> = Vec::new();
    // A value as read: one to keep, a spread one (ambiguous), or one that is
    // no value (a blank 「____」, another field's name).
    let take = |value: &str| -> Take {
        let value = unbracket(value.trim());
        if spread(value) {
            Take::Spread
        } else if usable(value, &names) {
            Take::Value(value.to_owned())
        } else {
            Take::None
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
    let empty = |b: &Block| match b {
        Block::Line(l) => l.trim().is_empty(),
        Block::Row(r) => r.iter().all(|c| c.trim().is_empty()),
    };
    // The one 「字段：」 opening a line, if it is a field line.
    let field_line = |line: &str| -> Option<(usize, String)> {
        let marks = markers(line, &names);
        let [(at, colon_end, name)] = marks.as_slice() else {
            return None;
        };
        numbering_only(&line[..*at]).then(|| (*colon_end, name.clone()))
    };
    let header = |row: &[String]| {
        let filled: Vec<&String> = row.iter().filter(|c| !c.trim().is_empty()).collect();
        filled.len() >= 2 && filled.iter().all(|c| is_field(c))
    };
    let strict = |row: &[String]| {
        is_field(&row[0])
            && row.iter().enumerate().all(|(i, c)| {
                if i % 2 == 0 {
                    // A trailing blank cell (merged or unused) is allowed.
                    is_field(c)
                        || (c.trim().is_empty() && row[i..].iter().all(|c| c.trim().is_empty()))
                } else {
                    c.trim().is_empty() || !label_like(c)
                }
            })
    };
    let field_block = |b: &Block| match b {
        Block::Line(l) => field_line(l).is_some(),
        Block::Row(r) => !r.is_empty() && (header(r) || strict(r)),
    };
    let mut out = Labelled::default();
    let mut at = blocks
        .iter()
        .position(|b| !empty(b))
        .unwrap_or(blocks.len());
    if at < blocks.len() && !field_block(&blocks[at]) {
        let heading = match &blocks[at] {
            Block::Line(l) => Some(l.as_str()),
            Block::Row(r) => {
                let filled: Vec<&String> = r.iter().filter(|c| !c.trim().is_empty()).collect();
                (filled.len() == 1).then(|| filled[0].as_str())
            }
        };
        if !heading.is_some_and(own_heading) {
            out.layout_unknown = true;
            return out;
        }
        at += 1;
    }
    while at < blocks.len() {
        let block = &blocks[at];
        at += 1;
        if empty(block) {
            continue;
        }
        if !field_block(block) {
            // PR-Daemon #2100 r3 D1 (监工): the rest must be one clean record
            // too, or the material is not plainly the user's: only other
            // 「标签：值」 lines of labels the template does not have (「爱好：跑步」).
            // A heading (「家属档案」), a table row or a template field again
            // is another record, maybe another person's.
            out.cut = blocks[at - 1..].iter().any(|b| match b {
                _ if empty(b) => false,
                Block::Row(_) => true,
                Block::Line(l) => {
                    let l = l.trim();
                    let Some(colon) = l.find(['：', ':']) else {
                        return true;
                    };
                    let label = l[..colon].trim();
                    label.is_empty()
                        || label.chars().count() > 12
                        || l[colon..].trim_start_matches(['：', ':']).trim().is_empty()
                        || !markers(l, &names).is_empty()
                }
            });
            break;
        }
        // What this block gives, kept apart until the block is known to be
        // the same person's (r8).
        let mut pairs: Vec<(String, String)> = Vec::new();
        let mut doubt: Vec<String> = Vec::new();
        // r9: each label this block states, with its plain value if it has
        // one (a blank, spread or unreadable value is still the label).
        let mut labels: Vec<(String, Option<String>)> = Vec::new();
        let mut give = |name: String, t: Take, pairs: &mut Vec<(String, String)>| match t {
            Take::Value(v) => pairs.push((name, v)),
            Take::Spread => doubt.push(name),
            Take::None => {}
        };
        match block {
            Block::Line(line) => {
                if let Some((colon_end, name)) = field_line(line) {
                    let value = line[colon_end..]
                        .trim()
                        .trim_end_matches(['；', ';', '，', ',', '、', '|', '。']);
                    let t = take(value);
                    labels.push((name.clone(), plain(&t)));
                    give(name, t, &mut pairs);
                }
            }
            Block::Row(row) => {
                if header(row) {
                    // r7 (监工): a header row (a row of field names only) is
                    // never read. Under it may be one row or a roster with
                    // blank or merged rows: whose the data is cannot be told.
                    // Its fields go to the user, and the reading stops.
                    for h in row.iter().filter(|h| is_field(h)) {
                        let k = label_key(h);
                        if !unsure.contains(&k) {
                            unsure.push(k);
                        }
                    }
                    out.cut = true;
                    break;
                }
                let below = match blocks.get(at) {
                    Some(Block::Row(n)) if n.len() == row.len() => Some(n),
                    _ => None,
                };
                for i in (0..row.len()).step_by(2) {
                    let name = label_key(&row[i]);
                    let Some(right) = row.get(i + 1).filter(|c| !c.trim().is_empty()) else {
                        if !name.is_empty() {
                            labels.push((name, None));
                        }
                        continue;
                    };
                    let right = take(right);
                    labels.push((name.clone(), plain(&right)));
                    if matches!(right, Take::None) {
                        // r8 B2: a blank here is not filled from the row under
                        // it (「姓名 | ____」 over 「李四 | 配偶」): ambiguous.
                        give(name, Take::Spread, &mut pairs);
                        continue;
                    }
                    give(name.clone(), right, &mut pairs);
                    // F1: the row under it could give a value here too; if
                    // it differs, which is meant cannot be told.
                    if let Some(down) = below
                        .map(|n| &n[i])
                        .filter(|c| !c.trim().is_empty() && !label_like(c))
                    {
                        give(name, take(down), &mut pairs);
                    }
                }
            }
        }
        // r8 (监工): only the first person's block is read; at a sign of a
        // second person the reading stops before this block. A sign: a
        // relation that is not the user's own (「关系：配偶」), or a field
        // with another value than the blocks before gave it (a second 「姓名」).
        // r9 (监工): the material must be one record.
        // 1. A label stated once: stated again in a later block, the reading
        //    stops before it, unless it is the same one plain value again.
        // 2. A label twice in one block: the block is not read, and stop.
        // 3. A relation field (「关系」「称谓」 in its name) not exactly
        //    「本人」「自己」 (unreadable or spread too): stop.
        let mut stop: Vec<String> = Vec::new();
        for (i, (n, v)) in labels.iter().enumerate() {
            let again = labels[..i].iter().any(|(m, _)| m == n)
                || (seen.contains(n)
                    && !v.as_ref().is_some_and(|v| {
                        found
                            .get(n)
                            .is_some_and(|f| f.as_slice() == std::slice::from_ref(v))
                    }));
            let other = (n.contains("关系") || n.contains("称谓"))
                && !v.as_deref().is_some_and(|v| ["本人", "自己"].contains(&v));
            if again || other {
                stop.push(n.clone());
            }
        }
        let clash = pairs
            .iter()
            .any(|(n, v)| found.get(n).is_some_and(|seen| !seen.contains(v)));
        if !stop.is_empty() || clash {
            out.cut = true;
            for n in stop.into_iter().chain(
                pairs
                    .iter()
                    .filter(|(n, v)| found.get(n).is_some_and(|seen| !seen.contains(v)))
                    .map(|(n, _)| n.clone()),
            ) {
                if !unsure.contains(&n) {
                    unsure.push(n);
                }
            }
            break;
        }
        seen.extend(labels.into_iter().map(|(n, _)| n));
        for n in doubt {
            if !unsure.contains(&n) {
                unsure.push(n);
            }
        }
        for (n, v) in pairs {
            let seen = found.entry(n).or_default();
            if !seen.contains(&v) {
                seen.push(v);
            }
        }
    }
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

/// A value without the brackets around it (「（李四）」 → 「李四」), or without a
/// closing one it does not open (「李四）」 → 「李四」).
fn unbracket(value: &str) -> &str {
    // r8 N1: only a pair around the whole value comes off, not
    // 「（研发部）（上海）」's first and last.
    let v = value
        .strip_prefix(['（', '('])
        .and_then(|v| v.strip_suffix(['）', ')']))
        .filter(|inner| {
            let mut depth = 0i32;
            inner.chars().all(|c| {
                match c {
                    '（' | '(' => depth += 1,
                    '）' | ')' => depth -= 1,
                    _ => {}
                }
                depth >= 0
            })
        })
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
