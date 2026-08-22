//! Agent24 domain-OS contract (M-E / ME-1; ADR-029 内核↔领域 OS 边界).
//!
//! Agent24 is a BASE: one kernel, plus a swappable **domain OS** (Sin90 today,
//! Cos72 or a third-party one tomorrow). This crate holds the contract between
//! them — types and traits only, no kernel and no module — so the dependency
//! arrow stays one-way and the kernel can mount a module **without knowing its
//! name**, which is the ME-1 acceptance.
//!
//! ```text
//!   agent24d (kernel)  ──depends on──▶  agent24-domain  ◀──depends on──  a domain OS
//! ```
//!
//! Three pieces:
//! - [`DomainOsManifest`] — the VALIDATED `domain-os.yml`. It has no public
//!   fields and no `Deserialize`, so the only ingress from CALLER-CONTROLLED data
//!   is [`DomainOsManifest::from_yaml`], which validates. (`Clone` also yields
//!   one, of course — from a value that already passed.) That is what makes
//!   "validation is not optional" true rather than merely stated.
//! - [`DomainModule`] — what a module implements: open its OWN store, hand back
//!   its routes. Its manifest is its SOLE identity — there is deliberately no
//!   `name()` accessor that could disagree with it.
//! - [`KernelCtx`] — what the kernel lends back. A capability is represented ONLY
//!   by the existence of a handle ([`KernelCtx::events`] returns `None` when
//!   events were not granted). There is deliberately no `grants()` accessor
//!   beside it: a second, informational answer to "may I?" invites
//!   `if ctx.grants().has(..) { ctx.events().unwrap() }`, which panics the moment
//!   the two disagree.
//!
//! [`http`] carries the fourth piece: the v1 error envelope and body limit, so
//! kernel and module CAN answer identically. It does not make them: nothing forces
//! a module's handlers through these helpers — it removes the excuse for a second
//! error shape, and the mounter uses them for the responses IT owns.
//!
//! **In-process modules are pinned to axum 0.8.** [`DomainModule::routes`] returns
//! an `axum::Router`, so this crate is a contract in types but not
//! framework-neutral. That is a deliberate ME-1b trade: a boxed tower service via
//! `nest_service` would be neutral at the cost of type-erasure noise, and the
//! kernel is axum anyway. `default-features = false` keeps a manifest-only or
//! out-of-process consumer from dragging in hyper/tokio. If a second HTTP
//! framework ever appears, split `agent24-domain-axum` out rather than widening
//! this trait.
//!
//! **Trust model, stated once and precisely.** An in-process module is compiled
//! into the daemon, so it is TRUSTED CODE. Rust visibility is not a sandbox:
//! nothing here prevents such a module from building its own [`EventSink`] or its
//! own [`Grants`]. What contains a module is (a) the TRANSPORT — a self-made sink
//! writes to a self-made [`EventBroadcast`] that reaches nobody, only the kernel
//! holds the real bus — and (b) for genuinely untrusted code, a PROCESS boundary,
//! which is ME-3's out-of-process provider. [`Grants`] is therefore
//! **informational**: it records what the kernel decided, and must never be
//! accepted from a caller as proof of authority.
//!
//! **What `validate` does NOT cover.** It checks one manifest for SELF-consistency
//! (namespace, event module and directory all derive from `name`). It cannot see
//! other modules, so **rejecting two modules that claim the same `name` is the
//! registry's job** — mounting a duplicate would collide on all three at once.
//! ME-1b's mounter must reserve the name atomically before opening a store or
//! mounting routes, and must refuse a manifest whose
//! [`DomainOsManifest::is_mountable_in_process`] is false.
//!
//! **Dependency note.** `serde_yaml 0.9.34+deprecated` is archived upstream
//! (2024-03). Adopted knowingly: the manifest is a small, local, trusted-path
//! document; that version carries no advisory and its `unsafe-libyaml` is past
//! RUSTSEC-2023-0075's fix line. It is a MAINTENANCE risk, not a parsing hole.
//! Revisit when ME-2 starts accepting manifests from elsewhere — at that point the
//! READER also needs a bounded read; [`DomainOsManifest::MAX_YAML_BYTES`] bounds an
//! already-loaded string and cannot stop a huge file from being read in first.
//!
//! **What is deliberately NOT here yet** (ME-1b and later, documented rather than
//! faked): the kernel-side registry that mounts modules and drops
//! `AppState.sin90`; `ScopedMemory` over the M-D stores (`KernelCtx::memory` —
//! ADR-029's open hole, which must consult kernel-owned policy and must NOT take a
//! caller-supplied [`Grants`]); the model / scheduler / policy handles; and the
//! out-of-process Provider path (ME-3).

