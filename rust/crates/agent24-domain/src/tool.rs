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
//! without touching what is already there — any registration whose MODULE
//! NAME has already registered, even if this call's operation names collide
//! with nothing (ADR-K1-01 §3: cross-module impersonation must fail closed at
//! the module level, not only when two operation names happen to collide).
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
//!
//! `risk` reuses [`agent24_protocol::RiskClass`] — the SAME closed enum the
//! approval path already reads (`Read`/`WriteLocal`/`Exec`/`External`) —
//! rather than a second, tool-specific vocabulary. `agent24-domain` already
//! depends on `agent24-protocol`, so this costs nothing, and it is what lets
//! Documenting's `handoff` (ADR-DOC-02, which maps it to `RiskClass::External`)
//! declare itself as `external` directly: a second enum here would have forced
//! a lossy mapping (no `External`-equivalent tier existed) the first time a
//! real manifest tried to declare that operation.

use std::collections::{BTreeMap, BTreeSet};

use agent24_protocol::RiskClass;
use serde::{Deserialize, Serialize};

use crate::{DomainError, is_valid_module_name};

/// Conservative sanity ceiling on a declared `timeout_ms`: ten minutes. The
/// ADR leaves the exact operative limit to the host, which "can shorten, never
/// lengthen" it (§2.4) — this bounds the DECLARATION itself, so a manifest
/// cannot claim a single tool call may run unboundedly long. Twice
/// `agent24_os_proto`'s existing `MAX_METHOD_CALL_TIMEOUT` (300s): that
/// constant bounds one kernel→module HTTP round trip, while this one bounds a
/// tool's declared ceiling for a call that may itself include slower
/// module-side work (e.g. a long-running local model); doubling the nearest
/// existing precedent rather than inventing an unrelated number.
pub const MAX_DECLARED_TIMEOUT_MS: u64 = 10 * 60 * 1000;

