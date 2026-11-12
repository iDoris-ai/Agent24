//! Skill manifest parsing, segment rendering, and text-consistency checks.
//!
//! ADR-040 loader v1, slice SL-1. This crate does not read the disk, open a
//! database, call a model, or register tools.
//!
//! [`SkillManifest::content_digest`] covers the parsed name, description,
//! requires, evals path, mode, and body. The semver `version` field stays on
//! the manifest and is not an input of that digest.
//!
//! Eval checks and run injection share [`render_skill_segment`].
//! [`RENDERER_VERSION`] changes with that format. The segment is delimited so
//! the body cannot be confused with the header. Authorization still depends
//! on the K1 origin label; the delimiters only prevent that confusion.
//!
//! A case id comes from [`case_id`]: the eval name plus the case fields.
//! [`CaseSummary::line`] is the jsonl line for display and is not part of the id.
//!
//! [`Judge::Exact`] compares that whole rendered segment, header included, so
//! exact is essentially only suitable for tool-generated expects.

use std::collections::HashSet;
use std::fmt;

use serde::Serialize;
use serde::de::{self, Deserializer, MapAccess, Visitor};
use serde_json::Map;
use sha2::{Digest, Sha256};

/// UTF-8 byte cap for one `SKILL.md` and for one eval document.
pub const BODY_MAX_BYTES: usize = 32 * 1024;

/// Bump this when [`render_skill_segment`] changes its text.
pub const RENDERER_VERSION: u32 = 2;

const SEGMENT_START: &str = "<<<agent24-skill>>>";
const SEGMENT_END: &str = "<<<agent24-skill-end>>>";
/// Replacement that does not itself contain [`SEGMENT_END`].
const SEGMENT_END_ESCAPED: &str = "<<<agent24-skill-end\\>>>";

const KNOWN_FRONTMATTER: &[&str] = &[
    "description",
    "evals",
    "mode",
    "name",
    "requires",
    "version",
];

/// Capability row copied from `docs/capabilities/REGISTRY.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegisteredCapability {
    pub id: &'static str,
    pub name_zh: &'static str,
}

/// The six ids in `REGISTRY.md`. `mcp-<server>` is admitted separately.
pub const REGISTERED_CAPABILITIES: &[RegisteredCapability] = &[
    RegisteredCapability {
        id: "model-gateway",
        name_zh: "本地模型网关",
    },
    RegisteredCapability {
        id: "nostr-dm",
        name_zh: "Nostr 私信",
    },
    RegisteredCapability {
        id: "nostr-group",
        name_zh: "Nostr 群组",
    },
    RegisteredCapability {
        id: "nostr-identity",
        name_zh: "Nostr 身份",
    },
    RegisteredCapability {
        id: "nostr-relay",
        name_zh: "Nostr 中继",
    },
    RegisteredCapability {
        id: "documents",
        name_zh: "文档",
    },
];

pub fn registered_capability(id: &str) -> Option<&'static RegisteredCapability> {
    REGISTERED_CAPABILITIES.iter().find(|row| row.id == id)
}

