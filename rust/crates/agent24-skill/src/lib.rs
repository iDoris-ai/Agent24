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
//! [`RENDERER_VERSION`] changes with that format.
//!
//! A case id comes from [`case_id`]: the eval name plus the case fields.
//! The jsonl line number is not part of the id.

use serde::Serialize;
use serde_json::Map;
use sha2::{Digest, Sha256};

/// UTF-8 byte cap for one `SKILL.md` and for one eval document.
pub const BODY_MAX_BYTES: usize = 32 * 1024;

/// Bump this when [`render_skill_segment`] changes its text.
pub const RENDERER_VERSION: u32 = 1;

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

impl SkillDescription {
    fn canonical(&self) -> String {
        if let Some(text) = &self.text {
            return format!("text:{text}");
        }
        format!(
            "zh:{}\nen:{}\nth:{}",
            self.zh.as_deref().unwrap_or(""),
            self.en.as_deref().unwrap_or(""),
            self.th.as_deref().unwrap_or("")
        )
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
    pub evals: String,
    pub mode: SkillMode,
    /// Frontmatter keys that were dropped, sorted. Includes `allowed-tools` when present.
    pub ineffective: Vec<String>,
    /// Bytes after the closing frontmatter fence.
    pub body: String,
}

impl SkillManifest {
    pub fn content_digest(&self) -> String {
        let description = self.description.canonical();
        let requires = self.requires.join("\n");
        digest_parts(&[
            "skill",
            self.name.as_str(),
            description.as_str(),
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
    if markdown.len() > BODY_MAX_BYTES {
        return Err(SkillError::BodyTooLarge);
    }
    let (yaml, body) = split_frontmatter(markdown)?;
    let value: serde_yaml::Value =
        serde_yaml::from_str(yaml).map_err(|err| SkillError::manifest(err.to_string()))?;
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
    let evals = required_string(&pairs, "evals")?;
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

/// Header plus `body`. Eval checks and run injection both call this.
pub fn render_skill_segment(body: &str, origin: SkillOrigin, content_digest: &str) -> String {
    format!(
        "origin: {}\ncontent_digest: {}\n\n{body}",
        origin.as_str(),
        content_digest
    )
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
    let summaries = cases
        .iter()
        .map(|case| CaseSummary {
            id: case_id(eval_name, case.judge.as_str(), &case.input, &case.expect),
            judge: case.judge,
            passed: case_passes(&segment, case),
            expect_digest: sha256_hex(&case.expect),
        })
        .collect();
    Ok(TextConsistencyReport {
        renderer_version: RENDERER_VERSION,
        segment_digest: sha256_hex(&segment),
        cases: summaries,
    })
}

fn parse_cases(eval_jsonl: &str) -> Result<Vec<EvalCase>, SkillError> {
    let mut cases = Vec::new();
    for line in eval_jsonl.lines() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        cases.push(parse_case(line)?);
    }
    Ok(cases)
}

fn parse_case(line: &str) -> Result<EvalCase, SkillError> {
    let value: serde_json::Value =
        serde_json::from_str(line).map_err(|_| SkillError::eval("eval line is not json"))?;
    let Some(obj) = value.as_object() else {
        return Err(SkillError::eval("eval line is not an object"));
    };
    for key in obj.keys() {
        if key != "input" && key != "expect" && key != "judge" {
            return Err(SkillError::eval(format!("unknown eval field {key}")));
        }
    }
    let input = json_string(obj, "input")?;
    let expect = json_string(obj, "expect")?;
    let judge_text = json_string(obj, "judge")?;
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
        input,
        expect,
        judge,
    })
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
