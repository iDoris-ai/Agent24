//! Agent24 domain-OS contract (M-E / ME-1; ADR-029 内核↔领域 OS 边界).
//!
//! Agent24 is a BASE: one kernel, plus a swappable **domain OS** (Sin90 today,
//! Cos72 or a third-party one tomorrow). This crate holds the contract between
//! them, and nothing else — no kernel, no module, no I/O runtime. That keeps the
//! dependency arrow one-way and lets the kernel mount a module **without knowing
//! its name**, which is the ME-1 acceptance.
//!
//! ```text
//!   agent24d (kernel)  ──depends on──▶  agent24-domain  ◀──depends on──  a domain OS
//! ```
//!
//! Three pieces:
//! - [`DomainOsManifest`] — what a `domain-os.yml` declares (route namespace,
//!   event module, data dir, required models/APIs, requested capabilities).
//! - [`DomainModule`] — what a module implements: open its OWN store, hand back
//!   its routes.
//! - [`KernelCtx`] — what the kernel lends BACK, **capability-scoped**: a module
//!   gets an [`EventSink`] that can only speak in its own name, not an ambient
//!   handle to everything.
//!
//! **What is deliberately NOT here yet** (ME-1b and later, documented rather than
//! faked): the kernel-side registry that mounts modules and drops
//! `AppState.sin90`; `ScopedMemory` over the M-D stores (needs
//! `KernelCtx::memory(scope, grants)` — the ADR-029 hole); the model / scheduler
//! / policy handles; and the out-of-process Provider path (ME-3). [`Capability`]
//! already enumerates them so a manifest can request one and the kernel can
//! refuse what it cannot grant.

use std::collections::BTreeSet;

use agent24_protocol::{EventBody, ModuleEventPayload};
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum DomainError {
    #[error("manifest: {0}")]
    Manifest(String),
    #[error("capability not granted: {0}")]
    CapabilityDenied(String),
    #[error("module store: {0}")]
    Store(String),
}

pub type Result<T> = std::result::Result<T, DomainError>;

/// A kernel capability a domain OS may request in its manifest. The kernel
/// grants a SUBSET; anything not granted must be unreachable, not merely
/// unused — see [`Grants`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Emit `EventBody::Module` events under the module's OWN name.
    Events,
    /// Ask the kernel's model router for completions.
    Models,
    /// Register/inspect schedules.
    Scheduler,
    /// Consult the approval/risk policy engine.
    Policy,
    /// A SCOPE-LIMITED memory handle (M-D stores). Not ambient — the scope and
    /// grants are fixed when the handle is minted. Implementation lands with
    /// `KernelCtx::memory` (ME-1b+).
    Memory,
}

impl Capability {
    pub fn as_str(self) -> &'static str {
        match self {
            Capability::Events => "events",
            Capability::Models => "models",
            Capability::Scheduler => "scheduler",
            Capability::Policy => "policy",
            Capability::Memory => "memory",
        }
    }
}

/// How a domain OS is implemented.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImplKind {
    /// Compiled into the daemon (Sin90 today).
    InProcessCrate,
    /// A separate process reached over a protocol (ME-3). Declared now so a
    /// manifest can say so; the kernel-side path is later.
    OutOfProcessProvider,
}

/// The parsed `domain-os.yml`.
///
/// `name` is the module's identity everywhere: it fixes the route namespace, the
/// `EventBody::Module` name, and the data directory. That single-source rule is
/// what lets the kernel mount a module generically — and what [`validate`]
/// (called by [`DomainOsManifest::from_yaml`]) enforces, so a manifest cannot
/// claim one name and emit under another.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainOsManifest {
    pub name: String,
    pub version: String,
    /// `/api/v1/<name>` — derived, but stored so a mismatch is a loud manifest
    /// error rather than a silent divergence.
    pub route_namespace: String,
    /// The `module` field of every `EventBody::Module` this OS emits.
    pub event_module: String,
    /// `~/.agent24/os/<name>/` — the module's OWN directory (its own DB).
    pub data_dir: String,
    #[serde(default)]
    pub requires_models: Vec<String>,
    #[serde(default)]
    pub requires_apis: Vec<String>,
    #[serde(default)]
    pub requires_deps: Vec<String>,
    #[serde(default)]
    pub kernel_capabilities: Vec<Capability>,
    #[serde(default)]
    pub ui_entry: Option<String>,
    pub impl_kind: ImplKind,
}