/// `[a-z0-9][a-z0-9-]{1,31}` from `REGISTRY.md`.
pub fn is_capability_id_shape(id: &str) -> bool {
    let bytes = id.as_bytes();
    (2..=32).contains(&bytes.len())
        && bytes
            .first()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

/// `mcp-` plus a body of 1..=28 `[a-z0-9-]` characters, without a leading or trailing `-`.
pub fn is_mcp_capability_id(id: &str) -> bool {
    let Some(body) = id.strip_prefix("mcp-") else {
        return false;
    };
    let bytes = body.as_bytes();
    (1..=28).contains(&bytes.len())
        && bytes.first().is_some_and(|b| *b != b'-')
        && bytes.last().is_some_and(|b| *b != b'-')
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

fn is_admitted_capability(id: &str) -> bool {
    registered_capability(id).is_some() || is_mcp_capability_id(id)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillOrigin {
    User,
    Imported,
}

impl SkillOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Imported => "imported",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillMode {
    Manual,
}

impl SkillMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Judge {
    /// Compares the whole rendered segment, including the header and markers.
    /// Exact is essentially only suitable for tool-generated expects.
    Exact,
    Contains,
}

impl Judge {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Contains => "contains",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkillDescription {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zh: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub en: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub th: Option<String>,
}

struct DescriptionSlots {
    kind: &'static str,
    zh: String,
    en: String,
    th: String,
}

impl SkillDescription {
    /// One digest part per language. A present value is `1` plus the text; an
    /// absent value is `0`. [`digest_parts`] length-prefixes each part, so a
    /// newline inside one language cannot impersonate the next.
    fn digest_slots(&self) -> DescriptionSlots {
        if let Some(text) = &self.text {
            return DescriptionSlots {
                kind: "text",
                zh: presence(Some(text)),
                en: presence(None),
                th: presence(None),
            };
        }
        DescriptionSlots {
            kind: "langs",
            zh: presence(self.zh.as_deref()),
            en: presence(self.en.as_deref()),
            th: presence(self.th.as_deref()),
        }
    }
}

fn presence(value: Option<&str>) -> String {
    match value {
        Some(text) => format!("1{text}"),
        None => "0".to_string(),
    }
}

/// Parsed `SKILL.md`. Unknown frontmatter keys are listed in `ineffective` and dropped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkillManifest {
    pub name: String,
    /// Semver metadata. Not an input of [`Self::content_digest`].
    pub version: String,
    pub description: SkillDescription,
    pub requires: Vec<String>,
    /// Eval path. An empty string is an empty eval list (`evals: []`), not a path.
    pub evals: String,
    pub mode: SkillMode,
    /// Frontmatter keys that were dropped, sorted. Includes `allowed-tools` when present.
    pub ineffective: Vec<String>,
    /// Bytes after the closing frontmatter fence.
    pub body: String,
}

impl SkillManifest {
    pub fn content_digest(&self) -> String {
        let slots = self.description.digest_slots();
        let mut requires = self.requires.clone();
        requires.sort_unstable();
        requires.dedup();
        let requires = requires.join("\n");
        digest_parts(&[
            "skill",
            self.name.as_str(),
            slots.kind,
            slots.zh.as_str(),
            slots.en.as_str(),
            slots.th.as_str(),
            requires.as_str(),
            self.evals.as_str(),
            self.mode.as_str(),
            self.body.as_str(),
        ])
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaseSummary {
    pub id: String,
    pub judge: Judge,
    pub passed: bool,
    pub expect_digest: String,
    /// 1-based source line. Display only; not an input of [`case_id`].
    pub line: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextConsistencyReport {
    pub renderer_version: u32,
    pub segment_digest: String,
    pub cases: Vec<CaseSummary>,
}

impl TextConsistencyReport {
    pub fn failure_ids(&self) -> Vec<&str> {
        self.cases
            .iter()
            .filter(|case| !case.passed)
            .map(|case| case.id.as_str())
            .collect()
    }

    pub fn consistent_counts(&self) -> (usize, usize) {
        let passed = self.cases.iter().filter(|case| case.passed).count();
        (passed, self.cases.len())
    }
}

struct EvalCase {
    line: usize,
    input: String,
    expect: String,
    judge: Judge,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SkillError {
    #[error("skill_manifest_invalid: {detail}")]
    ManifestInvalid { detail: String },
    #[error("skill_proactive_unsupported")]
    ProactiveUnsupported,
    #[error("skill_body_too_large")]
    BodyTooLarge,
    #[error("skill_capability_unknown: {id}")]
    CapabilityUnknown { id: String },
    #[error("skill_eval_invalid: {detail}")]
    EvalInvalid { detail: String },
    #[error("skill_eval_empty")]
    EvalEmpty,
}

impl SkillError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::ManifestInvalid { .. } => "skill_manifest_invalid",
            Self::ProactiveUnsupported => "skill_proactive_unsupported",
            Self::BodyTooLarge => "skill_body_too_large",
            Self::CapabilityUnknown { .. } => "skill_capability_unknown",
            Self::EvalInvalid { .. } => "skill_eval_invalid",
            Self::EvalEmpty => "skill_eval_empty",
        }
    }

    fn manifest(detail: impl Into<String>) -> Self {
        Self::ManifestInvalid {
            detail: detail.into(),
        }
    }

    fn eval(detail: impl Into<String>) -> Self {
        Self::EvalInvalid {
            detail: detail.into(),
        }
    }
}

pub fn sha256_hex(text: &str) -> String {
    hex_encode(Sha256::digest(text.as_bytes()).as_slice())
}

fn digest_parts(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    hex_encode(hasher.finalize().as_slice())
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    out
}

/// Id of one eval case. Line numbers are not an input.
pub fn case_id(eval_name: &str, judge: &str, input: &str, expect: &str) -> String {
    digest_parts(&["case", eval_name, judge, input, expect])
}

pub fn parse_skill_markdown(markdown: &str) -> Result<SkillManifest, SkillError> {
    parse_markdown(markdown, false)
}

/// Import entry for a community single-file `SKILL.md` (SL-3).
///
/// Missing `version`, `requires`, `mode`, and `evals` are filled first
/// (`0.0.0`, `[]`, `manual`, `[]`). The filled frontmatter then uses the same
/// checks as [`parse_skill_markdown`]. A user-written skill still has to carry
/// `version`, `requires`, and `evals`; this function is the only filler.
pub fn parse_imported(markdown: &str) -> Result<SkillManifest, SkillError> {
    parse_markdown(markdown, true)
}

fn parse_markdown(markdown: &str, imported: bool) -> Result<SkillManifest, SkillError> {
    refuse_bom(markdown, false)?;
    if markdown.len() > BODY_MAX_BYTES {
        return Err(SkillError::BodyTooLarge);
    }
    let (yaml, body) = split_frontmatter(markdown)?;
    let mut value: serde_yaml::Value =
        serde_yaml::from_str(yaml).map_err(|err| SkillError::manifest(err.to_string()))?;
    if imported {
        fill_import_defaults(&mut value)?;
    }
    let Some(mapping) = value.as_mapping() else {
        return Err(SkillError::manifest("frontmatter is not a map"));
    };
    let pairs = string_keys(mapping)?;
    let mode = parse_mode(&pairs)?;
    let mut ineffective = Vec::new();
    for (key, _) in &pairs {
        if !KNOWN_FRONTMATTER.contains(key) {
            ineffective.push((*key).to_string());
        }
    }
    let name = required_string(&pairs, "name")?;
    if !is_capability_id_shape(&name) {
        return Err(SkillError::manifest("name is not a skill id"));
    }
    let version = required_string(&pairs, "version")?;
    if !is_semver(&version) {
        return Err(SkillError::manifest("version is not semver"));
    }
    let description = parse_description(lookup(&pairs, "description"), &mut ineffective)?;
    let requires = parse_requires(lookup(&pairs, "requires"))?;
    let evals = parse_evals(lookup(&pairs, "evals"))?;
    ineffective.sort_unstable();
    Ok(SkillManifest {
        name,
        version,
        description,
        requires,
        evals,
        mode,
        ineffective,
        body: body.to_string(),
    })
}

fn refuse_bom(text: &str, eval: bool) -> Result<(), SkillError> {
    if text.starts_with('\u{feff}') {
        let detail = "文件以 BOM 开头，请去掉";
        return Err(if eval {
            SkillError::eval(detail)
        } else {
            SkillError::manifest(detail)
        });
    }
    Ok(())
}

fn fill_import_defaults(value: &mut serde_yaml::Value) -> Result<(), SkillError> {
    let Some(map) = value.as_mapping_mut() else {
        return Err(SkillError::manifest("frontmatter is not a map"));
    };
    insert_missing(
        map,
        "version",
        serde_yaml::Value::String("0.1.0".to_string()),
    );
    insert_missing(map, "requires", serde_yaml::Value::Sequence(Vec::new()));
    insert_missing(map, "mode", serde_yaml::Value::String("manual".to_string()));
    insert_missing(map, "evals", serde_yaml::Value::Sequence(Vec::new()));
    Ok(())
}

fn insert_missing(map: &mut serde_yaml::Mapping, key: &str, value: serde_yaml::Value) {
    if map.keys().any(|existing| existing.as_str() == Some(key)) {
        return;
    }
    map.insert(serde_yaml::Value::String(key.to_string()), value);
}

fn split_frontmatter(markdown: &str) -> Result<(&str, &str), SkillError> {
    let rest = if let Some(rest) = markdown.strip_prefix("---\r\n") {
        rest
    } else if let Some(rest) = markdown.strip_prefix("---\n") {
        rest
    } else {
        return Err(SkillError::manifest("missing frontmatter"));
    };
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            return Ok((&rest[..offset], &rest[offset + line.len()..]));
        }
        offset += line.len();
    }
    Err(SkillError::manifest("frontmatter is not closed"))
}

fn string_keys(map: &serde_yaml::Mapping) -> Result<Vec<(&str, &serde_yaml::Value)>, SkillError> {
    let mut pairs = Vec::with_capacity(map.len());
    for (key, value) in map {
        let Some(name) = key.as_str() else {
            return Err(SkillError::manifest("frontmatter key is not a string"));
        };
        pairs.push((name, value));
    }
    Ok(pairs)
}

fn lookup<'a>(
    pairs: &[(&'a str, &'a serde_yaml::Value)],
    key: &str,
) -> Option<&'a serde_yaml::Value> {
    pairs
        .iter()
        .find(|(name, _)| *name == key)
        .map(|(_, value)| *value)
}

