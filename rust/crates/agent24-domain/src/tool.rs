//! K1-5.1 — module-declared Agent tools: manifest schema, validation, and the
//! host-side registration directory (ADR-K1-01 §2.1/§2.2).
//!
//! # What this slice is, and what it deliberately is NOT
//!
//! A manifest may declare a limited list of tool operations. [`validate_tools`]
//! turns the raw, untrusted list into [`ToolDeclaration`]s — rejecting the
//! WHOLE manifest on the first invalid operation name, duplicate operation,
//! unplausible input schema, unknown risk/privacy enum, or inconsistent
//! `timeout_ms`/`inline_wait_ms` pair. [`ModuleToolRegistry`] then namespaces
//! each validated tool as `<module>.<operation>` and refuses — atomically,
//! without touching what is already there — any registration that would
//! collide with an existing entry.
//!
//! This is registration only. The registry here is **not wired to the agent
//! loop**: nothing in this module advertises a tool to a model, and nothing
//! provides a call path to invoke one. Those are K1-5.2 (discovery/
//! announcement) and K1-5.3 (the call path) respectively — ADR-K1-01 §0/§2.2.
//!
//! `risk` and `output_privacy` below are **self-reported by the module and
//! recorded only as a declaration**. The host determines a tool's actual risk
//! tier at K1-6a; nothing in this crate treats a module's own `risk` or
//! `output_privacy` value as an authorization, a grant, or a reason to skip
//! confirmation (ADR-K1-01 §2.1, §5(1)).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{DomainError, is_valid_module_name};

/// Conservative sanity ceiling on a declared `timeout_ms`: ten minutes. The
/// ADR leaves the exact operative limit to the host, which "can shorten, never
/// lengthen" it (§2.4) — this bounds the DECLARATION itself, so a manifest
/// cannot claim a single tool call may run unboundedly long.
pub const MAX_DECLARED_TIMEOUT_MS: u64 = 10 * 60 * 1000;

/// Self-reported risk tier for a module-declared tool operation.
///
/// A DECLARATION, not an authority (ADR-K1-01 §2.1, §5(1)): the host decides
/// a tool's real risk at K1-6a, and an unconfirmed third-party module
/// reporting `Read` must not be treated as proof the operation is low-risk.
/// `non_exhaustive` so a future tier does not silently break an exhaustive
/// match outside this crate (mirrors [`crate::Capability`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DeclaredRisk {
    Read,
    Write,
    Destructive,
}

