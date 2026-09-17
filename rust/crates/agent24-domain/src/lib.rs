//! Agent24 domain-OS contract (M-E / ME-1; ADR-029 内核↔领域 OS 边界).
//!
//! Agent24 is a BASE: one kernel, plus a swappable **domain OS** (Sin90 today,
//! Cos72 or a third-party one tomorrow). This crate holds the contract between
//! them — types and traits only, no kernel and no module — so the dependency
//! arrow stays one-way and the kernel's MOUNTER can mount a module **without
//! knowing its name**, which is the ME-1 acceptance. (The daemon's composition
//! root still names the OS it installs — someone has to say which one is
//! there — but nothing downstream of that does.)
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
//! [`memory`] carries the fifth: [`ScopedMemory`](memory::ScopedMemory), the
//! capability-limited view of the shared memory base that closes ADR-029's open
//! hole — until it existed, two domain OSes under one owner shared that base.
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
//! **What is deliberately NOT here yet** (documented rather than faked): the
//! model / scheduler / policy handles, and the out-of-process Provider path
//! (ME-3). The kernel-side registry landed in ME-1b-a
//! (`agent24d::domain`) and Sin90 moved behind this contract in ME-1b-b, so
//! `AppState.sin90` and the hardcoded `/api/v1/sin90/*` routes are gone.

pub mod http;
pub mod memory;

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
    /// The manifest declares a schema version, or a minimum daemon protocol,
    /// that THIS build does not support.
    ///
    /// Distinct from [`Self::Manifest`] ON PURPOSE. A future manifest hitting an
    /// old daemon is not a malformed document — it is a version mismatch, and the
    /// operator needs to be told which side is behind. Folding it into a generic
    /// serde error is what ME-3's gate 6 exists to prevent: the strict
    /// `RawManifest` shape would reject an unknown field with a message about that
    /// field, never mentioning that the daemon is simply too old.
    #[error(
        "manifest requires {requirement} (this daemon supports {supported}) — \
         module {module:?} needs a newer agent24d"
    )]
    ManifestUnsupported {
        module: String,
        requirement: String,
        supported: String,
    },
    #[error("invalid event: {0}")]
    InvalidEvent(String),
    #[error("module store: {0}")]
    Store(String),
    /// A [`memory`] request the kernel refused — an empty `kind`, or a `kind`/body
    /// over the per-memory size cap. Distinct from [`Self::Store`], which is the
    /// base failing rather than the request being unacceptable.
    #[error("memory: {0}")]
    Memory(String),
}

pub type Result<T> = std::result::Result<T, DomainError>;

/// A kernel capability a domain OS may request in its manifest. The kernel
/// grants a SUBSET; a capability that was not granted must be unreachable — see
/// [`KernelCtx`], where an ungranted capability has no handle at all.
/// `non_exhaustive` because this list grows: `Approval` lands with ME-3e, and
/// `Models` / `Scheduler` / `Policy` become real when their handles do. Without
/// it, every added variant breaks an exhaustive `match` in any crate outside this
/// one — a source-compatibility break shipped silently, because nothing in this
/// repository matches exhaustively and CI therefore cannot see it.
///
/// It does NOT derive `Deserialize`. Reading a manifest with a strict enum turns
/// an unrecognised capability into a serde message about a variant, which cannot
/// carry the name of the offending capability in a form a caller can act on. See
/// [`Capability::parse`] and `RawManifest`'s `kernel_capabilities`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
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
    /// Submit `_a24/approval/gate`/`advise` and query `_a24/approval/status`
    /// (T7b/ME-3e). Lands with `KernelCtx::approval`.
    Approval,
}

/// Every capability this build knows, in declaration order. The single place the
/// list is written down, so `parse` and the `supported` list in an error cannot
/// drift apart.
pub const ALL_CAPABILITIES: &[Capability] = &[
    Capability::Events,
    Capability::Models,
    Capability::Scheduler,
    Capability::Policy,
    Capability::Memory,
    Capability::Approval,
];

/// A capability string a manifest asked for that this build does not know.
///
/// A struct rather than a message, because the operator's next question is always
/// "which one, and what were the choices?" — and a serde variant error can answer
/// neither in a form a caller can read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownCapability {
    /// Exactly what the manifest said, unmodified. A typo is only findable if the
    /// error shows the typo.
    pub capability: String,
    pub supported: Vec<&'static str>,
}

impl std::fmt::Display for UnknownCapability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "unknown_capability: {:?} is not one of {}",
            self.capability,
            self.supported.join(", ")
        )
    }
}