fn required_string(pairs: &[(&str, &serde_yaml::Value)], key: &str) -> Result<String, SkillError> {
    match lookup(pairs, key) {
        Some(value) => match value.as_str() {
            Some(text) if !text.is_empty() => Ok(text.to_string()),
            Some(_) => Err(SkillError::manifest(format!("{key} is empty"))),
            None => Err(SkillError::manifest(format!("{key} is not a string"))),
        },
        None => Err(SkillError::manifest(format!("missing {key}"))),
    }
}

fn parse_mode(pairs: &[(&str, &serde_yaml::Value)]) -> Result<SkillMode, SkillError> {
    match lookup(pairs, "mode") {
        None => Ok(SkillMode::Manual),
        Some(value) => match value.as_str() {
            Some("manual") => Ok(SkillMode::Manual),
            Some("proactive") => Err(SkillError::ProactiveUnsupported),
            _ => Err(SkillError::manifest("mode is not supported")),
        },
    }
}

fn parse_description(
    value: Option<&serde_yaml::Value>,
    ineffective: &mut Vec<String>,
) -> Result<SkillDescription, SkillError> {
    let Some(value) = value else {
        return Err(SkillError::manifest("missing description"));
    };
    match value {
        serde_yaml::Value::String(text) => {
            if text.is_empty() {
                return Err(SkillError::manifest("description is empty"));
            }
            Ok(SkillDescription {
                text: Some(text.clone()),
                zh: None,
                en: None,
                th: None,
            })
        }
        serde_yaml::Value::Mapping(map) => {
            let mut description = SkillDescription {
                text: None,
                zh: None,
                en: None,
                th: None,
            };
            for (key, item) in map {
                let Some(name) = key.as_str() else {
                    return Err(SkillError::manifest("description key is not a string"));
                };
                if name != "zh" && name != "en" && name != "th" {
                    ineffective.push(format!("description.{name}"));
                    continue;
                }
                let Some(text) = item.as_str() else {
                    return Err(SkillError::manifest("description value is not a string"));
                };
                if text.is_empty() {
                    return Err(SkillError::manifest("description value is empty"));
                }
                let slot = match name {
                    "zh" => &mut description.zh,
                    "en" => &mut description.en,
                    _ => &mut description.th,
                };
                *slot = Some(text.to_string());
            }
            if description.zh.is_none() && description.en.is_none() && description.th.is_none() {
                return Err(SkillError::manifest("description is empty"));
            }
            Ok(description)
        }
        _ => Err(SkillError::manifest("description is not a string or a map")),
    }
}