impl DeclaredRisk {
    pub const ALL: &'static [Self] = &[Self::Read, Self::Write, Self::Destructive];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Destructive => "destructive",
        }
    }

    /// Names the rejected string and the choices, like [`crate::Capability::parse`]:
    /// a serde variant error cannot recover what the manifest actually typed.
    pub fn parse(s: &str) -> Result<Self, String> {
        Self::ALL
            .iter()
            .copied()
            .find(|r| r.as_str() == s)
            .ok_or_else(|| {
                format!(
                    "risk: {s:?} is not one of {}",
                    Self::ALL
                        .iter()
                        .map(|r| r.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
    }
}

/// Self-reported output-data sensitivity for a module-declared tool.
///
/// Also a DECLARATION, not a grant (ADR-K1-01 §2.1): `local_only` does not by
/// itself authorize or restrict anything, and widens or narrows no egress
/// decision — that execution gate belongs to ADR-K1-02/K1-6b. Recorded here
/// so the host has it on hand once that wiring lands; unenforced in this
/// slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DeclaredOutputPrivacy {
    LocalOnly,
    RemoteAllowed,
}

impl DeclaredOutputPrivacy {
    pub const ALL: &'static [Self] = &[Self::LocalOnly, Self::RemoteAllowed];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::LocalOnly => "local_only",
            Self::RemoteAllowed => "remote_allowed",
        }
    }

    pub fn parse(s: &str) -> Result<Self, String> {
        Self::ALL
            .iter()
            .copied()
            .find(|p| p.as_str() == s)
            .ok_or_else(|| {
                format!(
                    "output_privacy: {s:?} is not one of {}",
                    Self::ALL
                        .iter()
                        .map(|p| p.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
    }
}

/// Whether `op` is usable as the right-hand side of `<module>.<operation>`.
///
/// Reuses [`crate::is_valid_module_name`]'s charset/length rule rather than
/// redefining it: bounded ASCII `[a-z0-9][a-z0-9_-]*`. That charset excludes
/// `.`, which is what keeps `<module>.<operation>` splittable on the first dot
/// without an escaping scheme — an operation name is not itself a namespace
/// (same reasoning as [`crate::is_valid_host_command_name`] for host commands).
pub fn is_valid_operation_name(op: &str) -> bool {
    is_valid_module_name(op)
}

/// Whether `schema` is plausible as an input JSON Schema for a tool call.
///
/// Deliberately narrow: this crate does not implement JSON Schema, and
/// `input_schema` is only ever used for input validation / model description,
/// never for authorization (ADR-K1-01 §2.1). It requires an object — tool-call
/// arguments are a JSON object in every tool-calling convention this host
/// speaks — and if a top-level `type` is present, that it says so explicitly:
/// `"type": "string"` on a tool's INPUT schema is not a shape any caller here
/// could ever satisfy.
fn is_plausible_input_schema(schema: &serde_json::Value) -> bool {
    let Some(obj) = schema.as_object() else {
        return false;
    };
    match obj.get("type") {
        None => true,
        Some(serde_json::Value::String(t)) => t == "object",
        Some(_) => false,
    }
}

/// The wire shape of one `tools[]` entry in `domain-os.yml`. Collected raw and
/// validated afterwards by [`validate_tools`] — same reason [`RawManifest`]
/// collects `kernel_capabilities` as strings: a bad entry must be able to name
/// itself in the error, which a strict-enum serde error cannot do.
///
/// [`RawManifest`]: crate::RawManifest
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawToolDeclaration {
    operation: String,
    description: String,
    input_schema: serde_json::Value,
    risk: String,
    output_privacy: String,
    timeout_ms: u64,
    inline_wait_ms: u64,
}

/// One module-declared tool operation, VALIDATED.
///
/// No public fields and no `Deserialize` — same reason as
/// [`crate::DomainOsManifest`]: the only way to produce one is
/// [`validate_tools`], so a caller holding a `&[ToolDeclaration]` never has to
/// re-check it for a valid operation name, a plausible schema, or a known
/// risk/privacy enum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolDeclaration {
    operation: String,
    description: String,
    input_schema: serde_json::Value,
    risk: DeclaredRisk,
    output_privacy: DeclaredOutputPrivacy,
    timeout_ms: u64,
    inline_wait_ms: u64,
}

impl ToolDeclaration {
    pub fn operation(&self) -> &str {
        &self.operation
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn input_schema(&self) -> &serde_json::Value {
        &self.input_schema
    }

    /// Self-reported; see this module's docs — not an authority.
    pub fn risk(&self) -> DeclaredRisk {
        self.risk
    }

    /// Self-reported; see this module's docs — not a grant.
    pub fn output_privacy(&self) -> DeclaredOutputPrivacy {
        self.output_privacy
    }

    /// Hard per-call ceiling the module declared. ADR-K1-01 §2.4: a host
    /// config may shorten this; this crate does not implement shortening it.
    pub fn timeout_ms(&self) -> u64 {
        self.timeout_ms
    }

    /// Always strictly less than [`timeout_ms`](Self::timeout_ms) — enforced
    /// by [`validate_tools`], never by the caller.
    pub fn inline_wait_ms(&self) -> u64 {
        self.inline_wait_ms
    }
}

/// Validate a manifest's declared tools (ADR-K1-01 §2.1, §2.2, §5(2)).
///
/// All-or-nothing, like [`crate::DomainOsManifest::from_yaml`] itself: the
/// first invalid operation name, duplicate operation, implausible schema,
/// unknown risk/privacy enum, or inconsistent `timeout_ms`/`inline_wait_ms`
/// pair fails the WHOLE manifest. A manifest that declares no `tools` at all
/// (every manifest written before this field existed) validates to an empty
/// list, never an error.
pub(crate) fn validate_tools(
    raw: Vec<RawToolDeclaration>,
) -> Result<Vec<ToolDeclaration>, DomainError> {
    let mut seen_operations: BTreeSet<String> = BTreeSet::new();
    let mut out = Vec::with_capacity(raw.len());
    for r in raw {
        if !is_valid_operation_name(&r.operation) {
            return Err(DomainError::Manifest(format!(
                "tools: invalid operation name {:?} (expected bounded ASCII \
                 [a-z0-9][a-z0-9_-]*, no dots)",
                r.operation
            )));
        }
        if !seen_operations.insert(r.operation.clone()) {
            return Err(DomainError::Manifest(format!(
                "tools: operation {:?} is declared more than once in this manifest",
                r.operation
            )));
        }
        if r.description.trim().is_empty() {
            return Err(DomainError::Manifest(format!(
                "tools.{:?}: description must not be empty",
                r.operation
            )));
        }
        if !is_plausible_input_schema(&r.input_schema) {
            return Err(DomainError::Manifest(format!(
                "tools.{:?}: input_schema must be a JSON object, and if it declares \
                 a top-level `type` that type must be \"object\"",
                r.operation
            )));
        }
        let risk = DeclaredRisk::parse(&r.risk)
            .map_err(|e| DomainError::Manifest(format!("tools.{:?}: {e}", r.operation)))?;
        let output_privacy = DeclaredOutputPrivacy::parse(&r.output_privacy)
            .map_err(|e| DomainError::Manifest(format!("tools.{:?}: {e}", r.operation)))?;
        if r.timeout_ms == 0 {
            return Err(DomainError::Manifest(format!(
                "tools.{:?}: timeout_ms must be greater than zero",
                r.operation
            )));
        }
        if r.timeout_ms > MAX_DECLARED_TIMEOUT_MS {
            return Err(DomainError::Manifest(format!(
                "tools.{:?}: timeout_ms {} exceeds the {} ms ceiling",
                r.operation, r.timeout_ms, MAX_DECLARED_TIMEOUT_MS
            )));
        }
        if r.inline_wait_ms >= r.timeout_ms {
            return Err(DomainError::Manifest(format!(
                "tools.{:?}: inline_wait_ms ({}) must be strictly less than timeout_ms ({})",
                r.operation, r.inline_wait_ms, r.timeout_ms
            )));
        }
        out.push(ToolDeclaration {
            operation: r.operation,
            description: r.description,
            input_schema: r.input_schema,
            risk,
            output_privacy,
            timeout_ms: r.timeout_ms,
            inline_wait_ms: r.inline_wait_ms,
        });
    }
    Ok(out)
}

/// One registered tool, as the host-side registry holds it: the validated
/// declaration plus which module claimed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredTool {
    module: String,
    declaration: ToolDeclaration,
}

impl RegisteredTool {
    pub fn module(&self) -> &str {
        &self.module
    }

    pub fn declaration(&self) -> &ToolDeclaration {
        &self.declaration
    }
}

/// The host-side, READ-ONLY directory of registered module tools
/// (ADR-K1-01 §2.1, §2.2, §5(2)).
///
/// Read-only in the sense that matters: there is no public method that
/// removes or overwrites an entry. [`register_module`](Self::register_module)
/// is the only way in, and it refuses — atomically, leaving the registry
/// exactly as it was — rather than overwrite anything already registered.
///
/// **Not communicated to the agent, and provides no invocation path.** That
/// is K1-5.2 (discovery/announcement) and K1-5.3 (the call path); this type
/// offers neither.
#[derive(Debug, Clone, Default)]
pub struct ModuleToolRegistry {
    tools: BTreeMap<String, RegisteredTool>,
}

impl ModuleToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// `<module>.<operation>` — the stable, namespaced tool identity
    /// (ADR-K1-01 §2.1). Public so a caller looking an entry up derives the
    /// same key this type uses internally, rather than re-implementing the
    /// join and risking it drift (e.g. a different separator).
    pub fn full_name(module: &str, operation: &str) -> String {
        format!("{module}.{operation}")
    }

    /// Register every tool `module` declared, or none of them.
    ///
    /// Atomic and fail-closed (ADR-K1-01 §5(2)): if ANY of `module`'s tools
    /// would collide with an already-registered full name — including one
    /// registered under a DIFFERENT module id, which happens if two
    /// discovered packages declare the same module name — the whole call is
    /// refused and the registry is left exactly as it was. Never partially
    /// registers a module's tools, and never overwrites an existing entry.
    /// An empty `tools` slice always succeeds as a no-op (a module that
    /// declares no tools has nothing to register).
    pub fn register_module(
        &mut self,
        module: &str,
        tools: &[ToolDeclaration],
    ) -> Result<(), String> {
        let mut conflicts = Vec::new();
        for t in tools {
            let name = Self::full_name(module, t.operation());
            if self.tools.contains_key(&name) {
                conflicts.push(name);
            }
        }
        if !conflicts.is_empty() {
            return Err(format!(
                "tool registration refused for module {module:?}: already \
                 registered: {}",
                conflicts.join(", ")
            ));
        }
        for t in tools {
            let name = Self::full_name(module, t.operation());
            self.tools.insert(
                name,
                RegisteredTool {
                    module: module.to_owned(),
                    declaration: t.clone(),
                },
            );
        }
        Ok(())
    }

    pub fn get(&self, full_name: &str) -> Option<&RegisteredTool> {
        self.tools.get(full_name)
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &RegisteredTool)> {
        self.tools.iter().map(|(k, v)| (k.as_str(), v))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn raw(operation: &str, risk: &str, output_privacy: &str) -> RawToolDeclaration {
        RawToolDeclaration {
            operation: operation.to_owned(),
            description: "does a thing".to_owned(),
            input_schema: serde_json::json!({"type": "object", "properties": {}}),
            risk: risk.to_owned(),
            output_privacy: output_privacy.to_owned(),
            timeout_ms: 5_000,
            inline_wait_ms: 1_000,
        }
    }

    // ---------- validate_tools: positive control ----------

    #[test]
    fn a_valid_tool_declaration_round_trips() {
        let out = validate_tools(vec![raw("list-notes", "read", "local_only")]).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].operation(), "list-notes");
        assert_eq!(out[0].risk(), DeclaredRisk::Read);
        assert_eq!(out[0].output_privacy(), DeclaredOutputPrivacy::LocalOnly);
        assert_eq!(out[0].timeout_ms(), 5_000);
        assert_eq!(out[0].inline_wait_ms(), 1_000);
    }

    #[test]
    fn no_declared_tools_is_not_an_error() {
        // The compatibility case: a manifest written before this field existed
        // deserializes `tools` as empty via `#[serde(default)]`, and that must
        // validate to an empty list, not be refused.
        assert_eq!(validate_tools(vec![]).unwrap(), vec![]);
    }

    // ---------- ADR-K1-01 §5(2) reflex cases ----------

    #[test]
    fn an_invalid_operation_name_is_refused() {
        let err = validate_tools(vec![raw("List.Notes", "read", "local_only")]).unwrap_err();
        assert!(err.to_string().contains("invalid operation name"), "{err}");
    }

    #[test]
    fn a_duplicate_operation_in_one_manifest_is_refused() {
        let err = validate_tools(vec![
            raw("list-notes", "read", "local_only"),
            raw("list-notes", "write", "local_only"),
        ])
        .unwrap_err();
        assert!(err.to_string().contains("declared more than once"), "{err}");
    }

    #[test]
    fn an_empty_description_is_refused() {
        let mut bad = raw("list-notes", "read", "local_only");
        bad.description = "   ".to_owned();
        let err = validate_tools(vec![bad]).unwrap_err();
        assert!(err.to_string().contains("description"), "{err}");
    }

    #[test]
    fn an_unknown_risk_enum_is_refused() {
        let err =
            validate_tools(vec![raw("list-notes", "catastrophic", "local_only")]).unwrap_err();
        assert!(err.to_string().contains("risk:"), "{err}");
    }

    #[test]
    fn an_unknown_output_privacy_enum_is_refused() {
        let err = validate_tools(vec![raw("list-notes", "read", "anywhere")]).unwrap_err();
        assert!(err.to_string().contains("output_privacy:"), "{err}");
    }

    #[test]
    fn a_non_object_input_schema_is_refused() {
        let mut bad = raw("list-notes", "read", "local_only");
        bad.input_schema = serde_json::json!("not an object");
        let err = validate_tools(vec![bad]).unwrap_err();
        assert!(err.to_string().contains("input_schema"), "{err}");
    }

    #[test]
    fn an_input_schema_whose_declared_type_is_not_object_is_refused() {
        let mut bad = raw("list-notes", "read", "local_only");
        bad.input_schema = serde_json::json!({"type": "string"});
        let err = validate_tools(vec![bad]).unwrap_err();
        assert!(err.to_string().contains("input_schema"), "{err}");
    }

    #[test]
    fn a_zero_timeout_is_refused() {
        let mut bad = raw("list-notes", "read", "local_only");
        bad.timeout_ms = 0;
        bad.inline_wait_ms = 0;
        let err = validate_tools(vec![bad]).unwrap_err();
        assert!(err.to_string().contains("timeout_ms"), "{err}");
    }

    #[test]
    fn a_timeout_over_the_declared_ceiling_is_refused() {
        let mut bad = raw("list-notes", "read", "local_only");
        bad.timeout_ms = MAX_DECLARED_TIMEOUT_MS + 1;
        let err = validate_tools(vec![bad]).unwrap_err();
        assert!(err.to_string().contains("ceiling"), "{err}");
    }

    #[test]
    fn inline_wait_ms_equal_to_timeout_ms_is_refused() {
        // ADR-K1-01 §2.4: inline_wait_ms must be STRICTLY less than timeout_ms.
        let mut bad = raw("list-notes", "read", "local_only");
        bad.timeout_ms = 1_000;
        bad.inline_wait_ms = 1_000;
        let err = validate_tools(vec![bad]).unwrap_err();
        assert!(err.to_string().contains("inline_wait_ms"), "{err}");
    }

    #[test]
    fn inline_wait_ms_over_timeout_ms_is_refused() {
        let mut bad = raw("list-notes", "read", "local_only");
        bad.timeout_ms = 1_000;
        bad.inline_wait_ms = 2_000;
        let err = validate_tools(vec![bad]).unwrap_err();
        assert!(err.to_string().contains("inline_wait_ms"), "{err}");
    }

    // ---------- ModuleToolRegistry ----------

    fn declared(operation: &str) -> ToolDeclaration {
        validate_tools(vec![raw(operation, "read", "local_only")])
            .unwrap()
            .remove(0)
    }

    #[test]
    fn two_different_modules_register_without_conflict() {
        let mut reg = ModuleToolRegistry::new();
        reg.register_module("sin90", &[declared("list-notes")])
            .unwrap();
        reg.register_module("cos72", &[declared("list-notes")])
            .unwrap();

        assert_eq!(reg.len(), 2);
        assert!(reg.get("sin90.list-notes").is_some());
        assert!(reg.get("cos72.list-notes").is_some());
    }

    #[test]
    fn re_registering_the_same_full_name_is_refused_without_overwriting() {
        let mut reg = ModuleToolRegistry::new();
        reg.register_module("sin90", &[declared("list-notes")])
            .unwrap();
        let first = reg.get("sin90.list-notes").unwrap().clone();

        // Same module id registering again — e.g. a re-mount — must be refused,
        // not silently accepted as an update.
        let err = reg
            .register_module("sin90", &[declared("list-notes")])
            .unwrap_err();
        assert!(err.contains("already"), "{err}");
        assert_eq!(
            reg.get("sin90.list-notes").unwrap(),
            &first,
            "must not overwrite"
        );
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn two_discovered_packages_sharing_a_module_name_do_not_overwrite_each_other() {
        // §5(2)'s "two modules register the same operation": two installed
        // packages that (incorrectly) share a module id each declaring the
        // same operation name collide on the same full name.
        let mut reg = ModuleToolRegistry::new();
        reg.register_module("dup", &[declared("list-notes")])
            .unwrap();
        let before = reg.get("dup.list-notes").unwrap().clone();

        let err = reg
            .register_module("dup", &[declared("list-notes"), declared("other-op")])
            .unwrap_err();
        assert!(err.contains("dup.list-notes"), "{err}");
        // Atomic: `other-op` must NOT have been registered either.
        assert!(
            reg.get("dup.other-op").is_none(),
            "partial registration leaked an entry"
        );
        assert_eq!(reg.get("dup.list-notes").unwrap(), &before);
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn registering_an_empty_tool_list_is_a_harmless_no_op() {
        let mut reg = ModuleToolRegistry::new();
        reg.register_module("no-tools", &[]).unwrap();
        assert!(reg.is_empty());
    }

    #[test]
    fn full_name_is_module_dot_operation() {
        assert_eq!(
            ModuleToolRegistry::full_name("sin90", "list-notes"),
            "sin90.list-notes"
        );
    }
}
