//! `ScopedMemory` — the capability-limited memory handle a domain OS gets (F1).
//!
//! ADR-029 left this as an open hole, and the hole had a consequence worth
//! stating plainly: with no handle to give, two domain OSes mounted under one
//! owner **shared the memory base**. Their business databases were physically
//! separate from ME-1b-b onward; the shared M-D base was not.
//!
//! # What this is, and what it is not
//!
//! It is a **narrow capability API**, not a scoped-looking wrapper around the
//! real stores. That distinction is the whole design, because the failure it
//! avoids is specific: `agent24_memory::KvStore` is the ROOT handle — possessing
//! one yields accessors for events, artifacts, assertions, retrievers,
//! consolidation, knowledge, trace and vectors. A "scoped" wrapper that handed
//! back a `KvStore`, a pool, or any store derived from one would isolate
//! nothing.
//!
//! So the rules below are structural, not conventions:
//!
//! 1. **No method takes an owner.** The kernel injects it, exactly as
//!    [`EventSink`](crate::EventSink) stamps the module name rather than
//!    accepting one. A parameter a module can fill is a boundary a module can
//!    cross.
//! 2. **No underlying handle escapes** — no `KvStore`, no pool, no `EventLog`,
//!    no `AssertionLedger`, no raw connection.
//! 3. **No global maintenance.** Rebuilding a retrieval projection is a
//!    kernel operation over EVERY owner; a module holding it could wipe and
//!    rebuild the projections belonging to other modules and to the user's own
//!    memory. That is not a read leak, and it is still not a module's business.
//!
//! # What it does NOT give you — stated because the alternative is a lie
//!
//! The last several review rounds on this codebase were almost entirely
//! contracts asserting more than they delivered, so:
//!
//! - **The isolation is enforced by the KERNEL, not by the schema.** The storage
//!   layer enforces an opaque owner key; this crate's wrapper is what gives that
//!   key its module meaning. Do not describe it as schema-enforced module
//!   isolation.
//! - **Identifiers are DATABASE-GLOBAL.** `mem_events.id` is globally unique and
//!   `mem_assertions.id` is a bare primary key, so two isolated modules that
//!   both mint `msg-1` collide. They cannot read one another — but one can stop
//!   the other from writing. [`ScopedMemory`] therefore does not accept
//!   caller-minted ids at all (see [`Remembered`]); the kernel mints them.
//! - **`agent` / `session` / `run` are NOT isolation boundaries.** They exist in
//!   the serialized `Scope` and are enforced nowhere. Only `owner` is.
//! - **This is not a sandbox.** An in-process module is trusted code, as the
//!   crate-level trust model says. What this bounds is what the kernel HANDS
//!   OVER, not what a determined module could reach by other means.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Something a module asked the memory base to remember.
///
/// It carries **no id and no scope**: the kernel mints the identifier and
/// supplies the scope, so a module cannot choose an id that collides with
/// another module's (identifiers are database-global — see the module docs) and
/// cannot smuggle an owner in through a field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Remember {
    /// A short, module-defined category — its own `kind` space, not the
    /// kernel's.
    pub kind: String,
    /// The body. An object, matching the event envelope.
    pub body: serde_json::Map<String, serde_json::Value>,
}

impl Remember {
    pub fn new(kind: impl Into<String>, body: serde_json::Map<String, serde_json::Value>) -> Self {
        Self {
            kind: kind.into(),
            body,
        }
    }
}

/// What the kernel stored, as the module may see it.
///
/// `id` is the KERNEL's identifier. It is returned so a module can correlate its
/// own later reads, and it is deliberately opaque: a module must not parse it,
/// derive another from it, or assume anything about its shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Remembered {
    pub id: MemoryId,
    pub at: String,
}