fn parse_evals(value: Option<&serde_yaml::Value>) -> Result<String, SkillError> {
    let Some(value) = value else {
        return Err(SkillError::manifest("missing evals"));
    };
    if let Some(text) = value.as_str() {
        if text.is_empty() {
            return Err(SkillError::manifest("evals is empty"));
        }
        return Ok(text.to_string());
    }
    if value.as_sequence().is_some_and(Vec::is_empty) {
        return Ok(String::new());
    }
    Err(SkillError::manifest("evals is not a string"))
}

fn parse_requires(value: Option<&serde_yaml::Value>) -> Result<Vec<String>, SkillError> {
    let Some(value) = value else {
        return Err(SkillError::manifest("missing requires"));
    };
    let Some(seq) = value.as_sequence() else {
        return Err(SkillError::manifest("requires is not a list"));
    };
    let mut out = Vec::with_capacity(seq.len());
    for item in seq {
        let Some(id) = item.as_str() else {
            return Err(SkillError::manifest("requires entry is not a string"));
        };
        if !is_admitted_capability(id) {
            return Err(SkillError::CapabilityUnknown { id: id.to_string() });
        }
        if out.iter().any(|seen| seen == id) {
            return Err(SkillError::manifest(format!("duplicate capability {id}")));
        }
        out.push(id.to_string());
    }
    Ok(out)
}