pub mod http;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use agent24_protocol::{EventBody, ModuleEventPayload};
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum DomainError {
    #[error("manifest: {0}")]
    Manifest(String),
    #[error("manifest too large: {0}")]
    ManifestTooLarge(String),
    #[error("invalid event: {0}")]
    InvalidEvent(String),
    #[error("module store: {0}")]
    Store(String),
}

pub type Result<T> = std::result::Result<T, DomainError>;

/// A kernel capability a domain OS may request in its manifest. The kernel
/// grants a SUBSET; a capability that was not granted must be unreachable — see
/// [`KernelCtx`], where an ungranted capability has no handle at all.
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
    /// A SCOPE-LIMITED memory handle (M-D stores). Not ambient. Lands with
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
    /// manifest can say so; the transport is later, and the in-process mounter
    /// must refuse it (see [`DomainOsManifest::is_mountable_in_process`]).
    OutOfProcessProvider,
}

/// The wire shape of `domain-os.yml`. PRIVATE, and the only MANIFEST type that
/// derives `Deserialize`, so a caller cannot skip validation by deserializing
/// straight into the validated type. `deny_unknown_fields` turns a typo like
/// `kernel_capabilites` into an error instead of a silently-empty capability set.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    name: String,
    version: String,
    route_namespace: String,
    event_module: String,
    data_dir: String,
    #[serde(default)]
    requires_models: Vec<String>,
    #[serde(default)]
    requires_apis: Vec<String>,
    #[serde(default)]
    requires_deps: Vec<String>,
    #[serde(default)]
    kernel_capabilities: Vec<Capability>,
    #[serde(default)]
    ui_entry: Option<String>,
    impl_kind: ImplKind,
}

/// A VALIDATED `domain-os.yml`.
///
/// Fields are private and there is no `Deserialize` derive: every value of this
/// type has passed validation, so the kernel's mount path can rely on the identity
/// invariants without re-checking them — and no caller can produce a manifest that
/// skipped the checks.
///
/// `name` is the single source of identity: the route namespace, the event module
/// name and the data directory are all DERIVED from it, and a manifest that
/// declares any of them differently is rejected.
///
/// Deliberately NOT `Serialize`: the three derived wire fields are validated and
/// then discarded, so a derived `Serialize` would emit a document `from_yaml`
/// rejects — a broken round-trip on a type whose whole point is to represent a
/// valid `domain-os.yml`. Add it back only as a hand-written impl that
/// reconstructs all three.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainOsManifest {
    name: String,
    version: String,
    requires_models: Vec<String>,
    requires_apis: Vec<String>,
    requires_deps: Vec<String>,
    kernel_capabilities: Vec<Capability>,
    ui_entry: Option<String>,
    impl_kind: ImplKind,
}

/// Names that are not usable as a directory on Windows regardless of extension.
const RESERVED_NAMES: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Conservative cap: the name becomes one path SEGMENT, and most filesystems cap
/// a segment at 255 bytes. 64 leaves room for any suffix and keeps a manifest from
/// validating only to fail at mkdir.
const MAX_NAME_BYTES: usize = 64;

/// Lowercase ASCII alphanumerics plus `-`/`_`, starting alphanumeric, bounded, and
/// not a reserved device name. Deliberately stricter than the event schema's
/// pattern because the name is also a URL segment and a DIRECTORY. Restricting to
/// ASCII also removes Unicode-normalization aliasing (`é` vs `e` + U+0301), which
/// would otherwise let two distinct names land on one directory.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_BYTES
        && name.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        && !RESERVED_NAMES.contains(&name)
}

impl DomainOsManifest {
    /// Cap on a manifest DOCUMENT already in memory. Today manifests are local
    /// files; when ME-2 accepts them from elsewhere the READER also needs a bound
    /// (read at most this + 1), which this constant cannot provide on its own.
    pub const MAX_YAML_BYTES: usize = 64 * 1024;