/// A kernel-minted identifier, which a module should treat as opaque.
///
/// The accurate claim, since a looser one was written here first and review was
/// right to reject it: **a caller cannot choose the id supplied to
/// [`ScopedMemory::remember`]** — there is no id field on [`Remember`], so the
/// kernel's is the only one that reaches storage. That is the property isolation
/// depends on, because `mem_events.id` is database-global.
///
/// What this type does NOT do is prevent construction or inspection.
/// [`Self::from_kernel`] is `pub` and this crate is the modules' own dependency;
/// `Deserialize` is a second construction path; [`Self::as_str`] and `Display`
/// expose the string. "Opaque" is therefore a CONVENTION about depending on its
/// shape — one that costs nothing to keep and would break the moment the kernel
/// changed how it mints ids (the derived-key format is versioned precisely so it
/// can). It is not authority, and nothing downstream may treat a `MemoryId` as
/// proof of anything. That matters most at the ME-3 boundary: an out-of-process
/// module will send these over a wire, where a forged one must be as harmless as
/// it is today — which it is, because no method on this trait accepts one.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MemoryId(String);

impl MemoryId {
    /// Wrap a kernel-minted string.
    ///
    /// Named for the caller it is FOR, not for a restriction it enforces — see the
    /// type docs. Nothing stops a module from calling it; nothing is gained by
    /// doing so.
    pub fn from_kernel(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for MemoryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One result from a recall.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Recollection {
    pub id: MemoryId,
    pub kind: String,
    pub body: serde_json::Map<String, serde_json::Value>,
    pub at: String,
}

/// A domain OS's view of the shared memory base.
///
/// # No method here may accept a [`MemoryId`]
///
/// A rule for whoever extends this trait, stated here because it cannot be
/// enforced by a test — one was written (`a_memory_id_is_opaque`, then
/// `no_scoped_memory_method_accepts_a_memory_id`) and review was right that
/// neither could fail. `MemoryId` is constructible by any module (see its docs),
/// so it is not authority; it is harmless today only because there is nowhere to
/// spend one. A `fn forget(&self, id: MemoryId)` or `fn get(&self, id: MemoryId)`
/// would turn a forgeable value into a lookup key across partitions. If you need
/// one, resolve it against the caller's OWN partition — never on its own.
///
/// Every method is scoped to the module that was handed this object. There is no
/// parameter through which that scope can be widened, and no accessor through
/// which the underlying store can be reached — see the module docs for why both
/// are structural rather than advisory.
#[async_trait::async_trait]
pub trait ScopedMemory: Send + Sync {
    /// Remember something. The kernel mints the id and supplies the scope.
    async fn remember(&self, what: Remember) -> crate::Result<Remembered>;

    /// Recall by free text, newest and most relevant first.
    ///
    /// `limit` is honoured up to an implementation cap — a module asking for more
    /// than the kernel is willing to materialise gets the cap, not everything and
    /// not a silent page-sized slice. What it will never return is another
    /// module's memories, which is the guarantee that matters here.
    async fn recall(&self, query: &str, limit: usize) -> crate::Result<Vec<Recollection>>;

    /// This module's most recent memories, newest first — its own partition only.
    ///
    /// Same cap as [`Self::recall`]. Deliberately NOT "everything": a handle that
    /// let a caller ask for an unbounded set would make the kernel hold a whole
    /// partition in memory to answer one call.
    async fn recent(&self, limit: usize) -> crate::Result<Vec<Recollection>>;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn a_module_cannot_name_what_it_remembers() {
        // Identifiers are database-global (`mem_events.id` is globally UNIQUE),
        // so a caller-minted id lets one module DENY another module's write by
        // taking the name first. `Remember` therefore has no id field at all —
        // this is a compile-time property, and the test exists to say so where
        // someone would otherwise add one.
        let r = Remember::new("note", serde_json::Map::new());
        let json = serde_json::to_value(&r).unwrap();
        // As a SET: `serde_json::Map` is a BTreeMap, so the wire order is
        // alphabetical, not declaration order. What matters is which fields exist.
        let mut keys: Vec<&str> = json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec!["body", "kind"],
            "Remember must carry ONLY kind and body — no id, and no scope through \
             which an owner could be smuggled"
        );
    }
}