impl Capability {
    /// Map a manifest string onto a capability, naming what was wrong if it is not
    /// one. Deliberately not `Deserialize`: serde's variant error says which
    /// variants exist but not which STRING was rejected in a shape a caller can
    /// use, and a manifest full of capabilities gives no clue which one it meant.
    pub fn parse(s: &str) -> std::result::Result<Self, UnknownCapability> {
        ALL_CAPABILITIES
            .iter()
            .copied()
            .find(|c| c.as_str() == s)
            .ok_or_else(|| UnknownCapability {
                capability: s.to_owned(),
                supported: ALL_CAPABILITIES.iter().map(|c| c.as_str()).collect(),
            })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Capability::Events => "events",
            Capability::Models => "models",
            Capability::Scheduler => "scheduler",
            Capability::Policy => "policy",
            Capability::Memory => "memory",
            Capability::Approval => "approval",
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

/// How to start an out-of-process module.
///
/// # Why `command` + `args`, and not one string or one path
///
/// The alternative — a single executable path — forces every module NOT written
/// in a compiled language to ship a wrapper script (`#!/bin/sh` + `exec node
/// server.js`). That does not remove the indirection, it moves it somewhere
/// harder to review: the kernel then reads a manifest that names a shell script
/// whose contents nobody validated, instead of a manifest that says `node
/// server.js` in the open. **Making "which language is this written in" into
/// something authors have to work around produces workarounds that are worse
/// than the field.**
///
/// # This field is an execution boundary
///
/// Whoever can write a manifest can name a program the daemon will run. Nothing
/// here changes that — it is inherent to spawning a module at all. What follows
/// from it is a requirement OUTSIDE this type: the packages root must be a
/// directory only the user can write (see FU-41). A validation rule here cannot
/// substitute for that, and pretending otherwise would be the more dangerous
/// error, because the rule would carry a name saying it was checked.
///
/// What validation here DOES do is narrow what a manifest can point at: see
/// [`SpawnCommand::validate`].
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SpawnCommand {
    /// The program. Either a **bare name** resolved on `PATH` (`node`,
    /// `python3`), or a path **relative to the package directory**
    /// (`bin/my-module`). Absolute paths and any `..` component are refused —
    /// see [`SpawnCommand::validate`].
    pub command: String,
    /// Arguments, passed verbatim. Never shell-interpreted: the kernel executes the
    /// program directly, so quoting, globbing and `;` have no meaning here.
    #[serde(default)]
    pub args: Vec<String>,
}

impl SpawnCommand {
    /// What a manifest may point at.
    ///
    /// Refused: an empty command; an **absolute path**; any component that is
    /// `..`. Allowed: a bare name (resolved on `PATH`) or a relative path inside
    /// the package.
    ///
    /// The rule is not a security boundary — see the type's docs. What it
    /// actually provides is narrower than an earlier version of this comment
    /// claimed, and the narrower statement is the honest one: **the manifest
    /// contains no spelled-out path escape.**
    ///
    /// It does NOT establish that a package can be reviewed by reading it, and
    /// two things defeat that reading (both measured in review):
    ///
    /// - **It distinguishes by spelling, not by target.** `/bin/sh` is refused
    ///   while `sh` is accepted — the same program. `env` is worse: `command:
    ///   env` with `args: [FOO=1, sh, -c, …]` is entirely legal. Bare-name
    ///   resolution through `PATH` is deliberate (it is what lets `node` work),
    ///   so this is not a defect in the rule; `/bin/sh` was simply a bad example
    ///   of what the rule buys.
    /// - **A symlink inside the package is invisible to it.** `command:
    ///   bin/node` passes while `bin/node` points at `/bin/sh`, because this
    ///   check is purely lexical.
    ///
    /// Neither gives an attacker anything new — whoever can write the manifest
    /// can write the package directory too. What they take away is the BENEFIT
    /// this rule was said to provide.
    ///
    /// **The load-bearing check lives in `agent24_os_proto::launch::resolve`**,
    /// where the path is real: a relative command is canonicalised and must still
    /// lie under the canonicalised package directory, so `bin/node` pointing at
    /// `/bin/sh` is refused there. Named here rather than described as a future
    /// requirement — a cross-module requirement that lives only in the
    /// docstring of the module that cannot enforce it is a requirement nobody is
    /// holding.
    ///
    /// # Errors
    ///
    /// A message naming the offending value.
    pub fn validate(&self) -> std::result::Result<(), String> {
        if self.command.trim().is_empty() {
            return Err("spawn.command is empty".to_owned());
        }
        let path = Path::new(&self.command);
        if path.is_absolute() {
            return Err(format!(
                "spawn.command {:?} is an absolute path; use a bare name (resolved \
                 on PATH) or a path relative to the package",
                self.command
            ));
        }
        if path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(format!(
                "spawn.command {:?} escapes the package with `..`",
                self.command
            ));
        }
        Ok(())
    }
}

/// The `domain-os.yml` SCHEMA version this build understands.
///
/// A manifest that omits `manifest_version` is treated as **v1** — every manifest
/// written before this field existed is a v1 manifest, and making the field
/// required would break every one of them at once.
pub const MANIFEST_SCHEMA_VERSION: u32 = 1;

/// The kernel↔module PROTOCOL version this build speaks. Separate from the schema
/// version: a manifest can be v1 while the protocol moves, and vice versa.
pub const DAEMON_PROTOCOL_VERSION: u32 = 1;

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
    /// Collected as STRINGS and mapped afterwards. Deserializing straight into
    /// `Vec<Capability>` makes an unrecognised entry a serde variant error, which
    /// cannot be turned into [`UnknownCapability`] — the string it rejected is not
    /// recoverable from the message.
    #[serde(default)]
    kernel_capabilities: Vec<String>,
    #[serde(default)]
    ui_entry: Option<String>,
    impl_kind: ImplKind,
    #[serde(default)]
    spawn: Option<SpawnCommand>,
    /// Declared here ONLY so `deny_unknown_fields` does not reject the very
    /// fields step one just read. Their values are consumed by
    /// [`ManifestEnvelope`]; re-reading them here would be reading the same
    /// document twice and proving nothing, so they are deliberately never used.
    ///
    /// `allow(dead_code)` rather than `_`-prefixing: the names must match the YAML
    /// keys for `deny_unknown_fields` to accept them, and a rename attribute to
    /// achieve that would hide which key each one guards.
    #[serde(default)]
    #[allow(dead_code)]
    manifest_version: Option<u32>,
    #[serde(default)]
    #[allow(dead_code)]
    min_daemon_protocol: Option<u32>,
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
    spawn: Option<SpawnCommand>,
}

/// Names that are not usable as a directory on Windows regardless of extension.
const RESERVED_NAMES: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Conservative cap: the name becomes one path SEGMENT, and most filesystems cap
/// a segment at 255 bytes. 64 leaves room for any suffix and keeps a manifest from
/// validating only to fail at mkdir.
/// Longest a module name may be, in bytes.
///
/// Public because a test in another crate needs the REAL bound rather than a
/// guessed one: `agent24-os-proto` measures the largest possible `initialize`
/// frame against the frame limit, and an over-wide guess there eats slack that
/// does not exist — which turns into a spurious red over a name that can never
/// occur, and then someone edits the test. Exporting it means that measurement
/// follows this constant if it ever changes.
pub const MAX_NAME_BYTES: usize = 64;