/// Map a manifest's `risk` STRING onto [`RiskClass`], naming the rejected
/// value and the choices like [`crate::Capability::parse`] — `RiskClass`'s
/// own `Deserialize` (snake_case) does the real mapping, but its serde error
/// cannot quote which string the manifest actually typed in a form a caller
/// can read back.
fn parse_risk(s: &str) -> Result<RiskClass, String> {
    serde_json::from_value(serde_json::Value::String(s.to_owned())).map_err(|_| {
        format!(
            "risk: {s:?} is not one of read, write_local, exec, external \
             (agent24_protocol::RiskClass)"
        )
    })
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
    risk: RiskClass,
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
    pub fn risk(&self) -> RiskClass {
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
        let risk = parse_risk(&r.risk)
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
/// (ADR-K1-01 §2.1, §2.2, §3, §5(2)).
///
/// Read-only in the sense that matters: there is no public method that
/// removes or overwrites an entry. [`register_module`](Self::register_module)
/// is the only way in, and it refuses — atomically, leaving the registry
/// exactly as it was — rather than overwrite anything already registered.
///
/// The fail-closed boundary is the **module name**, not just the full
/// `<module>.<operation>` name (ADR-K1-01 §3's "cross-module impersonation /
/// tool-name collision": "模块名由 manifest 校验, 工具名按命名空间限定, 注册
/// 冲突 fail closed"). Once a module name has registered — even with an EMPTY
/// tool list — no later call under that same module name may add, change, or
/// claim anything, regardless of which operation names it declares. This
/// mirrors the mounter's own module-identity de-duplication: two discovered
/// packages that (incorrectly) declare the same `name` must be refused the
/// same way here as they are there, not just when their operation names
/// happen to collide too.
///
/// **Not communicated to the agent, and provides no invocation path.** That
/// is K1-5.2 (discovery/announcement) and K1-5.3 (the call path); this type
/// offers neither.
#[derive(Debug, Clone, Default)]
pub struct ModuleToolRegistry {
    tools: BTreeMap<String, RegisteredTool>,
    /// Every module name that has ever registered, even with zero tools. A
    /// module that declares no tools still CLAIMS its name — a later package
    /// sharing that name must be refused even though it collides with no
    /// individual tool.
    registered_modules: BTreeSet<String>,
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
    /// Atomic and fail-closed at the MODULE level (ADR-K1-01 §3, §5(2)): if
    /// `module` has already registered — under ANY previous call, including
    /// one with an empty `tools` list — this call is refused in full and the
    /// registry is left exactly as it was, regardless of whether this call's
    /// operation names happen to collide with anything already registered.
    /// That is the gap a full-name-only check misses: two discovered packages
    /// declaring the same module `name` but DIFFERENT operations (`list-notes`
    /// vs `delete-all`) produce no full-name collision at all, yet the second
    /// one is exactly the cross-module impersonation §3 requires to fail
    /// closed. A full-name collision check runs too (defense in depth — it
    /// should be unreachable once a module name is unique, since
    /// `validate_tools` already refuses a duplicate operation WITHIN one
    /// manifest), but the module-name check is the one that actually gates
    /// this method, and it runs first. Never partially registers a module's
    /// tools, and never overwrites an existing entry.
    pub fn register_module(
        &mut self,
        module: &str,
        tools: &[ToolDeclaration],
    ) -> Result<(), String> {
        if !is_valid_module_name(module) {
            return Err(format!(
                "tool registration refused: {module:?} is not a valid module name"
            ));
        }
        if self.registered_modules.contains(module) {
            return Err(format!(
                "tool registration refused for module {module:?}: this module name \
                 is already registered (ADR-K1-01 §3: cross-module impersonation \
                 must fail closed at the module level, not only on an operation-name \
                 collision)"
            ));
        }
        // Defense in depth only — see the docstring above. Unreachable in
        // practice once the module-name check above holds, since a module
        // name can register at most once and `validate_tools` already
        // refuses a duplicate operation within one manifest.
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
        self.registered_modules.insert(module.to_owned());
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

    /// Every registered tool whose gates (`view`) ALL currently pass
    /// (ADR-K1-01 §2.2, K1-5.2). Computed fresh on every call — nothing is
    /// cached — so re-calling after `view`'s answers change satisfies
    /// "动态状态变化时，后续通告应更新" with no cache to invalidate.
    ///
    /// Still registration-only: an advert carries display/description data
    /// only, never a call path or `tool_call_id`. That is K1-5.3.
    pub fn adverts(&self, view: &dyn ModuleToolAdvertView) -> Vec<ModuleToolAdvert> {
        self.tools
            .values()
            .filter(|t| {
                let module = t.module();
                let operation = t.declaration().operation();
                view.module_ready(module)
                    && view.operation_available(module, operation)
                    && view.is_authorized(module, operation)
                    && !view.blocked_by_remote_tier_guard(module, operation)
            })
            .map(|t| ModuleToolAdvert {
                full_name: Self::full_name(t.module(), t.declaration().operation()),
                description: t.declaration().description().to_owned(),
                input_schema: t.declaration().input_schema().clone(),
            })
            .collect()
    }
}

/// What the host shows the model for one advertisable module tool
/// (ADR-K1-01 §2.2, K1-5.2). Carries display/description data only — no
/// `tool_call_id`, no trusted context, no call path. Advertising a tool is
/// not itself permission to invoke it; that wiring is K1-5.3.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleToolAdvert {
    full_name: String,
    description: String,
    input_schema: serde_json::Value,
}

impl ModuleToolAdvert {
    /// `<module>.<operation>` — see [`ModuleToolRegistry::full_name`].
    pub fn full_name(&self) -> &str {
        &self.full_name
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn input_schema(&self) -> &serde_json::Value {
        &self.input_schema
    }
}

/// The per-`(module, operation)` gates a host must satisfy before a
/// registered tool is advertised to the model (ADR-K1-01 §2.2; jason's
/// 2026-10-08 ruling point 5). [`ModuleToolRegistry::adverts`] queries this
/// trait fresh on every call, for every registered tool — nothing cached.
///
/// **Every method defaults to the most conservative answer**, so a zero-
/// override impl — `impl ModuleToolAdvertView for Noop {}` — advertises
/// nothing. None of the four gates has a real backing system wired into
/// this crate yet: gates 1+2/3 need live lifecycle/`/capabilities` state a
/// future host (e.g. `agent24d`) must supply; gate 4 needs K1-6a.1's
/// authorization store (#769, not merged); gate 5 needs K1-6a AND K1-6b,
/// neither accepted yet. Gate 5 only ever NARROWS what gates 1-4 allowed,
/// never the reverse, so defaulting it open widens nothing.
pub trait ModuleToolAdvertView {
    /// Gates 1+2 (ADR-K1-01 §2.2): `module` is registered, enabled, running
    /// and ready — NOT disabled, circuit-broken (breaker tripped), backing
    /// off, draining/stopping, or crashed. Default: not ready (fail-closed).
    fn module_ready(&self, _module: &str) -> bool {
        false
    }

    /// Gate 3: `operation`'s capability is known AND currently available
    /// (e.g. the module's `/capabilities` response). Unknown and known-
    /// unavailable are the SAME outcome: both answer `false`. Default:
    /// unavailable (fail-closed — no capability source wired here).
    fn operation_available(&self, _module: &str, _operation: &str) -> bool {
        false
    }

    /// Gate 4 (ADR-K1-01 §2.1, §5(1)): a current host authorization for
    /// this exact `(module, operation)` exists — not expired/revoked, and
    /// covering any scope a version upgrade added. The module's own
    /// `risk`/`output_privacy` DECLARATION — including an unconfirmed
    /// third-party module self-reporting `Read`/`local_only` — never
    /// substitutes for this. Default: unauthorized — K1-6a.1 (#769) is not
    /// merged, so there is no authorization source to consult yet.
    fn is_authorized(&self, _module: &str, _operation: &str) -> bool {
        false
    }

    /// Gate 5 — jason's 2026-10-08 ruling point 5 (ADR-K1-01 §2.1, §4.4):
    /// while `true` for `(module, operation)`, that tool is withheld even
    /// if gates 1-4 all passed, until K1-6a AND K1-6b are BOTH accepted.
    /// This crate has no "document" domain of its own (§0 forbids adding
    /// one); a host wiring a real check owns both "is this a Documenting
    /// operation" and "does a remote model tier exist" and combines them
    /// itself. Default `false`: this slice has no Documenting tools and no
    /// remote-tier source, so the hook is a no-op — harmless, since the
    /// three gates above already default to blocking everything.
    fn blocked_by_remote_tier_guard(&self, _module: &str, _operation: &str) -> bool {
        false
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
        assert_eq!(out[0].risk(), RiskClass::Read);
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
            raw("list-notes", "write_local", "local_only"),
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

    /// THE reflex case for the module-level gap: two installed packages share
    /// a module `name` ("dup") but declare DIFFERENT operations, so there is
    /// NO full-name collision at all (`dup.list-notes` vs `dup.delete-all`).
    /// Before this fix, a full-name-only check let both through — exactly the
    /// cross-module impersonation ADR-K1-01 §3 requires to fail closed. Named
    /// after the module identity it is actually testing, not the operation
    /// names (which deliberately do NOT collide).
    #[test]
    fn same_module_name_different_operations_is_refused_at_the_module_level() {
        let mut reg = ModuleToolRegistry::new();
        reg.register_module("dup", &[declared("list-notes")])
            .unwrap();
        let before = reg.get("dup.list-notes").unwrap().clone();

        let err = reg
            .register_module("dup", &[declared("delete-all")])
            .unwrap_err();
        assert!(err.contains("dup"), "{err}");
        assert!(err.contains("already"), "{err}");
        // The whole point: no full name collided, yet the second package's
        // operation must still be refused.
        assert!(
            reg.get("dup.delete-all").is_none(),
            "a non-colliding operation name must not let a second package \
             claim an already-registered module"
        );
        assert_eq!(reg.get("dup.list-notes").unwrap(), &before);
        assert_eq!(reg.len(), 1);
    }

    /// A module that declares NO tools still claims its name. A later package
    /// sharing that name must be refused even though the first registration
    /// left nothing in `tools` to collide with.
    #[test]
    fn an_empty_tool_list_still_claims_the_module_name() {
        let mut reg = ModuleToolRegistry::new();
        reg.register_module("dup", &[]).unwrap();
        assert!(reg.is_empty(), "an empty tool list registers no tools");

        let err = reg
            .register_module("dup", &[declared("list-notes")])
            .unwrap_err();
        assert!(err.contains("dup"), "{err}");
        assert!(
            reg.get("dup.list-notes").is_none(),
            "the module name was already claimed by the empty-tools registration"
        );
        assert!(reg.is_empty());
    }

    #[test]
    fn registering_an_empty_tool_list_is_a_harmless_no_op() {
        let mut reg = ModuleToolRegistry::new();
        reg.register_module("no-tools", &[]).unwrap();
        assert!(reg.is_empty());
    }

    #[test]
    fn a_registration_with_an_invalid_module_name_is_refused() {
        let mut reg = ModuleToolRegistry::new();
        let err = reg
            .register_module("Not.Valid", &[declared("list-notes")])
            .unwrap_err();
        assert!(err.contains("not a valid module name"), "{err}");
        assert!(reg.is_empty());
    }

    #[test]
    fn full_name_is_module_dot_operation() {
        assert_eq!(
            ModuleToolRegistry::full_name("sin90", "list-notes"),
            "sin90.list-notes"
        );
    }

    // ---------- K1-5.2: ModuleToolRegistry::adverts (ADR-K1-01 §2.2) ----------

    /// A fully configurable [`ModuleToolAdvertView`] for tests: every gate is
    /// an explicit field, so each test states exactly which gates it is
    /// exercising rather than relying on the trait's conservative defaults.
    struct TestView {
        module_ready: bool,
        operation_available: bool,
        authorized: bool,
        remote_tier_blocked: bool,
    }

    impl TestView {
        fn all_pass() -> Self {
            Self {
                module_ready: true,
                operation_available: true,
                authorized: true,
                remote_tier_blocked: false,
            }
        }
    }

    impl ModuleToolAdvertView for TestView {
        fn module_ready(&self, _module: &str) -> bool {
            self.module_ready
        }

        fn operation_available(&self, _module: &str, _operation: &str) -> bool {
            self.operation_available
        }

        fn is_authorized(&self, _module: &str, _operation: &str) -> bool {
            self.authorized
        }

        fn blocked_by_remote_tier_guard(&self, _module: &str, _operation: &str) -> bool {
            self.remote_tier_blocked
        }
    }

    /// The trait's own defaults, with nothing overridden — the "default
    /// trait" the task asks for a dedicated test on.
    struct NoopView;
    impl ModuleToolAdvertView for NoopView {}

    fn registry_with_one_tool() -> ModuleToolRegistry {
        let mut reg = ModuleToolRegistry::new();
        reg.register_module("sin90", &[declared("list-notes")])
            .unwrap();
        reg
    }

    #[test]
    fn the_default_trait_advertises_nothing() {
        // A host that has wired up none of the four gates yet — e.g. before
        // K1-6a.1 (#769) merges an authorization source — must not advertise
        // ANY registered tool, no matter how many are registered.
        let mut reg = registry_with_one_tool();
        reg.register_module("cos72", &[declared("other-op")])
            .unwrap();
        assert!(reg.adverts(&NoopView).is_empty());
    }

    #[test]
    fn all_gates_passing_advertises_the_tool() {
        // Positive control: once every gate answers favorably, the tool is
        // advertised with the display data the manifest declared.
        let reg = registry_with_one_tool();
        let adverts = reg.adverts(&TestView::all_pass());
        assert_eq!(adverts.len(), 1);
        assert_eq!(adverts[0].full_name(), "sin90.list-notes");
        assert_eq!(adverts[0].description(), "does a thing");
        assert_eq!(
            adverts[0].input_schema(),
            &serde_json::json!({"type": "object", "properties": {}})
        );
    }

    // ---- ADR-K1-01 §5(4): disabled/breaker/backoff/draining/crashed or
    // capability unknown/unavailable → filtered out of the announcement.

    #[test]
    fn a_module_that_is_not_running_and_ready_is_filtered_even_when_otherwise_authorized() {
        // Stands in for disabled, circuit-broken, backing off, draining, or
        // crashed — all of those collapse to `module_ready() == false` here.
        let reg = registry_with_one_tool();
        let view = TestView {
            module_ready: false,
            ..TestView::all_pass()
        };
        assert!(reg.adverts(&view).is_empty());
    }

    #[test]
    fn an_operation_whose_capability_is_unknown_or_unavailable_is_filtered() {
        let reg = registry_with_one_tool();
        let view = TestView {
            operation_available: false,
            ..TestView::all_pass()
        };
        assert!(reg.adverts(&view).is_empty());
    }

    // ---- ADR-K1-01 §5(1): no host authorization → not advertised; a
    // self-reported low risk never substitutes for it.

    #[test]
    fn an_unauthorized_tool_is_not_advertised_even_when_fully_ready() {
        let reg = registry_with_one_tool();
        let view = TestView {
            authorized: false,
            ..TestView::all_pass()
        };
        assert!(reg.adverts(&view).is_empty());
    }

    #[test]
    fn a_self_reported_read_local_only_declaration_does_not_substitute_for_authorization() {
        // `declared()` builds a `risk: read, output_privacy: local_only`
        // tool — the least-alarming self-report a module can make. It must
        // not move the outcome: only `TestView::authorized` does.
        let mut reg = ModuleToolRegistry::new();
        reg.register_module("third-party", &[declared("list-notes")])
            .unwrap();
        let view = TestView {
            authorized: false,
            ..TestView::all_pass()
        };
        assert!(reg.adverts(&view).is_empty());
    }

    // ---- jason's 2026-10-08 ruling point 5: the remote-tier/document guard
    // narrows what gates 1-4 already allowed; it never widens it.

    #[test]
    fn the_remote_tier_guard_withholds_a_tool_even_when_otherwise_fully_authorized() {
        let reg = registry_with_one_tool();
        let view = TestView {
            remote_tier_blocked: true,
            ..TestView::all_pass()
        };
        assert!(reg.adverts(&view).is_empty());
    }

    // ---- "动态状态变化时，后续通告应更新": adverts is recomputed fresh on
    // every call, so two calls against views that disagree see different
    // results without any cache to invalidate.

    #[test]
    fn adverts_reflects_the_view_handed_to_it_on_every_call() {
        let reg = registry_with_one_tool();
        assert!(
            reg.adverts(&TestView {
                authorized: false,
                ..TestView::all_pass()
            })
            .is_empty(),
            "unauthorized view must not advertise"
        );
        assert_eq!(
            reg.adverts(&TestView::all_pass()).len(),
            1,
            "the same registry, now queried with a view where every gate \
             passes, must advertise immediately — no stale cache"
        );
    }

    #[test]
    fn only_the_tool_whose_module_passes_every_gate_is_advertised() {
        let mut reg = ModuleToolRegistry::new();
        reg.register_module("ready-module", &[declared("list-notes")])
            .unwrap();
        reg.register_module("unready-module", &[declared("list-notes")])
            .unwrap();

        struct PerModuleView;
        impl ModuleToolAdvertView for PerModuleView {
            fn module_ready(&self, module: &str) -> bool {
                module == "ready-module"
            }
            fn operation_available(&self, _module: &str, _operation: &str) -> bool {
                true
            }
            fn is_authorized(&self, _module: &str, _operation: &str) -> bool {
                true
            }
        }

        let adverts = reg.adverts(&PerModuleView);
        assert_eq!(adverts.len(), 1);
        assert_eq!(adverts[0].full_name(), "ready-module.list-notes");
    }
}
