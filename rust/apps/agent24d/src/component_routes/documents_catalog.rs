//! Documents' D9 catalog (ADR-038 C6).
//!
//! Documents is built with the daemon (D1: no lock until it is published on its
//! own). D9 says that catalog is compiled into the binary. It lives next to the
//! module (`agent24-documents/component.json`) and this file compiles it in:
//! `domain-os.yml` is the os-proto handshake manifest, parsed with
//! `deny_unknown_fields`, and the daemon cannot read it when the package is not
//! mounted. `kernel_capabilities` there is `requires.kernel`, which B1 split
//! from `provides`.
//!
//! One service capability (ADR-039 §2.2): the seven module tools are the
//! capability's tools, not seven capabilities. ADR-DOC-01 / DOC-1-06b name them.

use std::collections::BTreeSet;

use serde::Deserialize;
use serde_json::{Value, json};

use super::Provide;
use crate::component_manifest::CapProvide;

/// Kernel grants a domain OS may ask for (ADR-038 D2, `requires.kernel`).
const KERNEL: &[&str] = &[
    "approval",
    "events",
    "memory",
    "models",
    "policy",
    "scheduler",
];

#[derive(Debug, thiserror::Error)]
pub(super) enum CatalogError {
    #[error("documents component catalog is missing")]
    Missing,
    #[error("documents component catalog is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("documents component catalog field {0} is invalid")]
    Invalid(&'static str),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DocumentsCatalog {
    pub(super) id: String,
    pub(super) version: String,
    pub(super) name: Localized,
    provides: Vec<ProvideDecl>,
    pub(super) requires: Requires,
    pub(super) assurance: Vec<Assurance>,
    skills: Vec<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Localized {
    pub(super) zh: String,
    pub(super) en: String,
    pub(super) th: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProvideDecl {
    id: String,
    name: Localized,
    summary: Localized,
    tools: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Requires {
    pub(super) kernel: Vec<String>,
    pub(super) operations: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Assurance {
    pub(super) id: String,
    pub(super) source: String,
}

/// What [`DocumentsCatalog::check`] found.
#[derive(Debug, Default)]
pub(super) struct Checked {
    /// A capability the manifest declares lacks a tool the catalog expects:
    /// `component_contract_mismatch` (ADR-038 D9).
    pub(super) mismatch: Option<ToolsMismatch>,
    /// `(expected, actual)` when the running version is not the catalog's.
    /// Not a contract mismatch: a package that is downloaded on demand can be
    /// newer or older than this daemon, so this only says an update is due.
    pub(super) version_drift: Option<(String, String)>,
    /// Capability ids the catalog expects and the manifest's list omits.
    pub(super) unavailable: BTreeSet<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct ToolsMismatch {
    capability: String,
    expected: Vec<String>,
    actual: Vec<String>,
    missing: Vec<String>,
}

impl ToolsMismatch {
    /// `expected` / `actual` as D3 wants them in the error, plus the kind.
    pub(super) fn into_detail(self) -> serde_json::Map<String, Value> {
        let value = json!({
            "expected": self.expected, "actual": self.actual, "kind": "tools",
            "capability": self.capability, "missing": self.missing,
        });
        match value {
            Value::Object(map) => map,
            _ => serde_json::Map::new(),
        }
    }
}

fn require(ok: bool, field: &'static str) -> Result<(), CatalogError> {
    ok.then_some(()).ok_or(CatalogError::Invalid(field))
}

/// D9 capability id: `[a-z0-9][a-z0-9-]{1,31}`.
fn capability_id(id: &str) -> bool {
    let [first, rest @ ..] = id.as_bytes() else {
        return false;
    };
    (1..=31).contains(&rest.len())
        && (first.is_ascii_lowercase() || first.is_ascii_digit())
        && rest
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// ADR-K1-01 operation name: `[a-z0-9][a-z0-9_-]*`, no dot.
fn operation_ok(op: &str) -> bool {
    let [first, rest @ ..] = op.as_bytes() else {
        return false;
    };
    op.len() <= 64
        && (first.is_ascii_lowercase() || first.is_ascii_digit())
        && rest
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-' || *c == b'_')
}

/// `<component id>.<operation>`, one dot (ADR-038 D9, ADR-K1-01).
fn tool_name(component: &str, tool: &str) -> bool {
    match tool.split_once('.') {
        Some((head, op)) => head == component && !op.contains('.') && operation_ok(op),
        None => false,
    }
}

fn text_ok(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

fn version_ok(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b))
}

impl Localized {
    fn check(&self, field: &'static str) -> Result<(), CatalogError> {
        require(
            text_ok(&self.zh, 400) && text_ok(&self.en, 400) && text_ok(&self.th, 400),
            field,
        )
    }

    fn wire(&self) -> Value {
        json!({"zh": self.zh, "en": self.en, "th": self.th})
    }
}

impl DocumentsCatalog {
    /// `None` and blank text are a missing catalog. Anything else is parsed.
    /// Neither path panics: a bad catalog is [`CatalogError`].
    pub(super) fn load(text: Option<&str>) -> Result<Self, CatalogError> {
        let Some(text) = text.map(str::trim).filter(|s| !s.is_empty()) else {
            return Err(CatalogError::Missing);
        };
        let catalog: Self = serde_json::from_str(text)?;
        catalog.validate()?;
        Ok(catalog)
    }

    pub(super) fn embedded() -> Result<Self, CatalogError> {
        Self::load(Some(include_str!(
            "../../../agent24-documents/component.json"
        )))
    }

    fn validate(&self) -> Result<(), CatalogError> {
        require(self.id == "documents", "id")?;
        require(version_ok(&self.version), "version")?;
        self.name.check("name")?;
        require(!self.provides.is_empty(), "provides")?;
        let mut ids = BTreeSet::new();
        let mut tools = BTreeSet::new();
        for item in &self.provides {
            require(
                capability_id(&item.id) && ids.insert(item.id.as_str()),
                "provides.id",
            )?;
            item.name.check("provides.name")?;
            item.summary.check("provides.summary")?;
            require(!item.tools.is_empty(), "provides.tools")?;
            for tool in &item.tools {
                require(
                    tool_name(&self.id, tool) && tools.insert(tool.as_str()),
                    "provides.tools",
                )?;
            }
        }
        let mut kernel = BTreeSet::new();
        for name in &self.requires.kernel {
            require(
                KERNEL.contains(&name.as_str()) && kernel.insert(name.as_str()),
                "requires.kernel",
            )?;
        }
        let mut ops = BTreeSet::new();
        for op in &self.requires.operations {
            require(
                operation_ok(op) && ops.insert(op.as_str()),
                "requires.operations",
            )?;
        }
        // D9: skills stay an empty placeholder until ADR-040.
        require(self.skills.is_empty(), "skills")?;
        for level in &self.assurance {
            require(
                text_ok(&level.id, 32) && text_ok(&level.source, 512),
                "assurance",
            )?;
        }
        Ok(())
    }

    /// D9 expected-vs-actual. `runtime_version` is the mount report's,
    /// `actual` the manifest's capability list (`None` when no manifest was
    /// read; an empty list means the package declared none, which says nothing
    /// about what it provides, so nothing is checked against it).
    pub(super) fn check(
        &self,
        runtime_version: Option<&str>,
        actual: Option<&[CapProvide]>,
    ) -> Checked {
        let mut checked = Checked::default();
        if let Some(runtime) = runtime_version.filter(|runtime| *runtime != self.version) {
            checked.version_drift = Some((self.version.clone(), runtime.to_owned()));
        }
        let Some(actual) = actual.filter(|actual| !actual.is_empty()) else {
            return checked;
        };
        for expected in &self.provides {
            let Some(found) = actual.iter().find(|cap| cap.id == expected.id) else {
                checked.unavailable.insert(expected.id.clone());
                continue;
            };
            let missing: Vec<String> = expected
                .tools
                .iter()
                .filter(|tool| !found.tools.contains(tool))
                .cloned()
                .collect();
            if !missing.is_empty() && checked.mismatch.is_none() {
                checked.mismatch = Some(ToolsMismatch {
                    capability: expected.id.clone(),
                    expected: expected.tools.clone(),
                    actual: found.tools.clone(),
                    missing,
                });
            }
        }
        checked
    }

    #[cfg(test)]
    pub(super) fn tool_names(&self) -> Vec<&str> {
        self.provides
            .iter()
            .flat_map(|item| item.tools.iter().map(String::as_str))
            .collect()
    }

    pub(super) fn into_provides(self) -> Vec<Provide> {
        self.provides
            .into_iter()
            .map(|item| Provide {
                id: item.id,
                name: Some(item.name.wire()),
                summary: Some(item.summary.wire()),
                tools: item.tools,
                availability: None,
            })
            .collect()
    }
}