/// Lowercase ASCII alphanumerics plus `-`/`_`, starting alphanumeric, bounded, and
/// not a reserved device name. Deliberately stricter than the event schema's
/// pattern because the name is also a URL segment and a DIRECTORY. Restricting to
/// ASCII also removes Unicode-normalization aliasing (`é` vs `e` + U+0301), which
/// would otherwise let two distinct names land on one directory.
/// Whether `name` is usable as a domain-OS identity, WITHOUT parsing a manifest.
///
/// The kernel's catalogue names modules before it constructs them (so a module
/// whose constructor fails still has a name to switch off), and that name becomes
/// a URL segment and a directory the same way a manifest's does. Exposing the rule
/// is what stops the two from drifting: an unvalidated catalogue entry could
/// otherwise mount `bad/name` as a namespace that no manifest would ever be
/// allowed to claim.
pub fn is_valid_module_name(name: &str) -> bool {
    valid_name(name)
}

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
        // Parse the TEXT exactly once, into an untyped tree; both steps below read
        // that tree. Two `from_str` calls would be two parses, and YAML parsing is
        // not free on hostile input: a 446-byte alias-expansion bomb measured
        // ~171ms per parse on this machine (serde_yaml rejects it — after doing
        // the work), so parsing twice doubles what an attacker gets for a document
        // well under `MAX_YAML_BYTES`.
        //
        // It also removes a question this design would otherwise have to answer:
        // whether two independent parses of the same text are guaranteed to agree.
        // Reading one tree twice, they provably are.
        // NOTE for the disk-loading commit (ME-3a's next piece): a BOM is a
        // BYTE-level artefact, and this strip only covers the string that reaches
        // this function. Today the only manifest source is `include_str!`, so the
        // bytes arrive at compile time and this is enough. The moment a manifest
        // is read from disk at runtime, that path needs its own BOM test through
        // the real loader — a `format!("{BOM}{yaml}")` unit test knows nothing
        // about `fs::read` + `from_utf8` or a `BufReader` in between. A UTF-16 BOM
        // (FF FE) never reaches here at all: it fails earlier, in UTF-8 decoding,
        // and deserves its own readable reason on that path.
        //
        // A UTF-8 BOM makes serde_yaml report "containing more than one document
        // is not supported" — a sentence with nothing to do with the actual
        // problem, and one a reader cannot act on. Windows editors write a BOM by
        // default, so this is the likeliest way a hand-written manifest fails.
        // Strip it: this gate exists to produce READABLE reasons, and letting the
        // commonest authoring accident produce the least readable message defeats
        // it. (Found by a library-level probe, then reproduced through this
        // function.)
        let tree: serde_yaml::Value = serde_yaml::from_str(yaml.trim_start_matches('\u{feff}'))
            .map_err(|e| DomainError::Manifest(e.to_string()))?;

        // ---- step one: TOLERANT read, only to reach the version gate ----
        //
        // Order is load-bearing. Deserializing the strict shape first means a
        // manifest from the future dies on an unknown field, with a message about
        // that field and no hint that the daemon is the thing that is out of date.
        //
        // Plain key lookups rather than a typed envelope struct: a struct would
        // need `deny_unknown_fields` off to survive a future document, and then it
        // would be a second shape to keep in sync with the first. Three lookups
        // cannot drift.
        // ABSENT and MALFORMED are different answers, and collapsing them is the
        // very failure this gate exists to prevent: `as_u64()` returns `None` for
        // a string, so `manifest_version: "3"` would read as "absent" → default 1
        // → PASS the gate → then die in the strict shape on a type error naming
        // the field. That is exactly the unreadable outcome gate 6 removes.
        let field_u32 = |k: &str| -> Result<Option<u64>> {
            match tree.get(k) {
                // ABSENT only. `key:` with no value is PRESENT-but-empty, and it
                // falls through to the error arm below — the last place this
                // gate's own principle was still collapsing. Nobody writes an
                // empty value to mean "v1": omitting the key already means that
                // and is shorter, so in practice an empty value is a slip or a
                // template's unfilled slot, and both want to be told now. (If a
                // generator ever emits `key:` to mean "filled in later", that
                // belongs on the generating side — the kernel must not read it
                // as 1.)
                None => Ok(None),
                Some(v) => v.as_u64().map(Some).ok_or_else(|| {
                    DomainError::Manifest(format!("{k} must be a non-negative integer, got {v:?}"))
                }),
            }
        };
        let module = tree
            .get("name")
            .and_then(serde_yaml::Value::as_str)
            // A manifest too broken to yield a name still has to produce a message
            // better than a serde error, so the gate names it `<unnamed>` rather
            // than refusing to run.
            .unwrap_or("<unnamed>")
            .to_owned();

        // This read must stay ahead of the dispatch — the dispatch needs the value.
        // A judgement call rides on that: `manifest_version: 1.0` is a YAML float,
        // and it is refused as "must be a non-negative integer" even though this
        // build supports v1. That is deliberate. The daemon does support v1; what it
        // cannot accept is a version field that is not an integer, and saying so
        // names the actual defect and the fix. Calling it `ManifestUnsupported`
        // would claim the version is unsupported, which is false.
        let declared_schema = field_u32("manifest_version")?.unwrap_or(1);

        // Collected BEFORE the tree is consumed below. Cheap: one pass over the
        // top-level map. Used only on the error path (see there for why).
        let null_keys: Vec<String> = tree
            .as_mapping()
            .map(|m| {
                m.iter()
                    .filter(|(_, v)| matches!(v, serde_yaml::Value::Null))
                    .filter_map(|(k, _)| k.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();

        // Versions are dispatched EXPLICITLY, not by `<= current`.
        //
        // A `> MANIFEST_SCHEMA_VERSION` gate used to sit above this and answer
        // FIRST, so the future-manifest case — the entire reason this match exists
        // — was still being decided by `<= current`, and the match only ever saw
        // `0`. Nothing could tell: deleting that gate broke no test, because the
        // one test for a future manifest ignored `supported` with `..`, and that is
        // the only field where the two answers differed ("v1" vs "v1..=v1"). The
        // gate is gone and that assertion is now made.
        //
        // This is also what refuses `0`: versions start at 1, so `0` is not "older than v1" — there
        // is no v1-minus, and a document declaring it means something this build
        // cannot know. A dedicated `== 0` check was written here first and then
        // removed: the match already rejected it, with a byte-identical message, so
        // the check could not fail in any way the match did not. (A mutation found
        // it — disabling the dedicated check killed no test, because the match was
        // catching the case all along.) Today v1 is the
        // only one, so the match has a single arm — but writing it as a match is
        // the point: when v2 arrives it gets its own arm and its own struct, rather
        // than v1 documents being quietly fed to whatever `RawManifest` has become.
        // A shape that says "anything not newer than me is mine" cannot survive a
        // field being renamed or retyped.
        match declared_schema {
            1 => {}
            other => {
                return Err(DomainError::ManifestUnsupported {
                    module,
                    requirement: format!("manifest schema v{other}"),
                    supported: format!("v1..=v{MANIFEST_SCHEMA_VERSION}"),
                });
            }
        }

        // The protocol gate reads `min_daemon_protocol` — a field of THIS document
        // — so it can only run once the version dispatch above has accepted the
        // document's schema. It used to run BEFORE the dispatch, which had two
        // consequences, and the second is the one that matters:
        //
        //  - A future manifest with a malformed `min_daemon_protocol` was told its
        //    field was the wrong type, i.e. that it was a malformed document —
        //    contradicting `ManifestUnsupported`'s own documentation, which says a
        //    future manifest hitting an old daemon is NOT malformed.
        //  - More fundamentally: reading that field out of a v7 document assumes v7
        //    still spells it this way and still types it this way. This build was in
        //    the middle of announcing that it cannot read v7. That is precisely the
        //    reason §3 gives for why `<= current` cannot survive a field being
        //    renamed or retyped — except it was not in the `<= current`, it was
        //    three lines above it.
        if let Some(min) = field_u32("min_daemon_protocol")?
            && min > u64::from(DAEMON_PROTOCOL_VERSION)
        {
            return Err(DomainError::ManifestUnsupported {
                module,
                requirement: format!("kernel protocol >= {min}"),
                supported: format!("<= {DAEMON_PROTOCOL_VERSION}"),
            });
        }

        // ---- step two: the STRICT shape, now that the version is known-good ----
        let raw: RawManifest = serde_yaml::from_value(tree).map_err(|e| {
            // `from_value` is the RIGHT deserializer here but it has one real
            // cost: its errors carry no line/column, because the tree it walks has
            // no positions. For a gate whose whole purpose is readable reasons,
            // losing "at line 3 column 1" hurts.
            //
            // So on the ERROR PATH ONLY, re-read the text with `from_str` purely to
            // borrow a better-located message. Two rules keep this safe:
            //
            //  - If `from_str` ACCEPTS what `from_value` rejected, discard it and
            //    keep our error. The two disagree in ways where `from_str` is the
            //    LOOSER one — it turns `name: ~` into the literal string "~", and
            //    accepts `!!str 2` for a u32. Adopting its verdict would undo the
            //    strictness this path was chosen for; we only ever borrow its prose.
            //  - The cost is bounded: an expansion bomb never reaches here, because
            //    it already failed at the `from_str::<Value>` above. Only documents
            //    that parsed cleanly and then failed the SHAPE get the second read.
            if let Some(located) =
                serde_yaml::from_str::<RawManifest>(yaml.trim_start_matches('\u{feff}'))
                    .err()
                    .map(|located| located.to_string())
            {
                // Both facts are true and they do not compete: the borrowed
                // message locates the FIRST thing serde tripped on, while a null
                // field further up may be the reason the author is here at all.
                // Reporting only the borrowed one sends them round a second lap.
                return DomainError::Manifest(if null_keys.is_empty() {
                    located
                } else {
                    format!("{located} — null-valued field(s): {}", null_keys.join(", "))
                });
            }
            // Nothing to borrow. This is not the rare case — it is exactly the
            // case that needs help most, because `from_str` only has a message to
            // lend when IT also refuses, and it is the LOOSER of the two. So the
            // situations where `from_value` is stricter are precisely the ones
            // where its bare message stands alone, and that message names no
            // field: `name: ~` alone yields "invalid type: unit value, expected a
            // string" — no line, no field, in a document with five string fields.
            //
            // Recover the field name from the tree instead. A YAML null is the one
            // value that reaches serde as a type error with nothing to identify it,
            // so listing the null-valued keys is enough to point at the culprit —
            // and it needs no second copy of the field list, which is the trap the
            // version gate was written to avoid.
            DomainError::Manifest(if null_keys.is_empty() {
                e.to_string()
            } else {
                format!("{e} — null-valued field(s): {}", null_keys.join(", "))
            })
        })?;

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

        // Mapped here rather than during deserialization, so an unrecognised entry
        // can say WHICH string it was and what the choices are. `deny_unknown_fields`
        // catches a misspelled FIELD; this catches a misspelled VALUE, and until now
        // the second produced a serde variant error naming every valid option except
        // the one the author actually typed.
        let mut caps = Vec::with_capacity(raw.kernel_capabilities.len());
        for c in &raw.kernel_capabilities {
            caps.push(Capability::parse(c).map_err(|e| DomainError::Manifest(e.to_string()))?);
        }

        // `spawn` and `impl_kind` must agree, in BOTH directions.
        //
        // Missing when out-of-process: the kernel would have a module it cannot
        // start, and would find that out at spawn time rather than at parse time
        // — after the package is installed and the operator has been told it
        // worked.
        //
        // Present when in-process: a contradiction, and the dangerous half is
        // that it is a SILENT one. A crate compiled into the daemon ignores the
        // field, so a manifest saying `impl_kind: in_process_crate` with a spawn
        // command describes a program that will never run, while reading as
        // though it does.
        match (raw.impl_kind, raw.spawn.as_ref()) {
            (ImplKind::OutOfProcessProvider, None) => {
                return Err(DomainError::Manifest(
                    "impl_kind is out_of_process but no `spawn` command is declared; \
                     the kernel would have no way to start this module"
                        .to_owned(),
                ));
            }
            (ImplKind::InProcessCrate, Some(_)) => {
                return Err(DomainError::Manifest(
                    "`spawn` is declared but impl_kind is in_process_crate; a compiled-in \
                     module is never started as a process, so this command would never run"
                        .to_owned(),
                ));
            }
            _ => {}
        }
        if let Some(spawn) = raw.spawn.as_ref() {
            spawn.validate().map_err(DomainError::Manifest)?;
        }

        Ok(Self {
            name: raw.name,
            version: raw.version,
            requires_models: raw.requires_models,
            requires_apis: raw.requires_apis,
            requires_deps: raw.requires_deps,
            kernel_capabilities: caps,
            ui_entry: raw.ui_entry,
            impl_kind: raw.impl_kind,
            spawn: raw.spawn,
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
    /// How to start this module, when it is an out-of-process one.
    ///
    /// `None` for an in-process crate — and that is not "not configured yet",
    /// it is a state the parser refuses to produce for an out-of-process module.
    /// A caller therefore never has to decide what a missing command means.
    #[must_use]
    pub fn spawn(&self) -> Option<&SpawnCommand> {
        self.spawn.as_ref()
    }

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

/// A boxed, borrowed future — hand-written rather than pulling in `futures`/
/// `futures-core` for one alias (T7b/ME-3e design doc, decision 6: either is
/// fine, this crate picks the no-new-dependency option).
pub type ApprovalFuture<'a, T> =
    std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// Where a module's `_a24/approval/gate`/`advise`/`status` calls go
/// (T7b/ME-3e design doc, decision 6) — the in-process analogue of
/// [`EventBroadcast`]. The kernel supplies the real implementation
/// (`PolicyApprovalBackend` in `agent24d`); this crate only holds the
/// contract, so it need not depend on `agent24-policy` or `agent24-store`.
pub trait ApprovalBackend: Send + Sync {
    /// Submit a `gate` (kernel-executed) or `advise` (module-domain) action.
    /// Returns immediately with `decision: Pending` (or an error) — this is
    /// the async submit-then-poll model (design doc "v4→v5"), never a
    /// blocking wait for a human decision.
    fn submit<'a>(
        &'a self,
        module: &'a str,
        kind: agent24_protocol::ModuleApprovalKind,
        action: String,
        target: Option<String>,
        payload: serde_json::Value,
    ) -> ApprovalFuture<
        'a,
        std::result::Result<
            agent24_protocol::ApprovalAnswer,
            agent24_protocol::ApprovalRequestError,
        >,
    >;

    /// Query the current decision for `approval_id`, scoped to `module` —
    /// idempotent, callable any number of times, at any time after submit.
    fn status<'a>(
        &'a self,
        module: &'a str,
        approval_id: &'a str,
    ) -> ApprovalFuture<
        'a,
        std::result::Result<
            agent24_protocol::ApprovalAnswer,
            agent24_protocol::ApprovalRequestError,
        >,
    >;
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

/// A module's ONLY way to submit/query approvals in-process — bound to a
/// VALIDATED manifest at construction, exactly like [`EventSink::new`]
/// (T7b/ME-3e design doc, decision 6: "照抄 `EventSink::new`", not an
/// `impl Into<String>` a caller could hand an arbitrary string to). The
/// requester's EXISTENCE is the capability: the kernel builds one only for a
/// module it granted [`Capability::Approval`].
pub struct ApprovalRequester {
    module: String,
    backend: Arc<dyn ApprovalBackend>,
}

impl ApprovalRequester {
    /// Build the requester for `manifest`'s module. The name is taken from
    /// the manifest, never from a caller-supplied string.
    #[must_use]
    pub fn new(manifest: &DomainOsManifest, backend: Arc<dyn ApprovalBackend>) -> Self {
        Self {
            module: manifest.name().to_owned(),
            backend,
        }
    }

    /// Submit a `gate`/`advise` action. Returns immediately — see
    /// [`ApprovalBackend::submit`]'s docs on the async submit-then-poll model.
    pub async fn submit(
        &self,
        kind: agent24_protocol::ModuleApprovalKind,
        action: impl Into<String>,
        target: Option<String>,
        payload: serde_json::Value,
    ) -> std::result::Result<agent24_protocol::ApprovalAnswer, agent24_protocol::ApprovalRequestError>
    {
        self.backend
            .submit(&self.module, kind, action.into(), target, payload)
            .await
    }

    /// Query the current decision for `approval_id`, scoped to this
    /// requester's module.
    pub async fn status(
        &self,
        approval_id: &str,
    ) -> std::result::Result<agent24_protocol::ApprovalAnswer, agent24_protocol::ApprovalRequestError>
    {
        self.backend.status(&self.module, approval_id).await
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
/// [`KernelCtx::events`] and [`KernelCtx::memory`] exist today; `models` /
/// `scheduler` / `policy` land as their consumers do. Memory arrived in F1 and
/// kept the rule that was written here while it was still future work: it
/// consults kernel-owned policy and takes NO caller-supplied [`Grants`] and no
/// caller-supplied scope, because both would be boundaries the caller could
/// move.
pub trait KernelCtx: Send + Sync {
    /// The module-scoped event sink, or `None` when [`Capability::Events`] was not
    /// granted. `None` is an expected outcome, not an error: a module that can run
    /// without events simply does. The REASON a capability was withheld belongs in
    /// the kernel's mount diagnostics, not in every handle lookup.
    fn events(&self) -> Option<&EventSink>;

    /// The module-scoped memory handle, or `None` when [`Capability::Memory`] was
    /// not granted.
    ///
    /// Deliberately takes NO scope argument. An earlier sketch of this was
    /// `memory(scope, grants)`, and both parameters were mistakes: a scope the
    /// caller supplies is a boundary the caller can move, and `Grants` is
    /// informational rather than authority (see the crate trust model). The kernel
    /// builds one of these per admitted module from that module's VALIDATED
    /// manifest, exactly as it builds the event sink.
    ///
    /// See [`memory`] for what the handle does and does not guarantee — in
    /// particular that the isolation is enforced by the kernel rather than by the
    /// schema, and that identifiers are database-global.
    fn memory(&self) -> Option<&dyn memory::ScopedMemory> {
        // Defaulted so existing implementors (and tests) do not have to opt in to
        // a capability they never had. "Not granted" is the honest default: a
        // context that has not been taught to lend memory does not lend it.
        None
    }

    /// The module-scoped approval requester, or `None` when
    /// [`Capability::Approval`] was not granted (T7b/ME-3e). Defaulted for
    /// the same reason as [`Self::memory`]: existing implementors need not
    /// opt in to a capability they never had.
    fn approval(&self) -> Option<&ApprovalRequester> {
        None
    }
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

    // ---- ME-3a gate 6: manifest schema / protocol versioning ----------------
    //
    // The property under test is NOT "a bad version is rejected" — the strict
    // parse would reject it too, eventually, for the wrong reason. It is that a
    // manifest from the FUTURE produces a message naming the VERSION rather than
    // naming whichever unknown field happened to come first. See §3 gate 6 of
    // SPEC-ME3-OUT-OF-PROCESS.md.

    #[test]
    fn manifest_without_a_version_is_v1_and_still_loads() {
        // Every manifest written before the field existed. Making it required
        // would break all of them at once, so absence MUST mean v1 — and it must
        // not warn either, or upgrading the daemon lights up every module.
        assert!(!SIN90_YAML.contains("manifest_version"));
        let m = DomainOsManifest::from_yaml(SIN90_YAML).unwrap();
        assert_eq!(m.name(), "sin90");
    }

    #[test]
    fn manifest_version_equal_to_ours_loads() {
        let yaml = format!("{SIN90_YAML}manifest_version: {MANIFEST_SCHEMA_VERSION}\n");
        assert!(DomainOsManifest::from_yaml(&yaml).is_ok());
    }

    #[test]
    fn an_unknown_capability_names_itself_and_the_alternatives() {
        // A misspelled capability used to produce serde's variant error, which
        // lists every valid option EXCEPT the one the author typed — so the reader
        // has to diff the list against their own file to find it. The error must
        // carry the rejected string.
        let yaml = SIN90_YAML.replace(
            "kernel_capabilities: [events]",
            "kernel_capabilities: [events, telepthy]",
        );
        let err = DomainOsManifest::from_yaml(&yaml).unwrap_err().to_string();
        assert!(
            err.contains("telepthy"),
            "the error must show the string that was rejected, or a typo is a \
             scavenger hunt: {err}"
        );
        assert!(err.contains("unknown_capability"), "{err}");
        // And the alternatives, or "it is not one of them" is unactionable.
        assert!(err.contains("events") && err.contains("memory"), "{err}");
    }

    #[test]
    fn the_capability_list_and_the_parser_cannot_drift() {
        // `ALL_CAPABILITIES` feeds both `parse` and the `supported` list in the
        // error. If a variant is added to the enum but not to the slice, it becomes
        // unparseable while still being a legal value elsewhere — a split that
        // produces "unknown_capability: memory" if it ever happened to `Memory`.
        // The list must be built from the ENUM, not read off the slice. Iterating
        // `ALL_CAPABILITIES` only catches drift one way (a name in the slice that
        // `parse` rejects); the direction this test's own comment names — a variant
        // ADDED to the enum but not to the slice — is invisible to it, because the
        // loop never sees that variant. Measured: adding a variant to the enum and
        // to `as_str`, leaving the slice alone, kept every test green.
        //
        // The exhaustive `match` is the mechanism. `non_exhaustive` does not apply
        // inside the defining crate, so a new variant makes this fail to COMPILE
        // until it is listed here — and listing it here is what puts it in front of
        // the assertion below.
        let every_variant: Vec<Capability> = [
            Capability::Events,
            Capability::Models,
            Capability::Scheduler,
            Capability::Policy,
            Capability::Memory,
            Capability::Approval,
        ]
        .into_iter()
        .inspect(|c| {
            // Forces the compiler to check this list is complete: adding a variant
            // without adding it above breaks this match.
            match c {
                Capability::Events
                | Capability::Models
                | Capability::Scheduler
                | Capability::Policy
                | Capability::Memory
                | Capability::Approval => {}
            }
        })
        .collect();

        for c in &every_variant {
            assert!(
                ALL_CAPABILITIES.contains(c),
                "{} is a variant but missing from ALL_CAPABILITIES — `parse` would \
                 reject a legal capability while `as_str` still produces it",
                c.as_str()
            );
            assert_eq!(
                Capability::parse(c.as_str()).unwrap(),
                *c,
                "{} round-trips through its own string",
                c.as_str()
            );
        }
        // Control: the parser is not simply accepting everything.
        assert!(Capability::parse("definitely-not-a-capability").is_err());
    }

    #[test]
    fn schema_version_zero_is_refused() {
        // Versions start at 1, so `0` is not "older than v1" — there is no v1-minus.
        // Accepting it would treat a document whose author meant something else as
        // if it were unversioned.
        let yaml = format!("{SIN90_YAML}manifest_version: 0\n");
        let err = DomainOsManifest::from_yaml(&yaml).unwrap_err();
        assert!(
            matches!(err, DomainError::ManifestUnsupported { .. }),
            "v0 is a version mismatch, not a malformed document: {err:?}"
        );
        // Control: v1 and absent both still load, so this did not just break the
        // compatibility rule it sits beside.
        assert!(DomainOsManifest::from_yaml(SIN90_YAML).is_ok());
        assert!(DomainOsManifest::from_yaml(&format!("{SIN90_YAML}manifest_version: 1\n")).is_ok());
    }

    #[test]
    fn manifest_from_the_future_names_the_version_not_a_stray_field() {
        // The future manifest also carries a field this build has never heard of.
        // That is the whole point: the STRICT shape would fail on `warp_drive`
        // and say so, never mentioning that the daemon is simply too old.
        let yaml = format!(
            "{SIN90_YAML}manifest_version: {}\nwarp_drive: true\n",
            MANIFEST_SCHEMA_VERSION + 1
        );
        let err = DomainOsManifest::from_yaml(&yaml).unwrap_err();
        match &err {
            DomainError::ManifestUnsupported {
                module,
                requirement,
                supported,
            } => {
                assert_eq!(module, "sin90", "the operator needs to know WHICH module");
                assert!(
                    requirement.contains(&(MANIFEST_SCHEMA_VERSION + 1).to_string()),
                    "requirement must name the version it wanted: {requirement}"
                );
                // Assert `supported` too, and assert its SHAPE — a range, not a
                // bare number. Ignoring this field with `..` is exactly what let a
                // redundant second gate answer this case: the two paths produced
                // different `supported` strings ("v1" vs "v1..=v1") and no test
                // looked at the field where they differed.
                assert_eq!(
                    supported,
                    &format!("v1..=v{MANIFEST_SCHEMA_VERSION}"),
                    "the supported RANGE must be shown, and it must come from the \
                     version dispatch — a bare number means something else answered"
                );
            }
            other => panic!("expected ManifestUnsupported, got {other:?}"),
        }
        let msg = err.to_string();
        assert!(
            !msg.contains("warp_drive"),
            "the message must not blame the unknown field — that is the failure \
             mode gate 6 exists to prevent: {msg}"
        );
    }

    #[test]
    fn no_field_of_a_future_document_decides_the_outcome() {
        // The line is not "did it READ anything", it is "did anything it read
        // DECIDE anything". An earlier name for this test said "nothing reads a
        // future document's fields", which was stronger than the code: `module` is
        // read from that document's `name` and appears in the very error below
        // (`module: "sin90"`).
        //
        // That read is deliberate and harmless because `name` decides nothing — it
        // never participates in a judgement, and when it is missing or the wrong
        // type it degrades to `<unnamed>`. The worst it can produce is a MISLEADING
        // LABEL. The protocol gate was different in kind: it produced an ASSERTION
        // ABOUT THE DOCUMENT ("your field is the wrong type") from a document whose
        // schema this build had just declared it cannot read.
        //
        // So: a build announcing "I cannot read v7" must not let anything it finds
        // in that v7 document determine what happens. Doing so assumes v7 still
        // spells the field this way and still types it this way — the exact
        // assumption §3 says `<= current` cannot survive.
        //
        // The visible symptom was narrower and easier to dismiss: a future manifest
        // with a malformed `min_daemon_protocol` was told its FIELD was the wrong
        // type, i.e. that the document was malformed — contradicting
        // `ManifestUnsupported`'s own documentation.
        let future = format!(
            "{SIN90_YAML}manifest_version: {}\n",
            MANIFEST_SCHEMA_VERSION + 1
        );
        for tail in ["min_daemon_protocol: 99\n", "min_daemon_protocol: nope\n"] {
            let err = DomainOsManifest::from_yaml(&format!("{future}{tail}")).unwrap_err();
            match &err {
                DomainError::ManifestUnsupported { requirement, .. } => assert!(
                    requirement.contains("manifest schema"),
                    "the VERSION must be what is refused, not something read out of \
                     a document whose version we just rejected: {requirement}"
                ),
                other => panic!("expected a version refusal, got {other:?}"),
            }
        }

        // Control: at a version we DO support, the protocol gate must still fire.
        // Without this, "the version answers first" is equally satisfied by having
        // deleted the protocol gate altogether.
        let err = DomainOsManifest::from_yaml(&format!(
            "{SIN90_YAML}min_daemon_protocol: {}\n",
            DAEMON_PROTOCOL_VERSION + 1
        ))
        .unwrap_err();
        assert!(
            err.to_string().contains("kernel protocol"),
            "the protocol gate must still work at a supported version: {err}"
        );
    }

    #[test]
    fn min_daemon_protocol_above_ours_is_refused_with_both_numbers() {
        let yaml = format!(
            "{SIN90_YAML}min_daemon_protocol: {}\n",
            DAEMON_PROTOCOL_VERSION + 1
        );
        let err = DomainOsManifest::from_yaml(&yaml).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains(&(DAEMON_PROTOCOL_VERSION + 1).to_string())
                && msg.contains(&DAEMON_PROTOCOL_VERSION.to_string()),
            "both sides' versions must appear, or the operator cannot tell who is \
             behind: {msg}"
        );
    }

    #[test]
    fn min_daemon_protocol_at_or_below_ours_loads() {
        let yaml = format!("{SIN90_YAML}min_daemon_protocol: {DAEMON_PROTOCOL_VERSION}\n");
        assert!(DomainOsManifest::from_yaml(&yaml).is_ok());
    }

    #[test]
    fn an_unnamed_future_manifest_still_reports_a_version_error() {
        // A manifest too broken to yield a name must still reach the version
        // gate. Refusing to run the gate without a name would put us back where
        // we started: an unreadable serde error.
        let yaml = format!("manifest_version: {}\n", MANIFEST_SCHEMA_VERSION + 1);
        match DomainOsManifest::from_yaml(&yaml).unwrap_err() {
            DomainError::ManifestUnsupported { module, .. } => assert_eq!(module, "<unnamed>"),
            other => panic!("expected ManifestUnsupported, got {other:?}"),
        }
    }

    #[test]
    fn the_strict_shape_still_rejects_unknown_fields_at_a_supported_version() {
        // The tolerant envelope must not have loosened step two. Same stray field
        // as the future-manifest test, but at a version we DO support: now it is
        // a genuine typo and must fail, naming the field.
        let yaml = format!("{SIN90_YAML}warp_drive: true\n");
        let err = DomainOsManifest::from_yaml(&yaml).unwrap_err();
        assert!(
            matches!(err, DomainError::Manifest(_)),
            "a stray field at a supported version is a malformed manifest, not a \
             version mismatch: {err:?}"
        );
        assert!(err.to_string().contains("warp_drive"), "{err}");
    }

    #[test]
    fn a_bom_does_not_turn_into_an_unreadable_reason() {
        // Windows editors write a UTF-8 BOM by default, so this is the likeliest
        // way a hand-written manifest fails. Untreated, serde_yaml calls it
        // "containing more than one document is not supported" — a sentence about
        // something that is not the problem. This gate's whole purpose is readable
        // reasons; the commonest authoring accident must not produce the least
        // readable message.
        // The BOM must be ADJACENT to content. `SIN90_YAML` opens with a newline,
        // so the obvious `format!("\u{feff}{SIN90_YAML}")` produces `<BOM>\n…`,
        // which serde_yaml accepts — a test written that way passes with or
        // without the fix. (It was written that way; a mutation run caught it.)
        let with_bom = format!("\u{feff}{}", SIN90_YAML.trim_start_matches('\n'));
        // Assert the SHAPE of the failure, not merely that one occurred. `is_err()`
        // is satisfied by ANY error — a later edit that breaks this fixture's
        // indentation would keep the precondition green while it silently began
        // proving something else. That is the same disease as "the positive
        // control is non-zero, so the instrument works".
        let why = serde_yaml::from_str::<serde_yaml::Value>(&with_bom)
            .expect_err("fixture must reproduce the BOM failure")
            .to_string();
        assert!(
            why.contains("more than one document"),
            "the fixture must reproduce THE MISLEADING MESSAGE this strip exists to \
             prevent, not just some error. If upstream ever replaces it with a \
             readable one, this fails first — and then the thing to delete is the \
             workaround, not this test. Got: {why}"
        );
        let m = DomainOsManifest::from_yaml(&with_bom).unwrap();
        assert_eq!(m.name(), "sin90");
        // CRLF was NOT the trigger — pinned so a future "fix" does not go after
        // the wrong character.
        assert!(DomainOsManifest::from_yaml(&SIN90_YAML.replace('\n', "\r\n")).is_ok());
    }

    #[test]
    fn a_version_that_is_not_an_integer_is_refused_by_name() {
        // ABSENT and MALFORMED must not collapse. `Value::as_u64` returns None for
        // a string, so without this check `manifest_version: "3"` reads as absent
        // → defaults to 1 → PASSES the gate → dies later in the strict shape on a
        // type error. That is precisely the unreadable outcome the gate removes,
        // reached by a different road.
        for bad in ["\"3\"", "!!str 3", "three", "-1"] {
            let yaml = format!("{SIN90_YAML}manifest_version: {bad}\n");
            let err = DomainOsManifest::from_yaml(&yaml).unwrap_err();
            assert!(
                err.to_string().contains("manifest_version"),
                "the error must name the field, not the type mismatch it caused \
                 downstream ({bad}): {err}"
            );
        }
    }

    #[test]
    fn a_yaml_null_is_not_accepted_as_the_literal_string_tilde() {
        // Locks the reason `from_value` was chosen over `from_str` for the strict
        // shape. Measured on serde_yaml 0.9.34:
        //
        //   from_str  : name: ~  →  Ok(name == "~")   ← a STRING whose content is "~"
        //   from_value: name: ~  →  Err(invalid type: unit value, expected a string)
        //
        // The `from_str` outcome does not error, has the right type, and carries a
        // wrong value — an error answer sitting inside the distribution of legal
        // answers. If anyone ever switches this back to `from_str` (say, to regain
        // line/column in errors), a module could be named "~". This test is what
        // stops that from landing silently.
        // Use `version`, NOT `name`. Under `from_str` a null `name` becomes the
        // string "~" and is then caught by name validation — the same error
        // VARIANT either way, so a test asserting the variant passes under both
        // deserializers and proves nothing. (It was written that way; mutation 6
        // — switching the strict parse back to `from_str` — killed nothing, which
        // is how it was found.) `version` has no such second line of defence: "~"
        // is non-empty, so it sails through and the manifest loads with a version
        // of "~".
        let yaml = SIN90_YAML.replace(r#"version: "0.2.1""#, "version: ~");
        let err = DomainOsManifest::from_yaml(&yaml).unwrap_err();
        assert!(
            err.to_string().contains("invalid type"),
            "a YAML null must be refused ON TYPE, never coerced to the string \
             \"~\" and waved through: {err}"
        );
    }

    #[test]
    fn a_version_key_with_no_value_is_not_silently_v1() {
        // PRESENT-but-empty is not ABSENT. This was the last place where the
        // gate's own principle — keep those two apart — was still collapsing.
        // Nobody writes `manifest_version:` to mean v1: omitting the key already
        // means that and is shorter. So an empty value is a slip or an unfilled
        // template slot, and both want to be told now rather than to be read as 1.
        for key in ["manifest_version", "min_daemon_protocol"] {
            let yaml = format!("{SIN90_YAML}{key}:\n");
            let err = DomainOsManifest::from_yaml(&yaml).unwrap_err();
            assert!(
                err.to_string().contains(key),
                "an empty {key} must be refused BY NAME, not read as absent: {err}"
            );
        }
        // Control: the absent case must still load, or this check has simply
        // broken the compatibility rule it sits next to.
        assert!(DomainOsManifest::from_yaml(SIN90_YAML).is_ok());
    }

    #[test]
    fn a_located_message_also_carries_the_null_fields() {
        // Both facts are true and they do not compete. The borrowed message
        // locates the FIRST thing serde tripped on; a null field further up may be
        // the reason the author is here at all. Reporting only the borrowed one
        // sends them round a second lap — fix the stray field, run again, and only
        // then meet the real culprit.
        let yaml = format!(
            "{}\nwarp_drive: true\n",
            SIN90_YAML.replace(r#"version: "0.2.1""#, "version: ~")
        );
        let msg = DomainOsManifest::from_yaml(&yaml).unwrap_err().to_string();
        assert!(
            msg.contains("warp_drive"),
            "the located message must survive: {msg}"
        );
        // Assert the MARKER, not the word "version". serde's own message lists the
        // expected field names — `version` among them — so `contains("version")`
        // is satisfied whether or not the null list was appended. (It was written
        // that way; mutation 10 killed nothing, which is how it was found. Fourth
        // time in this change: the assertion landed on a set wider than the
        // property it claimed to test.)
        assert!(
            msg.contains("null-valued field(s): version"),
            "the null field must ride along, or the author needs two laps: {msg}"
        );
    }

    #[test]
    fn a_nameless_type_error_still_names_the_field() {
        // The case the borrowed message cannot help with, and the one that needs
        // help most: `from_str` is the LOOSER deserializer, so it only has a
        // message to lend when it ALSO refuses — never in the situations where
        // `from_value` is the stricter one. `version: ~` alone yields
        // "invalid type: unit value, expected a string": no line, no field, in a
        // document with five string fields.
        let yaml = SIN90_YAML.replace(r#"version: "0.2.1""#, "version: ~");
        let msg = DomainOsManifest::from_yaml(&yaml).unwrap_err().to_string();
        assert!(msg.contains("invalid type"), "{msg}");
        assert!(
            msg.contains("version"),
            "a type error with no field name is a scavenger hunt across every \
             string field; the null-valued key must be named: {msg}"
        );
    }

    #[test]
    fn a_shape_error_keeps_its_line_and_column() {
        // `from_value` errors carry no position — the tree it walks has none. The
        // error path re-reads the text with `from_str` PURELY to borrow a located
        // message. Without that, this gate would be strictly better at judging and
        // strictly worse at explaining, which is a poor trade for something whose
        // stated purpose is readable reasons.
        let yaml = format!("{SIN90_YAML}warp_drive: true\n");
        let msg = DomainOsManifest::from_yaml(&yaml).unwrap_err().to_string();
        assert!(msg.contains("warp_drive"), "{msg}");
        assert!(
            msg.contains("line") && msg.contains("column"),
            "the message must locate the offending field, or a long manifest is a \
             scavenger hunt: {msg}"
        );
    }

    #[test]
    fn the_document_is_parsed_once_not_twice() {
        // The property: BOTH steps read one tree, so they cannot disagree, and a
        // hostile document is not paid for twice. There is no clean way to count
        // parses from outside, so this asserts the observable consequence — the
        // two shapes agree about a document that is legal for one reading and not
        // the other. `serde_yaml` refuses duplicate keys outright (measured), so a
        // document cannot present one `name` to the gate and another to the strict
        // shape; this test pins that we depend on that refusal.
        let dup = format!("{SIN90_YAML}name: impostor\n");
        let err = DomainOsManifest::from_yaml(&dup).unwrap_err();
        assert!(
            err.to_string().contains("duplicate"),
            "a second `name` must be refused by the parser, not silently resolved \
             to one of the two — the version gate and the strict shape would then \
             be reading different documents: {err}"
        );
    }

    #[test]
    fn the_version_gate_runs_before_the_size_check_does_not_regress() {
        // Order matters the other way too: an oversized document must still be
        // refused for its SIZE, not parsed by the tolerant envelope first.
        let yaml = format!(
            "{}\n{}",
            SIN90_YAML,
            "#".repeat(DomainOsManifest::MAX_YAML_BYTES)
        );
        assert!(matches!(
            DomainOsManifest::from_yaml(&yaml).unwrap_err(),
            DomainError::ManifestTooLarge(_)
        ));
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
        // The fixture gained a `spawn` block when ME-3b-3 made one mandatory for
        // out-of-process modules. Without it this manifest is now refused at
        // PARSE time — which is the point of that rule — so the old fixture was
        // describing a manifest that can no longer exist.
        let yaml = SIN90_YAML.replace(
            "impl_kind: in_process_crate",
            "impl_kind: out_of_process_provider\nspawn:\n  command: bin/sin90\n",
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

#[cfg(test)]
mod spawn_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn manifest(impl_kind: &str, spawn: &str) -> Result<DomainOsManifest> {
        DomainOsManifest::from_yaml(&format!(
            "name: cos72\n\
             version: \"0.1.0\"\n\
             route_namespace: /api/v1/cos72\n\
             event_module: cos72\n\
             data_dir: ~/.agent24/os/cos72/\n\
             impl_kind: {impl_kind}\n{spawn}"
        ))
    }

    /// The field exists at all — which it did not until ME-3b-3, even though
    /// SPEC's ME-3a row said the manifest supports a spawn command. The half was
    /// simply never implemented, and nothing noticed because no code had reached
    /// the point of needing to start anything.
    #[test]
    fn an_out_of_process_module_declares_how_to_start_it() {
        let m = manifest(
            "out_of_process_provider",
            "spawn:\n  command: node\n  args: [\"server.js\"]\n",
        )
        .expect("a well-formed out-of-process manifest");
        let spawn = m.spawn().expect("out-of-process implies a spawn command");
        assert_eq!(spawn.command, "node");
        assert_eq!(spawn.args, ["server.js"]);
    }

    /// The point of `command` + `args`: a module written in any language is
    /// declared directly, rather than behind a wrapper script the kernel would
    /// read without anyone reviewing it.
    #[test]
    fn a_module_in_any_language_can_be_declared_without_a_wrapper() {
        for (command, args) in [
            ("node", "[\"server.js\"]"),
            ("python3", "[\"-u\", \"main.py\"]"),
            ("bin/my-module", "[]"),
        ] {
            let m = manifest(
                "out_of_process_provider",
                &format!("spawn:\n  command: {command}\n  args: {args}\n"),
            )
            .unwrap_or_else(|e| panic!("{command} was refused: {e}"));
            assert_eq!(m.spawn().unwrap().command, command);
        }
    }

    /// Both directions of the agreement between `impl_kind` and `spawn`.
    ///
    /// The in-process half is the one worth having: a compiled-in crate ignores
    /// the field, so without this check the manifest would describe a program
    /// that never runs while reading as though it does — a silent contradiction
    /// rather than a loud one.
    #[test]
    fn impl_kind_and_spawn_must_agree_in_both_directions() {
        let missing = manifest("out_of_process_provider", "").expect_err("no spawn command");
        assert!(missing.to_string().contains("spawn"), "{missing}");

        let contradiction = manifest("in_process_crate", "spawn:\n  command: node\n")
            .expect_err("in-process module with a spawn command");
        assert!(
            contradiction.to_string().contains("spawn"),
            "{contradiction}"
        );

        // Controls: each kind on its own is fine, so the two refusals above are
        // about the COMBINATION and not about either half.
        assert!(manifest("in_process_crate", "").is_ok());
        assert!(manifest("out_of_process_provider", "spawn:\n  command: node\n").is_ok());
    }

    /// A package that names something outside itself cannot be reviewed by
    /// reading it.
    ///
    /// Note what this is NOT: it is not a security boundary. Whoever can write
    /// the manifest can already name `node` and put anything on `PATH`. The
    /// boundary is who may write the packages root (FU-41). This rule is about
    /// what a package can DESCRIBE — and saying otherwise would be worse than
    /// having no rule, because the claim would carry a name saying it was checked.
    #[test]
    fn a_spawn_command_may_not_point_outside_the_package() {
        for bad in ["/bin/sh", "../../elsewhere/bin", "bin/../../escape", ""] {
            let m = manifest(
                "out_of_process_provider",
                &format!("spawn:\n  command: \"{bad}\"\n"),
            );
            assert!(m.is_err(), "{bad:?} was accepted");
        }
        // Controls: the two legal shapes must still pass, or "refuse everything"
        // would satisfy the loop above.
        assert!(manifest("out_of_process_provider", "spawn:\n  command: node\n").is_ok());
        assert!(
            manifest(
                "out_of_process_provider",
                "spawn:\n  command: bin/my-module\n"
            )
            .is_ok(),
            "a path inside the package must be allowed"
        );
    }

    /// Arguments are passed verbatim to an exec, never to a shell — so the
    /// characters that would be dangerous in a shell are just characters here.
    /// Asserted rather than assumed, because "we don't use a shell" is the kind
    /// of claim that quietly stops being true.
    #[test]
    fn arguments_are_not_shell_interpreted() {
        let m = manifest(
            "out_of_process_provider",
            "spawn:\n  command: node\n  args: [\"a b; rm -rf /\", \"$HOME\", \"*\"]\n",
        )
        .expect("odd-looking arguments are still just arguments");
        assert_eq!(m.spawn().unwrap().args, ["a b; rm -rf /", "$HOME", "*"]);
    }
}