    /// The only legal `route_namespace` for a module called `name`.
    pub fn declared_namespace(name: &str) -> String {
        format!("/api/v1/{name}")
    }

    /// The only legal `data_dir` STRING for a module called `name`.
    pub fn declared_data_dir(name: &str) -> String {
        format!("~/.agent24/os/{name}/")
    }

    /// Parse and VALIDATE a `domain-os.yml`. This is the sole constructor.
    pub fn from_yaml(yaml: &str) -> Result<Self> {
        if yaml.len() > Self::MAX_YAML_BYTES {
            return Err(DomainError::ManifestTooLarge(format!(
                "{} bytes exceeds the {} byte limit",
                yaml.len(),
                Self::MAX_YAML_BYTES
            )));
        }
        let raw: RawManifest =
            serde_yaml::from_str(yaml).map_err(|e| DomainError::Manifest(e.to_string()))?;

        if !valid_name(&raw.name) {
            return Err(DomainError::Manifest(format!(
                "invalid module name {:?}: 1-{MAX_NAME_BYTES} chars of [a-z0-9][a-z0-9_-]*, \
                 not a reserved device name",
                raw.name
            )));
        }
        if raw.version.trim().is_empty() {
            return Err(DomainError::Manifest("version must not be empty".into()));
        }
        let expected_ns = Self::declared_namespace(&raw.name);
        if raw.route_namespace != expected_ns {
            return Err(DomainError::Manifest(format!(
                "route_namespace {:?} must be exactly {:?} (derived from name)",
                raw.route_namespace, expected_ns
            )));
        }
        if raw.event_module != raw.name {
            return Err(DomainError::Manifest(format!(
                "event_module {:?} must equal name {:?} — a module may not emit \
                 events in another module's name",
                raw.event_module, raw.name
            )));
        }
        // EXACT equality, like the two rules above. A `contains` check let
        // `name: cos` pass with `data_dir: ~/.agent24/os/cos72/` — pointing at a
        // SIBLING OS's directory, which is precisely the contamination this
        // contract exists to prevent (review #127 B1).
        let expected_dir = Self::declared_data_dir(&raw.name);
        if raw.data_dir != expected_dir {
            return Err(DomainError::Manifest(format!(
                "data_dir {:?} must be exactly {:?} (derived from name)",
                raw.data_dir, expected_dir
            )));
        }

        Ok(Self {
            name: raw.name,
            version: raw.version,
            requires_models: raw.requires_models,
            requires_apis: raw.requires_apis,
            requires_deps: raw.requires_deps,
            kernel_capabilities: raw.kernel_capabilities,
            ui_entry: raw.ui_entry,
            impl_kind: raw.impl_kind,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    /// Derived, never stored: `/api/v1/<name>`.
    pub fn route_namespace(&self) -> String {
        Self::declared_namespace(&self.name)
    }

    /// Derived, never stored: equals [`name`](Self::name) by construction.
    pub fn event_module(&self) -> &str {
        &self.name
    }

    pub fn requires_models(&self) -> &[String] {
        &self.requires_models
    }

    pub fn requires_apis(&self) -> &[String] {
        &self.requires_apis
    }

    pub fn requires_deps(&self) -> &[String] {
        &self.requires_deps
    }

    pub fn kernel_capabilities(&self) -> &[Capability] {
        &self.kernel_capabilities
    }

    pub fn ui_entry(&self) -> Option<&str> {
        self.ui_entry.as_deref()
    }

    pub fn impl_kind(&self) -> ImplKind {
        self.impl_kind
    }

    /// The module's directory under `root`, derived LEXICALLY from the validated
    /// name. The manifest's declared `data_dir` string is checked for exact
    /// equality and then DISCARDED — this type does not keep it — so a field that
    /// lies cannot redirect anything.
    ///
    /// **This is a lexical guarantee only.** It does not stop two module
    /// directories from being symlinks to one place, and it cannot stop an
    /// implementation of [`DomainModule::open_store`] from ignoring the path it is
    /// handed. The mounter (ME-1b) creates the directory and DECLINES one that is
    /// already a symlink — degrading that module to 503 rather than letting two
    /// domain OSes resolve to one store. That catches the case that occurs in
    /// practice, but it is a check, not symlink-safe traversal: an ancestor may
    /// still be a link and the check is TOCTOU-prone. Real isolation needs `openat`-style directory
    /// handles; it is tracked, not claimed. (Do not "harden" this with
    /// `canonicalize` either: it resolves only when root and target already exist —
    /// false on first start — and is TOCTOU-prone in the same way.)
    pub fn data_dir_under(&self, root: &Path) -> PathBuf {
        root.join(&self.name)
    }

    /// Whether the kernel's IN-PROCESS mount path may load this module. An
    /// `OutOfProcessProvider` manifest parses fine (the shape is part of the
    /// contract) but ME-3's transport does not exist yet, so the in-process
    /// mounter MUST refuse it rather than half-mount a config it cannot honor.
    pub fn is_mountable_in_process(&self) -> bool {
        matches!(self.impl_kind, ImplKind::InProcessCrate)
    }
}

/// What the kernel decided a module may use — **informational, not authority**.
///
/// Actual authority is possession of a kernel-created handle: [`KernelCtx::events`]
/// returns `None` when events were not granted, so an ungranted capability has no
/// object to call. This type records the decision — for logging, for `agent24 os`
/// output, and so a module can degrade gracefully — and must never be accepted
/// from a caller as proof that something is permitted. An in-process module can
/// build one; see the crate-level trust model for why that is not the boundary.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Grants {
    granted: BTreeSet<Capability>,
}

impl Grants {
    /// What the kernel gives a module that REQUESTED `requested`, given the kernel
    /// is `willing` to hand out at most those: the INTERSECTION. Least privilege
    /// runs both ways — requesting more than the kernel offers gains nothing, and
    /// a capability the kernel would give but nobody asked for is not granted.
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

    pub fn iter(&self) -> impl Iterator<Item = Capability> + '_ {
        self.granted.iter().copied()
    }
}

/// Where a module's events go. The kernel supplies the transport; only the kernel
/// holds the real one.
pub trait EventBroadcast: Send + Sync {
    fn send(&self, body: EventBody);
}

/// Longest acceptable event `kind`. Kinds are dotted names like
/// `"task.transitioned"`, not payloads.
const MAX_KIND_BYTES: usize = 96;

/// `[a-z0-9_-]+(\.[a-z0-9_-]+)+` — at least two non-empty ASCII segments.
///
/// The protocol documents `kind` as "dotted like a first-party name", and all five
/// kinds Sin90 emits today (`direction.created`, `block.created`,
/// `block.transitioned`, `proposal.submitted`, `proposal.applied`) match, so this
/// is the grammar in use rather than a new restriction. Merely banning whitespace
/// and control characters was not enough: `"."`, `"task..transitioned"`,
/// `"not-dotted"`, an emoji, and a U+202E bidi override all passed it, and a client
/// that splits the documented dotted name into segments breaks on the first three.
/// ASCII-only additionally rules out NFC/NFD pairs collapsing into one kind at a
/// normalizing client. (The fuller fix is a `ModuleEventKind` newtype next to the
/// schema in `agent24-protocol`; this is the check that belongs at the boundary
/// either way.)
fn valid_kind(kind: &str) -> bool {
    let mut segments = 0;
    for seg in kind.split('.') {
        if seg.is_empty()
            || !seg
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        {
            return false;
        }
        segments += 1;
    }
    segments >= 2
}

/// A module's ONLY way to emit events — bound to a VALIDATED manifest at
/// construction.
///
/// [`EventSink::emit`] takes a `kind` and an OBJECT payload and stamps the module
/// name itself: there is no parameter through which a module could misattribute an
/// event. Taking a [`DomainOsManifest`] rather than a free string closes the other
/// half — a kernel wiring mistake cannot mount Sin90 and hand it a sink named
/// `cos72`, and a name that never passed [`valid_name`] cannot reach the wire.
/// The sink's EXISTENCE is the capability: the kernel builds one only for a module
/// it granted [`Capability::Events`], so there is no "denied" branch to forget. See
/// the crate-level trust model for what this does and does not contain.
pub struct EventSink {
    module: String,
    out: Arc<dyn EventBroadcast>,
}

impl EventSink {
    /// Build the sink for `manifest`'s module. The name is taken from the
    /// manifest, never from a caller-supplied string.
    pub fn new(manifest: &DomainOsManifest, out: Arc<dyn EventBroadcast>) -> Self {
        Self {
            module: manifest.name().to_owned(),
            out,
        }
    }