/// A module name: lowercase alphanumerics plus `-`/`_`, starting alphanumeric.
/// Deliberately stricter than the event schema's pattern — the name also becomes
/// a URL segment and a DIRECTORY, so `.`, `/`, `~` and leading dashes are out.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

impl DomainOsManifest {
    /// Parse and VALIDATE a `domain-os.yml`. Validation is not optional: an
    /// unchecked manifest could claim `name: sin90` while emitting events as
    /// `cos72` or serving routes under another namespace.
    pub fn from_yaml(yaml: &str) -> Result<Self> {
        let m: DomainOsManifest =
            serde_yaml::from_str(yaml).map_err(|e| DomainError::Manifest(e.to_string()))?;
        m.validate()?;
        Ok(m)
    }

    /// The invariants the kernel relies on when mounting generically.
    pub fn validate(&self) -> Result<()> {
        if !valid_name(&self.name) {
            return Err(DomainError::Manifest(format!(
                "invalid module name {:?}: must be [a-z0-9][a-z0-9_-]*",
                self.name
            )));
        }
        if self.version.trim().is_empty() {
            return Err(DomainError::Manifest("version must not be empty".into()));
        }
        let expected_ns = format!("/api/v1/{}", self.name);
        if self.route_namespace != expected_ns {
            return Err(DomainError::Manifest(format!(
                "route_namespace {:?} must be {:?} (derived from name)",
                self.route_namespace, expected_ns
            )));
        }
        if self.event_module != self.name {
            return Err(DomainError::Manifest(format!(
                "event_module {:?} must equal name {:?} — a module may not emit \
                 events in another module's name",
                self.event_module, self.name
            )));
        }
        if !self.data_dir.contains(&self.name) {
            return Err(DomainError::Manifest(format!(
                "data_dir {:?} must be the module's own directory (containing {:?})",
                self.data_dir, self.name
            )));
        }
        Ok(())
    }
}

/// The capabilities the kernel actually granted this module — the SUBSET of what
/// the manifest requested that the kernel was willing and able to provide.
///
/// A grant set is minted by the kernel, never by the module: a module cannot widen
/// its own authority by editing its manifest, because the kernel intersects the
/// request with what it will give ([`Grants::granting`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Grants {
    granted: BTreeSet<Capability>,
}

impl Grants {
    /// Grant exactly `caps`.
    pub fn new(caps: impl IntoIterator<Item = Capability>) -> Self {
        Self {
            granted: caps.into_iter().collect(),
        }
    }

    /// What the kernel will give a module that REQUESTED `requested`, given the
    /// kernel is `willing` to hand out at most those. The result is the
    /// intersection — requesting more than the kernel offers silently gains
    /// nothing rather than escalating.
    pub fn granting(requested: &[Capability], willing: &[Capability]) -> Self {
        let willing: BTreeSet<_> = willing.iter().copied().collect();
        Self {
            granted: requested
                .iter()
                .copied()
                .filter(|c| willing.contains(c))
                .collect(),
        }
    }

    pub fn has(&self, cap: Capability) -> bool {
        self.granted.contains(&cap)
    }

    pub fn require(&self, cap: Capability) -> Result<()> {
        if self.has(cap) {
            Ok(())
        } else {
            Err(DomainError::CapabilityDenied(cap.as_str().to_owned()))
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = Capability> + '_ {
        self.granted.iter().copied()
    }
}

/// Where a module's events go. The kernel supplies the transport; the module
/// never touches it directly.
pub trait EventBroadcast: Send + Sync {
    fn send(&self, body: EventBody);
}

/// A module's ONLY way to emit events — bound to one module name at construction.
///
/// This is the capability in "capability-scoped": [`EventSink::emit`] takes just a
/// `kind` and a payload, and stamps the module name itself. A module therefore
/// **cannot** forge an event in another module's name, no matter what it passes —
/// there is no parameter for it. Without the [`Capability::Events`] grant, the
/// sink refuses to emit at all.
pub struct EventSink {
    module: String,
    grants: Grants,
    out: std::sync::Arc<dyn EventBroadcast>,
}

impl EventSink {
    pub fn new(
        module: impl Into<String>,
        grants: Grants,
        out: std::sync::Arc<dyn EventBroadcast>,
    ) -> Self {
        Self {
            module: module.into(),
            grants,
            out,
        }
    }