fn is_semver(input: &str) -> bool {
    if input.is_empty() {
        return false;
    }
    let (version, build) = split_once_char(input, '+');
    if let Some(build) = build
        && !dot_separated_idents(build, false)
    {
        return false;
    }
    let (core, pre) = split_once_char(version, '-');
    numeric_core(core) && pre.is_none_or(|pre| dot_separated_idents(pre, true))
}

fn split_once_char(input: &str, sep: char) -> (&str, Option<&str>) {
    match input.split_once(sep) {
        Some((left, right)) => (left, Some(right)),
        None => (input, None),
    }
}

fn numeric_core(core: &str) -> bool {
    let mut parts = core.split('.');
    let (Some(major), Some(minor), Some(patch)) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    parts.next().is_none() && is_numeric_id(major) && is_numeric_id(minor) && is_numeric_id(patch)
}

fn is_numeric_id(text: &str) -> bool {
    !text.is_empty()
        && text.bytes().all(|byte| byte.is_ascii_digit())
        && (text.len() == 1 || !text.starts_with('0'))
}

fn dot_separated_idents(text: &str, reject_numeric_leading_zero: bool) -> bool {
    if text.is_empty() {
        return false;
    }
    text.split('.').all(|ident| {
        if ident.is_empty()
            || !ident
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return false;
        }
        if reject_numeric_leading_zero
            && ident.len() > 1
            && ident.starts_with('0')
            && ident.bytes().all(|byte| byte.is_ascii_digit())
        {
            return false;
        }
        true
    })
}

/// Header plus `body`, wrapped in start and end markers.
///
/// Authorization still depends on the K1 origin label. The markers only keep
/// the body from being confused with the header. A body that contains the end
/// marker is escaped, so the closing marker appears once, at the end.
///
/// [`Judge::Exact`] compares this whole segment, header included, so exact is
/// essentially only suitable for tool-generated expects.
pub fn render_skill_segment(body: &str, origin: SkillOrigin, content_digest: &str) -> String {
    let body = escape_segment_body(body);
    format!(
        "{SEGMENT_START}\norigin: {}\ncontent_digest: {}\n\n{body}\n{SEGMENT_END}\n",
        origin.as_str(),
        content_digest
    )
}

fn escape_segment_body(body: &str) -> String {
    body.replace(SEGMENT_END, SEGMENT_END_ESCAPED)
}