    /// The module name every event from this sink carries.
    pub fn module(&self) -> &str {
        &self.module
    }

    /// Emit an event of `kind` with an object payload, attributed to this sink's
    /// module.
    ///
    /// The payload is a `Map` rather than a `Value` on purpose: the envelope
    /// requires an object, and coercing a non-object (an array, a string) to `{}`
    /// would silently DESTROY a module's data while returning `Ok`. The type makes
    /// that unrepresentable instead of relying on a caller to avoid it.
    ///
    /// `kind` is checked against [`valid_kind`] — the dotted grammar the protocol
    /// documents and every module event in the tree already uses — because clients
    /// dispatch on `(module, kind)` and some split the name into segments.
    pub fn emit(
        &self,
        kind: &str,
        payload: serde_json::Map<String, serde_json::Value>,
    ) -> Result<()> {
        if kind.len() > MAX_KIND_BYTES {
            return Err(DomainError::InvalidEvent(format!(
                "event kind must be at most {MAX_KIND_BYTES} bytes, got {}",
                kind.len()
            )));
        }
        if !valid_kind(kind) {
            return Err(DomainError::InvalidEvent(format!(
                "event kind {kind:?} must be dotted ASCII: \
                 [a-z0-9_-]+(.[a-z0-9_-]+)+, e.g. \"task.transitioned\""
            )));
        }
        self.out.send(EventBody::Module(ModuleEventPayload {
            module: self.module.clone(),
            kind: kind.to_owned(),
            payload,
        }));
        Ok(())
    }
}

/// What the kernel lends a module. Capability-scoped by SHAPE: an ungranted
/// capability has no handle, so there is nothing to call and no check to forget.
///
/// There is deliberately no `grants()` here. Handles are the authority, so a
/// second informational answer could only ever agree redundantly or DISAGREE — and
/// the natural reading of a disagreement is
/// `if ctx.grants().has(Events) { ctx.events().unwrap() }`, a panic. [`Grants`]
/// belongs in the kernel's mount report, not beside the handles it describes.
///
/// Only [`KernelCtx::events`] exists today; `models` / `scheduler` / `policy` /
/// `memory(scope)` land as their consumers do (ME-1b+). When `memory` arrives it
/// must consult kernel-owned policy — it must NOT take a caller-supplied
/// [`Grants`], which is informational only.
pub trait KernelCtx: Send + Sync {
    /// The module-scoped event sink, or `None` when [`Capability::Events`] was not
    /// granted. `None` is an expected outcome, not an error: a module that can run
    /// without events simply does. The REASON a capability was withheld belongs in
    /// the kernel's mount diagnostics, not in every handle lookup.
    fn events(&self) -> Option<&EventSink>;
}

/// A domain OS the kernel can mount without knowing its name.
///
/// The manifest is the module's SOLE identity — there is deliberately no `name()`
/// or `event_module()` accessor, because a trait method could return something
/// that disagrees with the validated manifest and manifest validation would never
/// see it. The kernel reads [`DomainOsManifest::name`] and derives the namespace,
/// the event module and the data directory from it.
#[async_trait::async_trait]
pub trait DomainModule: Send + Sync {
    /// This module's validated manifest — its identity, capabilities and kind.
    fn manifest(&self) -> &DomainOsManifest;