    /// The module name every event from this sink carries.
    pub fn module(&self) -> &str {
        &self.module
    }

    /// Emit `module.<kind>`. Returns `CapabilityDenied` when `events` was not
    /// granted — a denial is an ERROR, not a silent no-op, so a module that
    /// depends on events fails loudly at the boundary instead of going quiet.
    pub fn emit(&self, kind: &str, payload: serde_json::Value) -> Result<()> {
        self.grants.require(Capability::Events)?;
        // The envelope guarantees an object; a non-object payload is coerced
        // rather than silently reshaping a client's parser expectations.
        let payload = match payload {
            serde_json::Value::Object(m) => m,
            _ => serde_json::Map::new(),
        };
        self.out.send(EventBody::Module(ModuleEventPayload {
            module: self.module.clone(),
            kind: kind.to_owned(),
            payload,
        }));
        Ok(())
    }
}

/// What the kernel lends a module. Capability-scoped by construction: the module
/// receives the handles it was GRANTED, not an ambient pointer to the daemon.
///
/// Only [`KernelCtx::events`] exists today; `models` / `scheduler` / `policy` /
/// `memory(scope, grants)` land as their consumers do (ME-1b+). They are named in
/// [`Capability`] so a manifest can already request them and the kernel can
/// already refuse.
pub trait KernelCtx: Send + Sync {
    /// The capabilities this module actually holds.
    fn grants(&self) -> &Grants;
    /// The module-scoped event sink (see [`EventSink`]).
    fn events(&self) -> &EventSink;
}

/// A domain OS the kernel can mount without knowing its name.
///
/// `routes` returns a namespaced router; the kernel mounts it under the
/// manifest's `route_namespace` and never hardcodes a path. `open_store` is the
/// module's OWN persistence (its own DB + migrations, under the manifest's
/// `data_dir`) — the kernel neither opens nor understands it, which is what keeps
/// two domain OSes from sharing a database by accident.
#[async_trait::async_trait]
pub trait DomainModule: Send + Sync {
    fn name(&self) -> &str;
    fn manifest(&self) -> &DomainOsManifest;

    /// Open this module's own store under `dir` (its `data_dir`), running its own
    /// migrations. A failure degrades THIS module only — the kernel keeps running
    /// and the module's routes answer 503 (the existing Sin90 behavior, made part
    /// of the contract).
    async fn open_store(&self, dir: &std::path::Path) -> Result<()>;