pub fn check_text_consistency(
    eval_name: &str,
    eval_jsonl: &str,
    body: &str,
    origin: SkillOrigin,
    content_digest: &str,
) -> Result<TextConsistencyReport, SkillError> {
    if eval_name.is_empty() {
        return Err(SkillError::eval("missing eval name"));
    }
    if eval_jsonl.len() > BODY_MAX_BYTES {
        return Err(SkillError::BodyTooLarge);
    }
    let cases = parse_cases(eval_jsonl)?;
    if cases.is_empty() {
        return Err(SkillError::EvalEmpty);
    }
    let segment = render_skill_segment(body, origin, content_digest);
    let mut seen = HashSet::with_capacity(cases.len());
    let mut summaries = Vec::with_capacity(cases.len());
    for case in &cases {
        let id = case_id(eval_name, case.judge.as_str(), &case.input, &case.expect);
        if !seen.insert(id.clone()) {
            return Err(SkillError::eval(format!(
                "duplicate eval case on line {}",
                case.line
            )));
        }
        summaries.push(CaseSummary {
            id,
            judge: case.judge,
            passed: case_passes(&segment, case),
            expect_digest: sha256_hex(&case.expect),
            line: case.line,
        });
    }
    Ok(TextConsistencyReport {
        renderer_version: RENDERER_VERSION,
        segment_digest: sha256_hex(&segment),
        cases: summaries,
    })
}

fn parse_cases(eval_jsonl: &str) -> Result<Vec<EvalCase>, SkillError> {
    refuse_bom(eval_jsonl, true)?;
    let mut cases = Vec::new();
    for (index, line) in eval_jsonl.lines().enumerate() {
        let line_no = index + 1;
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        cases.push(parse_case(line_no, line)?);
    }
    Ok(cases)
}

fn parse_case(line_no: usize, line: &str) -> Result<EvalCase, SkillError> {
    let obj = parse_json_object(line)?;
    for key in obj.keys() {
        if key != "input" && key != "expect" && key != "judge" {
            return Err(SkillError::eval(format!("unknown eval field {key}")));
        }
    }
    let input = json_string(&obj, "input")?;
    let expect = json_string(&obj, "expect")?;
    let judge_text = json_string(&obj, "judge")?;
    if expect.is_empty() {
        return Err(SkillError::eval("expect is empty"));
    }
    let judge = match judge_text.as_str() {
        "exact" => Judge::Exact,
        "contains" => Judge::Contains,
        "local_model" => return Err(SkillError::eval("local_model is not in v1")),
        _ => return Err(SkillError::eval("judge is not supported")),
    };
    Ok(EvalCase {
        line: line_no,
        input,
        expect,
        judge,
    })
}

fn parse_json_object(line: &str) -> Result<Map<String, serde_json::Value>, SkillError> {
    struct ObjVisitor;
    impl<'de> Visitor<'de> for ObjVisitor {
        type Value = Map<String, serde_json::Value>;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a json object")
        }

        fn visit_map<A>(self, mut access: A) -> Result<Self::Value, A::Error>
        where
            A: MapAccess<'de>,
        {
            let mut map = Map::new();
            while let Some(key) = access.next_key::<String>()? {
                if map.contains_key(&key) {
                    return Err(de::Error::custom(format!("duplicate key {key}")));
                }
                let value = access.next_value()?;
                map.insert(key, value);
            }
            Ok(map)
        }
    }

    let mut deserializer = serde_json::Deserializer::from_str(line);
    let map =
        Deserializer::deserialize_any(&mut deserializer, ObjVisitor).map_err(eval_json_error)?;
    deserializer
        .end()
        .map_err(|_| SkillError::eval("eval line is not json"))?;
    Ok(map)
}

fn eval_json_error(err: serde_json::Error) -> SkillError {
    let message = err.to_string();
    let head = match message.split_once(" at line ") {
        Some((head, _)) => head,
        None => message.as_str(),
    };
    if let Some(key) = head.strip_prefix("duplicate key ") {
        return SkillError::eval(format!("duplicate key {key}"));
    }
    if message.contains("invalid type") {
        return SkillError::eval("eval line is not an object");
    }
    SkillError::eval("eval line is not json")
}

fn json_string(obj: &Map<String, serde_json::Value>, key: &str) -> Result<String, SkillError> {
    match obj.get(key) {
        Some(serde_json::Value::String(text)) => Ok(text.clone()),
        Some(_) => Err(SkillError::eval(format!("{key} is not a string"))),
        None => Err(SkillError::eval(format!("missing {key}"))),
    }
}

fn case_passes(segment: &str, case: &EvalCase) -> bool {
    match case.judge {
        Judge::Exact => segment == case.expect,
        Judge::Contains => segment.contains(&case.expect),
    }
}

#[cfg(test)]
mod tests;