    /// Open this module's own store, running its own migrations.
    ///
    /// `dir` is the PERSISTENT location the kernel assigned it (derived via
    /// [`DomainOsManifest::data_dir_under`] and created before this call). A module
    /// configured for ephemeral operation may legitimately ignore it and open an
    /// in-memory store instead — that choice belongs to the module's constructor,
    /// not to this trait, which is why there is no mode parameter here.
    ///
    /// The trait cannot ENFORCE what happens on failure — an implementation is
    /// free to return `Err` and still hand back a router that answers 200 — so the
    /// rule is the MOUNTER's, and ME-1b owns it: on `Err`, do not use this module's
    /// router; nest a kernel-created 503 fallback under its namespace instead, and
    /// keep the kernel running. One failed domain OS must not take the daemon with
    /// it.
    async fn open_store(&self, dir: &Path) -> Result<()>;

    /// The module's routes, RELATIVE to its namespace (`/directions`, not
    /// `/api/v1/sin90/directions`). The kernel nests them under
    /// [`DomainOsManifest::route_namespace`], so a module need not spell its own
    /// prefix — and, whatever it spells, cannot mount outside that namespace.
    ///
    /// `Router<()>` means the module has already bound all of its OWN state; `ctx`
    /// is an `Arc` because handlers outlive this call and must keep it.
    ///
    /// **Mount order is security-relevant.** An axum layer applies only to routes
    /// already on the router, so nesting modules AFTER the kernel's auth layer
    /// leaves them unauthenticated. ME-1b must bind the kernel router's state
    /// first, nest every module, and apply kernel-owned auth LAST — with a test
    /// asserting a module route 401s without a token.
    fn routes(&self, ctx: Arc<dyn KernelCtx>) -> axum::Router;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::sync::Mutex;

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