    /// The module's event-module name. MUST equal `name()`; the manifest
    /// validation enforces it, and this accessor exists so the kernel can assert
    /// it when mounting.
    fn event_module(&self) -> &str {
        self.name()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::sync::{Arc, Mutex};

    const SIN90_YAML: &str = r#"
name: sin90
version: "0.2.1"
route_namespace: /api/v1/sin90
event_module: sin90
data_dir: ~/.agent24/os/sin90/
requires_models: []
requires_apis: []
requires_deps: []
kernel_capabilities: [events]
impl_kind: in_process_crate
"#;

    #[derive(Default)]
    struct RecordingBus {
        sent: Mutex<Vec<EventBody>>,
    }
    impl EventBroadcast for RecordingBus {
        fn send(&self, body: EventBody) {
            self.sent.lock().unwrap().push(body);
        }
    }

    fn module_events(bus: &RecordingBus) -> Vec<(String, String)> {
        bus.sent
            .lock()
            .unwrap()
            .iter()
            .filter_map(|b| match b {
                EventBody::Module(m) => Some((m.module.clone(), m.kind.clone())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn parses_a_valid_manifest() {
        let m = DomainOsManifest::from_yaml(SIN90_YAML).unwrap();
        assert_eq!(m.name, "sin90");
        assert_eq!(m.route_namespace, "/api/v1/sin90");
        assert_eq!(m.event_module, "sin90");
        assert_eq!(m.impl_kind, ImplKind::InProcessCrate);
        assert_eq!(m.kernel_capabilities, vec![Capability::Events]);
    }

    #[test]
    fn manifest_cannot_claim_another_modules_event_name() {
        // The whole point of validation: a module that emits as someone else
        // would let a rogue OS impersonate another's events on the shared WS hub.
        let yaml = SIN90_YAML.replace("event_module: sin90", "event_module: cos72");
        let err = DomainOsManifest::from_yaml(&yaml).unwrap_err();
        assert!(matches!(err, DomainError::Manifest(_)), "{err}");
        assert!(err.to_string().contains("event_module"), "{err}");
    }

    #[test]
    fn manifest_namespace_must_derive_from_name() {
        let yaml = SIN90_YAML.replace(
            "route_namespace: /api/v1/sin90",
            "route_namespace: /api/v1/other",
        );
        let err = DomainOsManifest::from_yaml(&yaml).unwrap_err();
        assert!(err.to_string().contains("route_namespace"), "{err}");
    }

    #[test]
    fn manifest_data_dir_must_be_the_modules_own() {
        // Two OSes pointing at one directory is exactly the cross-contamination
        // this contract exists to prevent.
        let yaml = SIN90_YAML.replace(
            "data_dir: ~/.agent24/os/sin90/",
            "data_dir: ~/.agent24/os/cos72/",
        );
        let err = DomainOsManifest::from_yaml(&yaml).unwrap_err();
        assert!(err.to_string().contains("data_dir"), "{err}");
    }

    #[test]
    fn rejects_names_that_are_unsafe_as_a_path_or_url_segment() {
        for bad in [
            "../evil",
            "a/b",
            "Sin90",
            "-lead",
            "",
            "has space",
            "dot.name",
        ] {
            let yaml = SIN90_YAML
                .replace("name: sin90", &format!("name: {bad:?}"))
                .replace(
                    "route_namespace: /api/v1/sin90",
                    &format!("route_namespace: /api/v1/{bad}"),
                )
                .replace("event_module: sin90", &format!("event_module: {bad:?}"))
                .replace("os/sin90/", &format!("os/{bad}/"));
            assert!(
                DomainOsManifest::from_yaml(&yaml).is_err(),
                "name {bad:?} must be rejected"
            );
        }
    }

    #[test]
    fn grants_are_the_intersection_not_the_request() {
        // A module asking for more than the kernel offers gains nothing.
        let g = Grants::granting(
            &[Capability::Events, Capability::Models, Capability::Memory],
            &[Capability::Events],
        );
        assert!(g.has(Capability::Events));
        assert!(!g.has(Capability::Models), "requesting does not grant");
        assert!(!g.has(Capability::Memory));
        assert!(g.require(Capability::Models).is_err());
    }

    #[test]
    fn event_sink_stamps_its_own_module_name() {
        let bus = Arc::new(RecordingBus::default());
        let sink = EventSink::new("sin90", Grants::new([Capability::Events]), bus.clone());
        sink.emit("task.transitioned", serde_json::json!({"id": "t1"}))
            .unwrap();
        assert_eq!(
            module_events(&bus),
            vec![("sin90".to_owned(), "task.transitioned".to_owned())]
        );
    }

    #[test]
    fn a_module_cannot_forge_another_modules_events() {
        // There is no parameter for the module name — the sink supplies it. Even
        // a kind that LOOKS like another module's is still stamped as this one's.
        let bus = Arc::new(RecordingBus::default());
        let sink = EventSink::new("sin90", Grants::new([Capability::Events]), bus.clone());
        sink.emit("cos72.stolen", serde_json::json!({})).unwrap();
        let events = module_events(&bus);
        assert_eq!(events[0].0, "sin90", "module name is not caller-supplied");
    }

    #[test]
    fn emitting_without_the_events_grant_is_a_loud_error() {
        let bus = Arc::new(RecordingBus::default());
        let sink = EventSink::new("sin90", Grants::default(), bus.clone());
        let err = sink.emit("x", serde_json::json!({})).unwrap_err();
        assert!(matches!(err, DomainError::CapabilityDenied(_)), "{err}");
        assert!(module_events(&bus).is_empty(), "nothing was emitted");
    }

    #[test]
    fn non_object_payload_is_coerced_to_an_object() {
        let bus = Arc::new(RecordingBus::default());
        let sink = EventSink::new("sin90", Grants::new([Capability::Events]), bus.clone());
        sink.emit("k", serde_json::json!("not an object")).unwrap();
        match &bus.sent.lock().unwrap()[0] {
            EventBody::Module(m) => assert!(m.payload.is_empty()),
            other => panic!("expected Module, got {other:?}"),
        }
    }
}