    fn manifest(name: &str) -> DomainOsManifest {
        DomainOsManifest::from_yaml(&respell(name)).unwrap()
    }

    fn respell(name: &str) -> String {
        SIN90_YAML
            .replace("name: sin90", &format!("name: {name}"))
            .replace(
                "route_namespace: /api/v1/sin90",
                &format!("route_namespace: /api/v1/{name}"),
            )
            .replace("event_module: sin90", &format!("event_module: {name}"))
            .replace("os/sin90/", &format!("os/{name}/"))
    }

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

    fn obj(v: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
        match v {
            serde_json::Value::Object(m) => m,
            other => panic!("test payload must be an object, got {other}"),
        }
    }

    // ---------- manifest ----------

    #[test]
    fn parses_a_valid_manifest() {
        let m = DomainOsManifest::from_yaml(SIN90_YAML).unwrap();
        assert_eq!(m.name(), "sin90");
        assert_eq!(m.version(), "0.2.1");
        assert_eq!(m.route_namespace(), "/api/v1/sin90");
        assert_eq!(m.event_module(), "sin90");
        assert_eq!(m.impl_kind(), ImplKind::InProcessCrate);
        assert_eq!(m.kernel_capabilities(), &[Capability::Events]);
        assert_eq!(m.ui_entry(), None);
    }

    #[test]
    fn manifest_cannot_claim_another_modules_event_name() {
        let yaml = SIN90_YAML.replace("event_module: sin90", "event_module: cos72");
        let err = DomainOsManifest::from_yaml(&yaml).unwrap_err();
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
    fn a_prefix_name_cannot_claim_a_sibling_os_directory() {
        // The old `contains` check passed `name: cos` with `data_dir: .../cos72/`
        // — BOTH are real domain OSes here, so a prefix name could point at its
        // sibling's data. The sin90-vs-cos72 tests above cannot catch it: those
        // names share no prefix.
        let cases = [
            ("cos", "~/.agent24/os/cos72/"),
            ("sin90", "/tmp/evil/sin90"),
            ("sin90", "../../../etc/sin90"),
            ("sin90", "~/.agent24/os/other/sin90x"),
        ];
        for (name, dir) in cases {
            let yaml = respell(name).replace(
                &format!("data_dir: ~/.agent24/os/{name}/"),
                &format!("data_dir: {dir}"),
            );
            let err = DomainOsManifest::from_yaml(&yaml).unwrap_err();
            assert!(
                err.to_string().contains("data_dir"),
                "name={name} dir={dir} must be rejected, got: {err}"
            );
        }
    }

    #[test]
    fn rejects_names_that_are_unsafe_as_a_path_or_url_segment() {
        for bad in ["../evil", "a/b", "Sin90", "-lead", "has space", "dot.name"] {
            let yaml = respell(&format!("{bad:?}"));
            assert!(
                DomainOsManifest::from_yaml(&yaml).is_err(),
                "name {bad:?} must be rejected"
            );
        }
    }

    #[test]
    fn reserved_and_overlong_names_are_rejected() {
        for bad in ["con", "nul", "com1", "lpt9"] {
            assert!(!valid_name(bad), "{bad} is a reserved device name");
        }
        assert!(!valid_name(""));
        assert!(!valid_name(&"a".repeat(MAX_NAME_BYTES + 1)));
        assert!(valid_name(&"a".repeat(MAX_NAME_BYTES)));
        // ASCII-only rules out Unicode normalization aliasing on
        // normalization-insensitive filesystems.
        assert!(!valid_name("\u{e9}"));
        assert!(!valid_name("e\u{301}"));
    }

    #[test]
    fn a_typo_in_a_field_name_is_an_error_not_a_silent_default() {
        // `kernel_capabilites` used to be ignored as unknown while
        // `kernel_capabilities` defaulted to empty — a typo silently changing
        // behavior. The same shape would silently skip a `requires_models` check.
        let yaml = SIN90_YAML.replace("kernel_capabilities:", "kernel_capabilites:");
        assert!(DomainOsManifest::from_yaml(&yaml).is_err());
    }

    #[test]
    fn a_complete_manifest_with_a_bad_name_still_fails_name_validation() {
        // Deliberately COMPLETE except for the name: an earlier version of this
        // test passed `"name: ../evil"`, which serde rejects for missing fields —
        // so it would have passed even with name validation deleted.
        let yaml = respell("../evil");
        let err = DomainOsManifest::from_yaml(&yaml).unwrap_err();
        assert!(
            err.to_string().contains("invalid module name"),
            "must fail on the NAME, not on a missing field: {err}"
        );
        // The same document with a good name parses, proving nothing else in it
        // was the reason.
        assert!(DomainOsManifest::from_yaml(&respell("sin90")).is_ok());
    }

    #[test]
    fn the_directory_is_derived_from_the_name_not_the_declared_string() {
        let m = DomainOsManifest::from_yaml(SIN90_YAML).unwrap();
        assert_eq!(
            m.data_dir_under(Path::new("/var/lib/agent24/os")),
            PathBuf::from("/var/lib/agent24/os/sin90")
        );
        // Distinct validated names never share a directory, a namespace or an
        // event module.
        let c = DomainOsManifest::from_yaml(&respell("cos72")).unwrap();
        let root = Path::new("/r");
        assert_ne!(m.data_dir_under(root), c.data_dir_under(root));
        assert_ne!(m.route_namespace(), c.route_namespace());
        assert_ne!(m.event_module(), c.event_module());
    }

    #[test]
    fn out_of_process_manifest_parses_but_is_not_in_process_mountable() {
        let yaml = SIN90_YAML.replace(
            "impl_kind: in_process_crate",
            "impl_kind: out_of_process_provider",
        );
        let m = DomainOsManifest::from_yaml(&yaml).unwrap();
        assert!(!m.is_mountable_in_process());
        assert!(
            DomainOsManifest::from_yaml(SIN90_YAML)
                .unwrap()
                .is_mountable_in_process()
        );
    }

    #[test]
    fn oversized_manifest_is_rejected_before_it_is_parsed() {
        // The oversized document is deliberately INVALID YAML: an implementation
        // that parsed first and checked size afterwards would surface a
        // `Manifest` parse error instead, so this pins the ORDER, not just that
        // something failed.
        let huge = format!(
            "{{{{{{ not yaml {}",
            "x".repeat(DomainOsManifest::MAX_YAML_BYTES)
        );
        let err = DomainOsManifest::from_yaml(&huge).unwrap_err();
        assert!(
            matches!(err, DomainError::ManifestTooLarge(_)),
            "size must be checked before parsing, got: {err}"
        );
    }

    // ---------- grants ----------

    #[test]
    fn grants_are_the_intersection_in_both_directions() {
        let g = Grants::granting(
            &[Capability::Events, Capability::Models, Capability::Memory],
            &[Capability::Events],
        );
        assert!(g.has(Capability::Events));
        assert!(!g.has(Capability::Models), "requesting does not grant");
        assert!(!g.has(Capability::Memory));

        let g = Grants::granting(
            &[Capability::Events],
            &[
                Capability::Events,
                Capability::Models,
                Capability::Scheduler,
            ],
        );
        assert!(g.has(Capability::Events));
        assert!(!g.has(Capability::Models), "unrequested is not granted");
        assert_eq!(g.iter().count(), 1);
    }

    // ---------- event sink ----------

    #[test]
    fn the_sinks_module_comes_from_the_manifest_not_a_caller_string() {
        // The constructor takes a VALIDATED manifest, so a kernel wiring mistake
        // cannot mount sin90 with a sink named cos72, and a name that never passed
        // valid_name cannot reach the wire.
        let bus = Arc::new(RecordingBus::default());
        let sink = EventSink::new(&manifest("sin90"), bus.clone());
        assert_eq!(sink.module(), "sin90");
        sink.emit("task.transitioned", obj(serde_json::json!({"id": "t1"})))
            .unwrap();
        assert_eq!(
            module_events(&bus),
            vec![("sin90".to_owned(), "task.transitioned".to_owned())]
        );

        let other = EventSink::new(&manifest("cos72"), bus.clone());
        assert_eq!(other.module(), "cos72");
    }

    #[test]
    fn emit_cannot_override_the_sinks_module() {
        // Named for what it actually proves: `emit` has no module parameter, so a
        // kind that LOOKS like another module's is still stamped as this one's. It
        // does NOT prove a module cannot construct its own sink — see the
        // crate-level trust model, where the transport is the boundary.
        let bus = Arc::new(RecordingBus::default());
        let sink = EventSink::new(&manifest("sin90"), bus.clone());
        sink.emit("cos72.stolen", obj(serde_json::json!({})))
            .unwrap();
        assert_eq!(module_events(&bus)[0].0, "sin90");
    }

    #[test]
    fn a_payload_key_named_module_does_not_shadow_the_envelope() {
        let bus = Arc::new(RecordingBus::default());
        let sink = EventSink::new(&manifest("sin90"), bus.clone());
        sink.emit("a.b", obj(serde_json::json!({"module": "cos72"})))
            .unwrap();
        match &bus.sent.lock().unwrap()[0] {
            EventBody::Module(m) => {
                assert_eq!(m.module, "sin90");
                assert_eq!(m.payload["module"], serde_json::json!("cos72"));
            }
            other => panic!("expected Module, got {other:?}"),
        }
    }

    #[test]
    fn a_malformed_kind_is_rejected_rather_than_emitted() {
        let bus = Arc::new(RecordingBus::default());
        let sink = EventSink::new(&manifest("sin90"), bus.clone());
        let overlong = format!("a.{}", "x".repeat(MAX_KIND_BYTES));
        let bad = [
            "",                     // empty
            " task",                // whitespace
            "task\ntransitioned",   // newline
            "a\u{0}b",              // control character
            ".",                    // two empty segments
            "task..transitioned",   // empty inner segment
            "task.",                // trailing dot
            "not-dotted",           // single segment; the protocol says dotted
            "Task.Transitioned",    // uppercase
            "task.tr\u{e9}s",       // non-ASCII: NFC/NFD would alias at a client
            "task.\u{202e}spoofed", // bidi override
            "\u{1f4a9}.x",          // emoji
            &overlong,
        ];
        for k in bad {
            let err = sink.emit(k, serde_json::Map::new()).unwrap_err();
            assert!(matches!(err, DomainError::InvalidEvent(_)), "{k:?}: {err}");
        }
        assert!(module_events(&bus).is_empty(), "nothing may be emitted");

        // The five kinds Sin90 emits today must all still pass, or this rule is a
        // regression dressed as a check.
        let good = [
            "direction.created",
            "block.created",
            "block.transitioned",
            "proposal.submitted",
            "proposal.applied",
            "a.b.c",
            "task_1.sub-step",
        ];
        for k in good {
            sink.emit(k, serde_json::Map::new())
                .unwrap_or_else(|e| panic!("{k:?} must be accepted: {e}"));
        }
        assert_eq!(module_events(&bus).len(), good.len());
    }

    #[test]
    fn an_empty_object_payload_is_preserved() {
        // Named for what it checks. The property "a non-object payload is
        // unrepresentable" is a COMPILE-TIME one — `emit` takes a `Map`, so
        // `json!(["critical","data"])` cannot be passed at all — and a runtime test
        // cannot demonstrate it. (The earlier `Value` signature accepted it and
        // emitted `{}` while returning Ok: silent data loss. `apps/agent24d/src/
        // sin90.rs::emit` still has that shape and loses it when ME-1b moves Sin90
        // behind this contract.)
        let bus = Arc::new(RecordingBus::default());
        let sink = EventSink::new(&manifest("sin90"), bus.clone());
        sink.emit("a.b", serde_json::Map::new()).unwrap();
        match &bus.sent.lock().unwrap()[0] {
            EventBody::Module(m) => assert!(m.payload.is_empty()),
            other => panic!("expected Module, got {other:?}"),
        }
    }
}
