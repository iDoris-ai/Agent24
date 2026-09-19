//! The kernel side of [`ScopedMemory`] — a module's view of the shared memory
//! base, keyed so that two domain OSes cannot reach each other.
//!
//! # What owns a memory (F8)
//!
//! A **space** does, inside an **org**. Not a user.
//!
//! F1 shipped the dimension as `(user, module)`, which is the shape of a
//! single-user product: it makes the owner of a memory the person who happened
//! to be logged in. The moment there are two people, the real owner is a
//! container they both relate to — Team Shared, Finance Private, Customer A —
//! and a user is an ACCESSOR of one. F8 separates those before there is data to
//! migrate; a personal deployment is then an org of one rather than a different
//! architecture.
//!
//! Isolation is UNCHANGED by that renaming: each module still gets its own
//! private space ([`SpaceId::module_private`]), so there is still exactly one
//! partition per module and still no way for one to read another's.
//!
//! # The key
//!
//! ```text
//!   v2\0<len(org)>\0<org>\0<len(space)>\0<space>
//! ```
//!
//! Three properties, each of which is load-bearing:
//!
//! - **LENGTH-PREFIXED**, so two different `(org, space)` pairs cannot produce
//!   one key. F1's first attempt was merely NUL-separated and its own test found
//!   the collision: `("a", "b\0os:c")` and `("a\0os:b", "c")` both rendered as
//!   `v1\0a\0os:b\0os:c`. Neither input is reachable today, but this repo has
//!   already paid once for a concat identity two pairs could produce (MD-5's
//!   `consol-{owner}-{key}`, review #122 B1), and "unreachable" is an argument
//!   where a length prefix is a property. Widening the dimension did not get to
//!   drop it.
//! - **Version-prefixed.** F1's review was blunt about why: baking a name into
//!   storage identity creates semantic migration debt, and after a module is
//!   renamed the database alone cannot say whether `…os:calendar` should become
//!   `calendar`, become `schedule`, merge, or stay separate as an uninstalled
//!   historical module. The version does not remove that debt —
//!   [`OsMemoryCatalog`] is what makes it payable — but it stops a migration from
//!   guessing which encoding it is reading. It is also what let F8 happen at all:
//!   see [`OsMemoryCatalog::migrate_legacy_partitions`], the first time that
//!   mechanism was used rather than merely described.
//! - **Disjoint from the user's own key.** The agent loop's memory is keyed by
//!   the bare user id, and every partition key begins with `v2\0`, so a module
//!   cannot reach the user's own memory and the user's memory is not polluted by
//!   modules. Precisely: this holds for any user id that does not itself begin
//!   with `v2\0`, and nothing validates that it does not — the daemon's only user
//!   id is the constant `LOCAL_USER`. A future multi-user id scheme has to keep
//!   that true, and this is the line that says so.
//!
//! # What F8 deliberately did NOT do
//!
//! - **The user's own memory is still keyed by the bare user id**, not by a
//!   space. It is the one partition with real data in the wild, and moving it is
//!   a migration with something to lose; F8 moved the partitions that were one
//!   day old. So "everything is space-owned" is NOT true yet, and the agent
//!   loop's memory is the exception.
//! - **There is no `mem_spaces` registry**, because nothing could read one. No
//!   path creates a space that is not a module's own, since nothing can grant
//!   access to one — a space that cannot be granted does not exist yet.
//! - **There are no roles, policies or permissions.** The org has members and
//!   nothing else. Whether an accessor MAY reach a space is not asked anywhere;
//!   isolation is still "your key or nothing", which is a partition, not a
//!   decision. Do not describe this file as access control.
//! - **There is no membership WORKFLOW.** This is the limitation most easily
//!   overstated, so it is stated flatly: what F8 delivers is the ownership
//!   DIMENSION, not a feature for adding people to orgs. The daemon creates
//!   exactly one org, for its one user, and never calls
//!   `KvStore::add_org_member` — which itself refuses any user who already has
//!   an org, i.e. anyone who has ever started the daemon. So no supported path
//!   puts a second member into an org today, and every claim here about a second
//!   member is a claim about what the STORAGE MODEL admits, not about behaviour a
//!   user can reach.
//!
//!   That is the intended scope rather than an unfinished corner. F8's whole
//!   argument is that the ownership dimension has to be right BEFORE there is
//!   data to migrate, because that is the part a later change cannot do cheaply;
//!   a membership workflow can be built any time, against whatever the real
//!   requirements turn out to be, and building one now would be inventing them.
//!   What had to happen while the catalog was one day old has happened.
//!
//! # What this does NOT do
//!
//! The isolation is enforced by the KERNEL, not by the schema: `agent24-memory`
//! enforces an opaque owner key, and this file is what gives that key its module
//! meaning. Two consequences worth stating rather than discovering:
//!
//! - **"Everything for this user" is no longer one `WHERE`.** It is this key
//!   plus every derived key the catalog knows. For a single-user local daemon
//!   that is also "the memory.db file", which is why the trade was acceptable —
//!   but any future export/erase path must go through [`OsMemoryCatalog`] rather
//!   than prefix-matching strings that contain NUL.
//! - **Identifiers stay database-global.** `mem_events.id` is globally UNIQUE, so
//!   two modules that minted the same id would collide even though neither can
//!   read the other. That is why [`ScopedMemory`] does not accept caller-minted
//!   ids at all — the kernel mints `osmem:<ULID>`.
//!
//!   Between modules that makes a collision IMPROBABLE, not impossible: an
//!   earlier version prefixed the partition key to make it unrepresentable, and
//!   that leaked the user id (round 4). What makes the weaker property safe is
//!   `EventLog::append` REFUSING an existing id under a different owner instead
//!   of aliasing into it — so do not weaken that conflict check on the grounds
//!   that ids cannot collide here. They can; the store just says no.

use std::sync::Arc;

use agent24_domain::memory::{MemoryId, Recollection, Remember, Remembered, ScopedMemory};
use agent24_domain::{Capability, DomainError, DomainOsManifest};
use agent24_memory::event::{EventQuery, EventStore, MemEvent, Origin, Scope, Trust};
use agent24_os_proto::drain::{RequestLifecycle, bind_to_lifecycle};
use agent24_os_proto::rpc::ErrorKind;
use tokio::sync::Semaphore;

use crate::events_emit::RateLimiter;
use crate::os_memory_page::{
    MEMORY_COST_RECALL, MEMORY_COST_REMEMBER, MEMORY_MAX_PAGE_SIZE,
    MEMORY_PAGE_RESPONSE_BUDGET_BYTES, MEMORY_SCAN_ROW_BUDGET, METHOD_TAG_RECALL,
    METHOD_TAG_RECENT, MemoryRpcError, Needle, PageMode, RecallPage, Reservation, decode_cursor,
    memory_cost_recent, page_from_stream,
};

/// The most rows either read method will ever return.
///
/// A cap that the CONTRACT states, rather than a clamp a caller discovers: a
/// module asking for 1000 gets 1000 if they exist. What it cannot do is ask for
/// everything and have the kernel hold a whole partition to answer.
const MAX_RESULTS: usize = 1000;

/// How many events one page reads.
///
/// Bounds the WORKING SET, separately from how many rows come back: `recall`
/// filters in Rust (the FTS index covers the assertion ledger, which modules do
/// not write to), so without paging it would have to hold a whole partition to
/// answer one query.
const RECALL_PAGE: i64 = 500;

/// The largest `kind` a module may write.
///
/// Isolation here is CONFIDENTIALITY, not a sandbox — but "module A cannot
/// affect module B" is a weaker claim than it sounds when A can write unbounded
/// blobs into the database B shares. These two caps are the cheap floor: they do
/// not make it a quota, and they do stop one module from filling `memory.db`
/// with a single call.
///
/// `pub(crate)`: T8.5c-P's `os_memory_page.rs` needs this same value for its
/// compile-time response-byte-budget proof (design §5.1) — one constant, not
/// two independently maintained copies.
pub(crate) const MAX_KIND_BYTES: usize = 128;

/// The largest serialized body a module may remember in one call.
///
/// Well under the 1 MiB HTTP body cap, because this is one memory rather than
/// one request. `pub(crate)` for the same reason as [`MAX_KIND_BYTES`].
pub(crate) const MAX_BODY_BYTES: usize = 64 * 1024;

/// The derived-key format version. Bump ONLY together with a migration that can
/// read the previous one — the catalog is what makes that possible.
///
/// `v1` was F1's `(user, module)`. `v2` is F8's `(org, space)`: same isolation,
/// a dimension that can hold more than one person.
const KEY_VERSION: &str = "v2";

/// F1's key format, kept ONLY so partitions written under it can be found and
/// re-keyed. Nothing new is ever written with this.
const LEGACY_KEY_VERSION: &str = "v1";

/// An organisation. Opaque, stable, and never parsed.
///
/// It is a value read from `mem_orgs`. Orgs the kernel creates get a generated
/// id (`org_<ULID>`) rather than one derived from whoever is logged in, which is
/// the point of F8: an org whose id is a function of a user is a user wearing an
/// org's name, and it has to be re-issued — and therefore every partition
/// re-keyed — the day it gains a second member.
///
/// **One exception, in upgraded databases**: migration 0013 has to invent an org
/// for each user F1 had already recorded a partition for, and SQL cannot mint a
/// ULID, so those rows carry `org_legacy_<user>`. Review flagged the earlier
/// wording here ("NOT something derived from whoever is logged in") as claiming
/// more than that. What actually holds for both shapes is what callers depend
/// on: the id is opaque, no code parses it, it is resolved by MEMBERSHIP, and it
/// never changes again — so a legacy org gains a second member exactly as
/// cheaply as a generated one. What does not hold is that the string is free of
/// a user's name.
///
/// No module can see either shape: a handle exposes only `osmem:<ULID>` ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgId(String);

impl OrgId {
    /// Wrap an id the store issued. Named for its caller: only the kernel, and
    /// only with a value that came from `mem_orgs`.
    pub fn from_store(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A container of memories within an org — the thing that OWNS a partition.
///
/// The user's examples are the shape to hold in mind: Team Shared, Finance
/// Private, Customer A. A person is an accessor of one, not the owner of it.
///
/// Today exactly one kind is constructible, [`Self::module_private`], which
/// reproduces F1's isolation exactly: one partition per module. Shared spaces
/// are deliberately not constructible, because nothing can grant access to one
/// — a space that cannot be granted does not exist, and a constructor for it
/// would be an API promising a capability the kernel does not have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaceId(String);

impl SpaceId {
    /// A module's own private space.
    ///
    /// The `os:` prefix is also written by migration 0013's backfill; the two
    /// are pinned to each other by a test, because one convention spelled in two
    /// places is how they drift.
    pub fn module_private(module: &str) -> Self {
        Self(format!("os:{module}"))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    /// An arbitrary space id, for testing the KEY ENCODER against inputs no
    /// production path can produce.
    ///
    /// `#[cfg(test)]` on purpose: a public one would be a constructor for shared
    /// spaces, which is the capability this type deliberately does not have yet.
    #[cfg(test)]
    pub(crate) fn raw(s: &str) -> Self {
        Self(s.to_owned())
    }
}

/// Derive a partition key. Kernel-only: nothing a module can call.
///
/// LENGTH-PREFIXED, not merely separated. A plain `v2\0{org}\0{space}` looks
/// unambiguous and is not — F1's own test found the equivalent collision in the
/// v1 format: `("a", "b\0os:c")` and `("a\0os:b", "c")` both rendered as
/// `v1\0a\0os:b\0os:c`. Both components are constrained today, but "unreachable"
/// is an argument where a length prefix is a property, and this repo has already
/// paid once for a concat identity that two different pairs could produce (MD-5's
/// `consol-{owner}-{key}`, review #122 B1).
///
/// The lengths are BYTE counts, which is why the equivalent cannot be written in
/// SQL: SQLite's `length()` counts characters, so a non-ASCII org id would give a
/// migration a key that silently disagrees with this one.
pub(crate) fn partition_key(org: &OrgId, space: &SpaceId) -> String {
    let (o, s) = (org.as_str(), space.as_str());
    format!(
        "{KEY_VERSION}\u{0}{}\u{0}{o}\u{0}{}\u{0}{s}",
        o.len(),
        s.len()
    )
}

/// F1's key, for finding what must be re-keyed. Never used to write.
pub(crate) fn legacy_partition_key(user: &str, module: &str) -> String {
    format!(
        "{LEGACY_KEY_VERSION}\u{0}{}\u{0}{user}\u{0}os:{}\u{0}{module}",
        user.len(),
        module.len()
    )
}

/// What the kernel knows about a partition that the key itself cannot say.
///
/// The adversarial review's sharpest point about design C was that future
/// GDPR/export/migration code must NOT discover partitions by prefix-matching
/// storage keys. This is the alternative: an explicit record of which logical
/// user and which module a physical key belongs to.
///
/// It is deliberately built from what the kernel already knows at mount time —
/// the resolved org, the authenticated user and the VALIDATED manifest — rather
/// than parsed back out of the key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OsMemoryPartition {
    /// The physical `scope_owner` value used in storage.
    pub key: String,
    /// The org that owns this partition.
    pub org: OrgId,
    /// The space within that org — the actual owner of the memories.
    pub space: SpaceId,
    /// The logical user the partition was created for.
    ///
    /// NOT the same fact as [`Self::org`], and kept separate for the day they
    /// stop lining up: the org is who the data belongs to, this is who caused it
    /// to exist. The export/erase path reads it.
    pub user: String,
    /// The module's manifest name AT THE TIME OF MOUNT.
    ///
    /// A rename produces a DIFFERENT key and therefore a different partition —
    /// the old one keeps its data under its old name and is never visited again.
    /// That orphan is exactly the debt the catalog exists to make payable.
    pub module: String,
}

/// Every partition this daemon handed out THIS RUN, in mount order.
///
/// Held by the kernel, never by a module.
///
/// # This is the in-memory half
///
/// The durable half is the `mem_os_partitions` table (migration 0012), and the
/// distinction is not academic. The first version of this type was ONLY the
/// `Vec`, with a doc-comment claiming that recording partitions now was what
/// stopped future production data from becoming unrecoverable. Adversarial
/// review pointed out that this was exactly backwards: a `Vec` rebuilt from
/// whichever modules happened to mount, then dropped after a startup log, knows
/// nothing about
///
/// - partitions written by previous daemon runs,
/// - modules that are currently disabled or have been uninstalled,
/// - the partition a module left behind when it was renamed,
/// - partitions created under an older [`KEY_VERSION`].
///
/// Those four are the entire reason a catalog was required. So
/// [`Self::ensure_recorded`] now WRITES, and the `Vec` (populated by
/// [`Self::mark_mounted`]) is what it says it is: this run's mount
/// inventory, used for the startup log and for tests. Anything asking
/// "which partitions exist for this org" must ask the table —
/// [`Self::durable_for_org`] — not this.
#[derive(Debug, Clone, Default)]
pub struct OsMemoryCatalog {
    partitions: Vec<OsMemoryPartition>,
}

impl OsMemoryCatalog {
    /// Ensure the durable identity row for `(user, manifest)` exists.
    ///
    /// T8.5c-W-mount decision 5 (H1): this does **not** touch this run's
    /// in-memory inventory and does **not** advance `last_seen_at` — it runs
    /// BEFORE the kernel knows whether the module will actually mount
    /// successfully, so it must not let the durable catalog claim a
    /// partition is "just seen active" on a mount that then fails. The
    /// caller must call [`Self::mark_mounted`] once it has confirmed the
    /// mount actually succeeded.
    ///
    /// Fallible ON PURPOSE, and the caller must not lend a partition it could
    /// not record: an unrecorded partition is precisely the orphaned data
    /// this exists to prevent — rows under a NUL-containing owner key that
    /// nothing can later attribute to a user or a module.
    pub async fn ensure_recorded(
        &self,
        org: &OrgId,
        user: &str,
        manifest: &DomainOsManifest,
        kv: &agent24_memory::KvStore,
    ) -> Result<OsMemoryPartition, String> {
        let space = SpaceId::module_private(manifest.name());
        let p = OsMemoryPartition {
            key: partition_key(org, &space),
            org: org.clone(),
            space,
            user: user.to_owned(),
            module: manifest.name().to_owned(),
        };
        kv.record_os_partition(agent24_memory::OsPartitionIdentity {
            owner_key: &p.key,
            key_version: KEY_VERSION,
            org_id: p.org.as_str(),
            space_id: p.space.as_str(),
            user: &p.user,
            module: &p.module,
        })
        .await
        .map_err(|e| e.to_string())?;
        Ok(p)
    }

    /// Confirm that `partition`'s module really did mount successfully this
    /// run — routes are nested (in-process) or the supervisor has registered
    /// it as a running child (OOP; see the module docs for what that
    /// boundary does and does not mean). This is the only true source of
    /// [`Self::partitions`] ("what mounted this run"), and the only call
    /// site allowed to advance the durable `last_seen_at` signal.
    ///
    /// Idempotent by the partition's physical `key` (T8.5c-W-mount M2): a
    /// second call for the same partition within this run returns
    /// immediately — it neither adds a second entry to this run's inventory
    /// nor touches the durable timestamp again (the implementation below
    /// checks `self.partitions` and returns before doing either; it does
    /// NOT fall through to a harmless re-touch).
    pub async fn mark_mounted(
        &mut self,
        partition: OsMemoryPartition,
        kv: &agent24_memory::KvStore,
        clock: &dyn agent24_memory::Clock,
    ) {
        if self.partitions.iter().any(|p| p.key == partition.key) {
            return;
        }
        // Best-effort: the caller has already decided this mount succeeded
        // (its precondition for calling this at all — see the two call
        // sites in `domain.rs`), so a failure to advance the durable
        // timestamp must not un-mount it. It only makes the durable catalog
        // under-report this partition's liveness until the next successful
        // mount, which is logged rather than propagated.
        if let Err(e) = kv.touch_os_partition_last_seen(&partition.key, clock).await {
            tracing::warn!(
                module = %partition.module,
                error = %e,
                "mounted, but could not advance this partition's last-seen-at; \
                 the durable catalog will under-report its liveness until the \
                 next successful mount"
            );
        }
        self.partitions.push(partition);
    }

    /// Re-key every partition still stored under F1's `v1` format.
    ///
    /// Returns how many partitions moved. Errors are per-partition and do NOT
    /// abort the sweep: one partition whose target key is already taken must not
    /// stop the others from migrating, because leaving them on v1 means the
    /// kernel derives a v2 key at mount, finds an empty partition, and the
    /// module silently loses its history. The failure is logged and the row
    /// stays on v1 so a later run can retry it.
    ///
    /// # This is the catalog's first real job
    ///
    /// F1 built `mem_os_partitions` for exactly this — "a future export, erase or
    /// key-version migration has an explicit list instead of prefix-matching
    /// strings that contain NUL" — and then shipped without ever exercising it.
    /// Doing the v1→v2 move through the catalog now, while the only rows that
    /// exist are on developer machines that ran `main` since yesterday, is the
    /// one chance to find out whether that mechanism works while being wrong
    /// costs nothing.
    pub async fn migrate_legacy_partitions(kv: &agent24_memory::KvStore) -> Result<usize, String> {
        let stale = kv
            .os_partitions_with_key_version(LEGACY_KEY_VERSION)
            .await
            .map_err(|e| e.to_string())?;
        let mut moved = 0usize;
        for row in stale {
            // Recomputed, never trusted: if the stored key does not match what
            // F1's encoder would have produced for this row's own identity, the
            // row and the key disagree and this code does not know which is
            // right. Rewriting on a guess is how one module's memories end up in
            // another's partition.
            let expected = legacy_partition_key(&row.logical_user, &row.module_name);
            if expected != row.owner_key {
                tracing::error!(
                    owner_key = %row.owner_key.escape_debug(),
                    "catalog row does not match the v1 key its own (user, module) \
                     would produce; leaving it alone rather than re-keying on a guess"
                );
                continue;
            }
            let org = OrgId::from_store(&row.org_id);
            let space = SpaceId::module_private(&row.module_name);
            if space.as_str() != row.space_id {
                tracing::error!(
                    owner_key = %row.owner_key.escape_debug(),
                    "catalog row's space_id disagrees with its module_name; leaving it"
                );
                continue;
            }
            let new_key = partition_key(&org, &space);
            match kv
                .rekey_os_partition(&row.owner_key, &new_key, KEY_VERSION)
                .await
            {
                Ok(events) => {
                    moved += 1;
                    tracing::info!(
                        module = %row.module_name,
                        events,
                        "re-keyed a v1 memory partition onto its (org, space) identity"
                    );
                }
                // The module does NOT then mount with an empty partition: the
                // stale v1 row still holds this (org, space), and that pair is
                // UNIQUE in the catalog, so `ensure_recorded` fails and `lend`
                // withholds the capability entirely. Losing memory for a run is
                // the correct outcome; silently starting a fresh partition
                // beside the old one is not.
                Err(e) => tracing::error!(
                    module = %row.module_name,
                    error = %e,
                    "could not re-key a v1 memory partition; it stays on v1, and this \
                     module will be refused the memory capability until it is resolved"
                ),
            }
        }
        Ok(moved)
    }

    /// What mounted this run. NOT the answer to "what exists" — see the type docs.
    pub fn partitions(&self) -> &[OsMemoryPartition] {
        &self.partitions
    }

    /// Every partition EVER recorded for `org`, from the durable table.
    ///
    /// The answer an export or erase path needs, and the reason it must not be a
    /// `LIKE` query over keys that contain NUL. Includes partitions belonging to
    /// modules that are disabled, uninstalled or renamed — which is the whole
    /// point, and the thing this run's [`Self::partitions`] cannot tell you.
    ///
    /// # Keyed by ORG, because that is what owns a partition
    ///
    /// This took the user until round 3 to follow the storage layer. It was
    /// `durable_for(kv, user)` over `os_partitions_for`, which after round 2
    /// answers only "what did this user CREATE" — so the startup inventory
    /// undercounted by exactly the partitions a second member had written to but
    /// not created, which is the population F8 exists for. Review caught that the
    /// storage layer had been split and its caller had not.
    pub async fn durable_for_org(
        kv: &agent24_memory::KvStore,
        org: &OrgId,
    ) -> Result<Vec<agent24_memory::OsPartitionRow>, String> {
        kv.os_partitions_for_org(org.as_str())
            .await
            .map_err(|e| e.to_string())
    }
}

/// T8.5c-W-mount decision 1: mount layer's single source of truth for
/// "can this (out-of-process) module use `_a24/memory/private/*` right
/// now, or the future `_a24/memory/scoped/*`".
///
/// Invariant: `private` is `Some` if and only if all three required parts —
/// a real `Arc<OsScopedMemory>`, this `(module, partition)`'s own
/// `Arc<RateLimiter>` (decision 3), and the daemon-level shared
/// `Arc<Semaphore>` (decision 4) — are present together.
/// [`PrivateMemoryHandle`]'s three fields are none of them `Option`, so the
/// only way `private` can be `None` is the whole `Option` being `None` —
/// there is no "half a handle" state.
#[derive(Clone)]
pub struct MemoryEntitlement {
    private: Option<PrivateMemoryHandle>,
    /// Always `None` — SPEC §3 leaves door 5: an old manifest's `memory`
    /// maps only to `private`; `scoped` is a future capability. No
    /// construction path can set this to `Some` today (`ScopedMemoryHandle`
    /// declares no fields, see below).
    ///
    /// `#[allow(dead_code)]`: never READ today for the same reason
    /// `PrivateMemoryHandle`'s fields below are not — nothing consumes
    /// `MemoryEntitlement` yet except this mount layer's own `granted`/
    /// `provides` filtering (`memory_grant_name`), which only asks
    /// `private_handle().is_some()`. `_a24/memory/scoped/*`'s wire
    /// implementation (F8c/F9) is what will read it.
    #[allow(dead_code)]
    scoped: Option<ScopedMemoryHandle>,
}

/// The three parts an OOP module needs to actually call
/// `remember_checked`/`recall_page`/`recent_page` — all real, all required.
///
/// `#[allow(dead_code)]`: this mount-layer design doc's job stops at handing
/// this struct to T8.5c-W-wire's `Handler::call()` implementation, which is
/// what actually reads `memory`/`limiter`/`admission` — not built yet, so
/// rustc's `--bin`-target reachability analysis (see `os_memory_page.rs`'s
/// module doc for why `cargo test` does not silence this) sees three fields
/// that are written but never read.
#[allow(dead_code)]
#[derive(Clone)]
pub struct PrivateMemoryHandle {
    pub memory: Arc<OsScopedMemory>,
    pub limiter: Arc<RateLimiter>,
    /// Always real — not `Option`. `Option` lives only in
    /// [`crate::domain::MemoryLease::admission`]'s return value;
    /// [`PrivateMemoryHandle`] is only ever constructed once that call
    /// already returned `Some` (T8.5c-W-mount decision 4).
    pub admission: Arc<Semaphore>,
}

/// A placeholder type with no fields today — `Infallible` makes it
/// uninhabited, turning "`scoped` is not reachable yet" from a doc promise
/// into a compile-time fact. When F8c/F9 design `scoped` for real, this
/// type gains real fields and the `Option<ScopedMemoryHandle>` construction
/// path opens up — `MemoryEntitlement`'s shape does not need to change.
#[derive(Clone)]
pub struct ScopedMemoryHandle(std::convert::Infallible);

impl MemoryEntitlement {
    pub const NONE: MemoryEntitlement = MemoryEntitlement {
        private: None,
        scoped: None,
    };

    pub fn private(handle: PrivateMemoryHandle) -> MemoryEntitlement {
        MemoryEntitlement {
            private: Some(handle),
            scoped: None,
        }
    }

    pub fn private_handle(&self) -> Option<&PrivateMemoryHandle> {
        self.private.as_ref()
    }
}

/// T8.5c-W-mount decision 3/4: given the result of one `lend()` call (just
/// the two `Arc`s this function actually needs — `partition` is left with
/// the caller, for `mark_mounted`, so this cannot accidentally consume it),
/// build the `MemoryEntitlement` that mount should hand this module. Pure —
/// no I/O, no dependency on any other local state in `mount_package` — so it
/// is unit-testable without running a mount at all.
pub(crate) fn build_private_memory_entitlement(
    lend: Option<(Arc<OsScopedMemory>, Arc<Semaphore>)>,
) -> MemoryEntitlement {
    match lend {
        Some((scoped, admission)) => MemoryEntitlement::private(PrivateMemoryHandle {
            memory: scoped,
            // Exactly one limiter per call to this function — not inside the
            // `methods_for` closure, which runs once per restart generation
            // and must only ever CLONE this `Arc`, never rebuild it (unlike
            // `_a24/events/emit`'s deliberately-per-generation limiter).
            limiter: Arc::new(RateLimiter::new(
                crate::os_memory_page::MEMORY_RATE_CAPACITY,
                crate::os_memory_page::MEMORY_RATE_REFILL_PER_SEC,
            )),
            admission,
        }),
        None => MemoryEntitlement::NONE,
    }
}

/// T8.5c-W-mount decision 2 (§5.3): whether `"memory"` belongs in
/// `MountReport.granted`/`Offer.provides` — the single rule both call sites
/// in `mount_package` use, so they cannot drift apart. `Some("memory")` iff
/// `entitlement` really carries a handle: granting the capability without a
/// handle would be a lie the caller has no way to detect.
pub(crate) fn memory_grant_name(entitlement: &MemoryEntitlement) -> Option<&'static str> {
    entitlement
        .private_handle()
        .is_some()
        .then_some(Capability::Memory.as_str())
}

/// A module's handle onto the shared memory base.
///
/// Holds the derived key and nothing a module could widen. It stores the pool
/// indirectly through `EventLog`, and deliberately exposes no accessor for it:
/// `KvStore` is the ROOT handle in `agent24-memory` — one yields events,
/// artifacts, assertions, retrievers, consolidation, knowledge, trace and
/// vectors — so a handle that leaked it would isolate nothing.
pub struct OsScopedMemory {
    key: String,
    module: String,
    events: agent24_memory::event::EventLog,
}

impl OsScopedMemory {
    pub fn new(partition: &OsMemoryPartition, kv: &agent24_memory::KvStore) -> Self {
        Self {
            key: partition.key.clone(),
            module: partition.module.clone(),
            events: kv.events(),
        }
    }

    /// Mint an id for a module's memory.
    ///
    /// `osmem:<ULID>` — deliberately carrying NOTHING about the partition.
    ///
    /// The first version prefixed the partition key, reasoning that since
    /// `mem_events.id` is globally UNIQUE, a shared id would let one module DENY
    /// another's write, and a prefix makes that unrepresentable rather than
    /// unlikely. Review pointed out what it cost: the partition key contains the
    /// logical USER id verbatim and a NUL byte, and this string is handed straight
    /// back to the module. A module with no other route to the user's identity
    /// could read it out of an id, and a NUL-bearing identifier is a hazard
    /// anywhere it is logged, rendered, or put on a wire.
    ///
    /// Dropping the prefix is NOT the same trade this file refused earlier for
    /// `partition_key`. There the collision was deterministic and reachable from
    /// constructible inputs, which is why length-prefixing it was worth a
    /// property. Here nothing the module supplies reaches the id at all — the
    /// kernel mints it — so a cross-module collision needs a ULID collision (80
    /// random bits within one millisecond), and its outcome is a hard error rather
    /// than an alias: `EventLog::append` refuses an existing id under a different
    /// owner instead of merging into it. Negligible probability with a safe
    /// failure is a different thing from an input an adversary can construct.
    fn mint_id(&self) -> String {
        format!("osmem:{}", agent24_core::util::ulid())
    }

    /// One page of this partition's events, NEWEST first, older than `before`
    /// (or from the newest end when `before` is `None`).
    ///
    /// Backwards on purpose. The first version paged forwards from seq 0 and
    /// stopped at a short page, which is both O(partition) for a bounded answer
    /// and — as review pointed out — not guaranteed to terminate: a module
    /// appending while another task reads keeps every page full, so the loop
    /// chases a tail that keeps moving. A descending cursor only ever decreases,
    /// so concurrent appends land above it and the walk ends.
    ///
    /// Honest about the size of that second argument: keeping a page full needs
    /// [`RECALL_PAGE`] appends per page-read, which a real writer does not sustain
    /// — an attempt to pin it with a test failed to distinguish the two shapes at
    /// all (see the note in this file's tests). The bound is worth having because
    /// it is structural rather than a matter of relative speed, but the reason to
    /// read backwards is the first one: `recent` becomes one query.
    async fn page(&self, before: Option<i64>, size: i64) -> agent24_domain::Result<Page> {
        let mut q = EventQuery::owner(&self.key).newest().limit(size);
        if let Some(b) = before {
            q = q.before(b);
        }
        let rows = self
            .events
            .scan(&q)
            .await
            .map_err(|e| DomainError::Store(e.to_string()))?;
        let short = (rows.len() as i64) < size;
        let oldest_seq = rows.last().map(|r| r.seq);
        Ok(Page {
            items: rows.into_iter().map(to_recollection).collect(),
            oldest_seq,
            short,
        })
    }
}

/// One page of a partition's events, newest first.
struct Page {
    items: Vec<Recollection>,
    /// The lowest seq in this page — the cursor for the next (older) page.
    /// `None` when the page is empty, which is also when the walk is over.
    oldest_seq: Option<i64>,
    /// Fewer rows than asked for, so there is nothing older than this page.
    short: bool,
}

/// `pub(crate)`: T8.5c-P's `page_from_stream` (`os_memory_page.rs`) needs the
/// exact same row → `Recollection` mapping `page()`'s in-process path uses,
/// so the two never disagree about what a stored event looks like on the
/// wire.
pub(crate) fn to_recollection(s: agent24_memory::event::StoredEvent) -> Recollection {
    Recollection {
        id: MemoryId::from_kernel(s.event.id),
        kind: s.event.kind,
        body: match s.event.body {
            serde_json::Value::Object(m) => m,
            // The envelope requires an object and `remember` only ever writes one,
            // so this is unreachable through this handle.
            _ => serde_json::Map::new(),
        },
        at: s.event.at,
    }
}

impl OsScopedMemory {
    /// The bound checks and `MemEvent` construction `remember`/
    /// `remember_checked` (T8.5c-P) both need — factored out so the
    /// in-process trait path and the OOP wire path can never validate a
    /// `Remember` differently. Returns a human-readable message on failure;
    /// each caller maps it into its own error type (`DomainError` for the
    /// trait path, `MemoryRpcError` for the wire path).
    fn build_remember_event(&self, what: Remember) -> Result<MemEvent, String> {
        // Bounded BEFORE anything is written. See `MAX_KIND_BYTES`: the partition
        // stops A from reading B, and these stop A from crowding B out of the
        // database they share.
        if what.kind.trim().is_empty() {
            return Err("kind must not be empty".into());
        }
        if what.kind.len() > MAX_KIND_BYTES {
            return Err(format!("kind exceeds {MAX_KIND_BYTES} bytes"));
        }
        let body = serde_json::Value::Object(what.body);
        let encoded =
            serde_json::to_string(&body).map_err(|e| format!("body is not serialisable: {e}"))?;
        if encoded.len() > MAX_BODY_BYTES {
            return Err(format!(
                "body is {} bytes, over the {MAX_BODY_BYTES}-byte limit for one memory",
                encoded.len()
            ));
        }
        Ok(MemEvent::new(
            self.mint_id(),
            // The scope the module never gets to choose. `agent` records the
            // module for diagnostics ONLY — it is enforced nowhere, which the
            // contract says out loud; `owner` is what actually isolates.
            Scope {
                owner: self.key.clone(),
                agent: Some(self.module.clone()),
                session: None,
                run: None,
            },
            what.kind,
            body,
            Origin {
                source: format!("os:{}", self.module),
                // UNCONDITIONALLY `ToolOutput`, and that is a deliberate, stated
                // limitation rather than a classification.
                //
                // What it means: "a module produced this". What it does NOT mean:
                // "this content originated with the module". A domain OS that
                // remembers something it fetched from the web records it here as
                // ToolOutput, and the write gate treats ToolOutput (held, mapped to
                // `Observed`) more leniently than WebFetch (rejected). So this
                // boundary CAN launder upstream provenance, and must not be relied
                // on as a provenance signal.
                //
                // The alternative — letting a module declare provenance — trades a
                // known limitation for a forgeable field, and would need a
                // constrained subset that cannot claim `System` or `UserSaid`. That
                // belongs with the assertion path (F2), not here: nothing consumes
                // these events for assertions or consolidation today, which is what
                // keeps this a documented limitation rather than a live hole.
                trust: Trust::ToolOutput,
            },
        ))
    }
}

#[async_trait::async_trait]
impl ScopedMemory for OsScopedMemory {
    async fn remember(&self, what: Remember) -> agent24_domain::Result<Remembered> {
        let ev = self
            .build_remember_event(what)
            .map_err(DomainError::Memory)?;
        let at = ev.at.clone();
        let id = ev.id.clone();
        self.events
            .append(&ev)
            .await
            .map_err(|e| DomainError::Store(e.to_string()))?;
        Ok(Remembered {
            id: MemoryId::from_kernel(id),
            at,
        })
    }

    async fn recall(&self, query: &str, limit: usize) -> agent24_domain::Result<Vec<Recollection>> {
        // Substring matching over this partition's own events. Deliberately NOT
        // the FTS retriever: that indexes the ASSERTION ledger, which modules do
        // not write to, and reaching for it would also drag in `rebuild()` — a
        // global operation no module should hold.
        //
        // Paged BACKWARDS from the newest, stopping as soon as `want` matches are
        // in hand. Two earlier shapes were wrong and both are worth remembering:
        // `recent(usize::MAX)` became `LIMIT i64::MAX` and defeated the memory
        // crate's own scan cap; forward paging to a short page could not terminate
        // against a concurrent writer. Backwards, the cursor only decreases, and a
        // query whose matches are recent costs a page rather than a partition. It
        // stops when `want` matches are in hand or history runs out — so matches
        // OLDER than the newest `want` are deliberately not returned, which is what
        // a limit means; what it does not do is stop at a page boundary and call
        // that the end.
        let want = limit.min(MAX_RESULTS);
        if want == 0 {
            return Ok(Vec::new());
        }
        let needle = query.trim().to_lowercase();
        let mut hits: Vec<Recollection> = Vec::new();
        let mut cursor: Option<i64> = None;
        loop {
            let page = self.page(cursor, RECALL_PAGE).await?;
            cursor = page.oldest_seq;
            for r in page.items {
                let hit = needle.is_empty()
                    || r.kind.to_lowercase().contains(&needle)
                    || serde_json::to_string(&r.body)
                        .unwrap_or_default()
                        .to_lowercase()
                        .contains(&needle);
                if hit {
                    hits.push(r);
                    if hits.len() == want {
                        return Ok(hits);
                    }
                }
            }
            if page.short || cursor.is_none() {
                return Ok(hits);
            }
        }
    }

    async fn recent(&self, limit: usize) -> agent24_domain::Result<Vec<Recollection>> {
        // ONE descending query. Not a walk: "the newest N" is exactly what
        // `seq DESC LIMIT N` returns, so the work is proportional to the answer
        // rather than to the partition, and there is no loop for a concurrent
        // writer to keep alive.
        //
        // It got here the long way. First `LIMIT n` over an ASCENDING scan then
        // reversed — which returns the OLDEST n backwards, and every test used a
        // limit larger than the row count, where the two agree. Then a forward
        // walk keeping a ring of the last n, which was correct but O(partition)
        // and unbounded in time against an active writer.
        let want = limit.min(MAX_RESULTS);
        if want == 0 {
            return Ok(Vec::new());
        }
        Ok(self.page(None, want as i64).await?.items)
    }
}

// ---- T8.5c-P: the OOP wire entry points — `recall_page`/`recent_page`/
// `remember_checked` (T8.5c v1 decision D4 already planned these three
// existing; this crate gives them their real, fully-wired bodies). Inherent,
// not on `ScopedMemory` (that trait is the IN-PROCESS capability surface;
// these are the paginated/budgeted/billed OOP-wire surface T8.5c v1 D4
// distinguishes them from — see this file's module doc).
//
// `limiter`/`admission`/`lifecycle` are explicit parameters rather than
// fields on `OsScopedMemory` because creating the daemon-level `RateLimiter`/
// `Semaphore` SINGLETONS (design §6.3's "mount 层创建一次" / §6.5's "与共享
// 池同作用域创建一次") is mount-time wiring that belongs in `domain.rs`/
// `server.rs` — outside this design doc's scope (§0) and explicitly left to
// T8.5c-W (§12). These three methods are where those singletons get USED.
//
// `#[allow(dead_code)]`: real, tested code (this crate's own tests below and
// `os_memory_page.rs`'s) with no caller reachable from `main` yet, for the
// same reason `os_memory_page.rs`'s module doc explains — `dead_code`'s
// binary-crate reachability analysis starts at `main` and does not see
// `#[cfg(test)]` code.
#[allow(dead_code)]
impl OsScopedMemory {
    /// Design §6.5's `remember_checked` pseudocode: reservation → admission
    /// permit (inside the `bind_to_lifecycle`-wrapped work future) → append →
    /// commit. `ev` is validated up front, outside the reservation/permit —
    /// a malformed `Remember` must not spend a rate-limit token or wait on
    /// the shared connection semaphore (decision P5's "capability first,
    /// cost second" ordering, mirrored from `_a24/events/emit`).
    pub async fn remember_checked(
        &self,
        lifecycle: Option<RequestLifecycle>,
        limiter: Arc<RateLimiter>,
        admission: Arc<Semaphore>,
        what: Remember,
    ) -> Result<Remembered, MemoryRpcError> {
        let ev = self
            .build_remember_event(what)
            .map_err(MemoryRpcError::Invalid)?;
        let Some(mut reservation) = Reservation::reserve(limiter, MEMORY_COST_REMEMBER) else {
            return Err(MemoryRpcError::application(
                ErrorKind::RateLimited,
                "memory rate limit exceeded",
            ));
        };
        let events = self.events.clone();
        let outcome = bind_to_lifecycle(lifecycle, async move {
            let _permit = admission.acquire_owned().await.map_err(|_| {
                MemoryRpcError::Store("connection admission semaphore closed".into())
            })?;
            // About to touch the shared pool — see `Reservation::mark_touched`
            // for the precise (conservative) meaning of this boundary.
            reservation.mark_touched();
            events
                .append(&ev)
                .await
                .map_err(|e| MemoryRpcError::Store(e.to_string()))?;
            reservation.commit();
            Ok(Remembered {
                id: MemoryId::from_kernel(ev.id.clone()),
                at: ev.at.clone(),
            })
        })
        .await;
        match outcome {
            Ok(inner) => inner,
            Err(timeout) => Err(MemoryRpcError::from(timeout)),
        }
    }

    /// Design §4.2/§6.5's `recall_page`: decode+validate the cursor against
    /// `needle` (§7.1's fingerprint check), reserve the worst-case scan cost,
    /// then — inside the `bind_to_lifecycle`-wrapped, fully owned work
    /// future — acquire the shared admission permit and drive
    /// [`page_from_stream`] over a real [`agent24_memory::event::EventLog::scan_stream`].
    pub async fn recall_page(
        &self,
        lifecycle: Option<RequestLifecycle>,
        limiter: Arc<RateLimiter>,
        admission: Arc<Semaphore>,
        needle: &Needle,
        page_size: usize,
        cursor: Option<&str>,
    ) -> Result<RecallPage, MemoryRpcError> {
        if page_size == 0 || page_size > MEMORY_MAX_PAGE_SIZE {
            return Err(MemoryRpcError::Invalid(format!(
                "page_size must be between 1 and {MEMORY_MAX_PAGE_SIZE}"
            )));
        }
        let resolved_seq = match cursor {
            Some(token) => Some(decode_cursor(token, METHOD_TAG_RECALL, needle)?),
            None => None,
        };
        let Some(mut reservation) = Reservation::reserve(limiter, MEMORY_COST_RECALL) else {
            return Err(MemoryRpcError::application(
                ErrorKind::RateLimited,
                "memory rate limit exceeded",
            ));
        };
        let events = self.events.clone();
        let key = self.key.clone();
        let needle = needle.clone();
        let outcome = bind_to_lifecycle(lifecycle, async move {
            let _permit = admission.acquire_owned().await.map_err(|_| {
                MemoryRpcError::Store("connection admission semaphore closed".into())
            })?;
            let mut q = EventQuery::owner(&key)
                .newest()
                .limit(MEMORY_SCAN_ROW_BUDGET as i64);
            if let Some(s) = resolved_seq {
                q = q.before(s);
            }
            let mut stream = std::pin::pin!(events.scan_stream(&q));
            let page = page_from_stream(
                stream.as_mut(),
                PageMode::Recall(&needle),
                page_size,
                MEMORY_SCAN_ROW_BUDGET,
                MEMORY_PAGE_RESPONSE_BUDGET_BYTES,
                resolved_seq,
                &mut reservation,
            )
            .await?;
            reservation.commit();
            Ok(page)
        })
        .await;
        match outcome {
            Ok(inner) => inner,
            Err(timeout) => Err(MemoryRpcError::from(timeout)),
        }
    }

    /// Design §4.2/§6.5's `recent_page` — same shape as `recall_page`, with
    /// `MatchPolicy::Always` (via [`PageMode::Recent`]), the reservation sized
    /// to `page_size` (design §6.3's `memory_cost_recent`, v3 L2: a
    /// reservation, not a promise the settlement equals it), and
    /// [`Needle::none_for_recent`]'s fixed empty-string cursor fingerprint.
    pub async fn recent_page(
        &self,
        lifecycle: Option<RequestLifecycle>,
        limiter: Arc<RateLimiter>,
        admission: Arc<Semaphore>,
        page_size: usize,
        cursor: Option<&str>,
    ) -> Result<RecallPage, MemoryRpcError> {
        if page_size == 0 || page_size > MEMORY_MAX_PAGE_SIZE {
            return Err(MemoryRpcError::Invalid(format!(
                "page_size must be between 1 and {MEMORY_MAX_PAGE_SIZE}"
            )));
        }
        let fingerprint_needle = Needle::none_for_recent();
        let resolved_seq = match cursor {
            Some(token) => Some(decode_cursor(
                token,
                METHOD_TAG_RECENT,
                &fingerprint_needle,
            )?),
            None => None,
        };
        let Some(mut reservation) = Reservation::reserve(limiter, memory_cost_recent(page_size))
        else {
            return Err(MemoryRpcError::application(
                ErrorKind::RateLimited,
                "memory rate limit exceeded",
            ));
        };
        let events = self.events.clone();
        let key = self.key.clone();
        let outcome = bind_to_lifecycle(lifecycle, async move {
            let _permit = admission.acquire_owned().await.map_err(|_| {
                MemoryRpcError::Store("connection admission semaphore closed".into())
            })?;
            let mut q = EventQuery::owner(&key).newest().limit(page_size as i64);
            if let Some(s) = resolved_seq {
                q = q.before(s);
            }
            let mut stream = std::pin::pin!(events.scan_stream(&q));
            let page = page_from_stream(
                stream.as_mut(),
                PageMode::Recent,
                page_size,
                page_size,
                MEMORY_PAGE_RESPONSE_BUDGET_BYTES,
                resolved_seq,
                &mut reservation,
            )
            .await?;
            reservation.commit();
            Ok(page)
        })
        .await;
        match outcome {
            Ok(inner) => inner,
            Err(timeout) => Err(MemoryRpcError::from(timeout)),
        }
    }
}

/// A [`KernelCtx`](agent24_domain::KernelCtx) that also lends memory and
/// approval (T7b/ME-3e).
pub struct MemoryCtx {
    pub sink: Option<agent24_domain::EventSink>,
    pub memory: Option<Arc<OsScopedMemory>>,
    pub approval: Option<agent24_domain::ApprovalRequester>,
}

impl agent24_domain::KernelCtx for MemoryCtx {
    fn events(&self) -> Option<&agent24_domain::EventSink> {
        self.sink.as_ref()
    }
    fn memory(&self) -> Option<&dyn ScopedMemory> {
        self.memory.as_deref().map(|m| m as &dyn ScopedMemory)
    }
    fn approval(&self) -> Option<&agent24_domain::ApprovalRequester> {
        self.approval.as_ref()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn manifest(name: &str) -> DomainOsManifest {
        DomainOsManifest::from_yaml(&format!(
            "name: {name}\nversion: \"0.1.0\"\nroute_namespace: /api/v1/{name}\n\
             event_module: {name}\ndata_dir: ~/.agent24/os/{name}/\n\
             kernel_capabilities: [memory]\nimpl_kind: in_process_crate\n"
        ))
        .unwrap()
    }

    /// The org a user acts in, through the same resolver the daemon uses — so a
    /// test cannot accidentally pin an org id the kernel would never produce.
    async fn org_of(kv: &agent24_memory::KvStore, user: &str) -> OrgId {
        OrgId::from_store(kv.ensure_org_for_user(user).await.unwrap())
    }

    async fn handle(kv: &agent24_memory::KvStore, user: &str, name: &str) -> OsScopedMemory {
        let cat = OsMemoryCatalog::default();
        let org = org_of(kv, user).await;
        let p = cat
            .ensure_recorded(&org, user, &manifest(name), kv)
            .await
            .unwrap();
        OsScopedMemory::new(&p, kv)
    }

    /// Test-only convenience for the (many) tests here that only care about
    /// the OLD, single-step `record` behaviour — `ensure_recorded` followed
    /// immediately by `mark_mounted`, as if the mount that follows always
    /// succeeds. The tests that specifically exercise the split (H1) call
    /// the two steps separately instead of using this.
    async fn record_and_mark(
        cat: &mut OsMemoryCatalog,
        org: &OrgId,
        user: &str,
        manifest: &DomainOsManifest,
        kv: &agent24_memory::KvStore,
    ) -> OsMemoryPartition {
        let p = cat.ensure_recorded(org, user, manifest, kv).await.unwrap();
        cat.mark_mounted(p.clone(), kv, &agent24_memory::SystemClock)
            .await;
        p
    }

    #[tokio::test]
    async fn two_modules_under_one_user_cannot_read_each_other() {
        // THE question this whole piece exists to answer. Before it, two domain
        // OSes mounted under one owner shared the memory base.
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();
        let sin90 = handle(&kv, "alice", "sin90").await;
        let cos72 = handle(&kv, "alice", "cos72").await;

        let mut body = serde_json::Map::new();
        body.insert("secret".into(), "sin90 only".into());
        sin90.remember(Remember::new("note", body)).await.unwrap();

        assert_eq!(sin90.recent(10).await.unwrap().len(), 1);
        assert!(
            cos72.recent(10).await.unwrap().is_empty(),
            "the other module must see NOTHING"
        );
        assert!(
            cos72.recall("sin90 only", 10).await.unwrap().is_empty(),
            "and must not find it by searching for its content either"
        );
    }

    #[tokio::test]
    async fn a_module_cannot_reach_the_users_own_memory() {
        // The agent loop keys memory by the bare user id. A module's key starts
        // with the version marker, so the two can never be equal — the module
        // cannot read the user's memory, and its writes do not pollute it.
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();
        let log = kv.events();
        log.append(&MemEvent::new(
            "user-own-1",
            Scope::owner("alice"),
            "chat",
            serde_json::json!({"said": "private"}),
            Origin {
                source: "agent".into(),
                trust: Trust::UserSaid,
            },
        ))
        .await
        .unwrap();

        let sin90 = handle(&kv, "alice", "sin90").await;
        assert!(sin90.recent(10).await.unwrap().is_empty());
        assert!(sin90.recall("private", 10).await.unwrap().is_empty());

        // And the reverse: what the module remembers is not in the user's own
        // partition.
        sin90
            .remember(Remember::new("note", serde_json::Map::new()))
            .await
            .unwrap();
        let user_side = log.scan(&EventQuery::owner("alice")).await.unwrap();
        assert_eq!(user_side.len(), 1, "still just the user's own event");
    }

    #[tokio::test]
    async fn the_same_module_for_two_users_is_two_partitions() {
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();
        let alice = handle(&kv, "alice", "sin90").await;
        let bob = handle(&kv, "bob", "sin90").await;
        alice
            .remember(Remember::new("note", serde_json::Map::new()))
            .await
            .unwrap();
        assert_eq!(alice.recent(10).await.unwrap().len(), 1);
        assert!(bob.recent(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn two_modules_can_both_remember_without_denying_each_other() {
        // The non-read leak the review found: `mem_events.id` is globally UNIQUE,
        // so if modules minted their own ids, one taking "note-1" would make the
        // other's write FAIL. They could not read each other — but one could stop
        // the other working. Ids are kernel-minted, so a module cannot aim at
        // another's; see `a_shared_id_is_refused_rather_than_aliased` for what
        // happens in the improbable case that two minted ids agree anyway.
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();
        let a = handle(&kv, "alice", "sin90").await;
        let b = handle(&kv, "alice", "cos72").await;
        for _ in 0..5 {
            a.remember(Remember::new("note", serde_json::Map::new()))
                .await
                .expect("sin90 writes");
            b.remember(Remember::new("note", serde_json::Map::new()))
                .await
                .expect("cos72 writes — neither may deny the other");
        }
        assert_eq!(a.recent(10).await.unwrap().len(), 5);
        assert_eq!(b.recent(10).await.unwrap().len(), 5);

        // And what an id must NOT contain. It used to carry the partition key,
        // which embeds the logical user and a NUL byte, and it is handed straight
        // back to the module — so a module with no other route to the user's
        // identity could simply read it out of an id it was given.
        for r in a
            .recent(10)
            .await
            .unwrap()
            .iter()
            .chain(b.recent(10).await.unwrap().iter())
        {
            let id = r.id.as_str();
            assert!(id.starts_with("osmem:"), "{id:?}");
            assert!(
                !id.contains('\u{0}'),
                "no NUL in an id that reaches logs, \
                JSON and one day a wire: {id:?}"
            );
            assert!(
                !id.contains("alice"),
                "an id must not disclose the user: {id:?}"
            );
            assert!(!id.contains("sin90") && !id.contains("cos72"), "{id:?}");
        }
        assert_ne!(a.key, b.key);
    }

    #[tokio::test]
    async fn a_shared_id_is_refused_rather_than_aliased() {
        // What makes dropping the partition prefix from `mint_id` safe. Two modules
        // colliding needs a ULID collision, and if it ever happened the store
        // REFUSES the second write instead of quietly merging it into the first
        // module's row — a hard error, not a cross-partition alias.
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();
        let log = kv.events();
        let ev = |owner: &str| {
            MemEvent::new(
                "osmem:collision",
                Scope::owner(owner),
                "note",
                serde_json::json!({}),
                Origin {
                    source: "test".into(),
                    trust: Trust::ToolOutput,
                },
            )
        };
        let org = org_of(&kv, "alice").await;
        log.append(&ev(&partition_key(&org, &SpaceId::module_private("sin90"))))
            .await
            .unwrap();
        let err = log
            .append(&ev(&partition_key(&org, &SpaceId::module_private("cos72"))))
            .await
            .expect_err("the same id under another owner must not be aliased");
        assert!(
            matches!(err, agent24_memory::MemoryError::Conflict(_)),
            "{err}"
        );
    }

    #[tokio::test]
    async fn recent_returns_the_newest_not_the_oldest() {
        // The test whose ABSENCE let the original implementation ship: it asked for
        // `LIMIT n` from a seq-ASC scan and reversed the result, which returns the
        // OLDEST n in reverse order. Every earlier test used a limit larger than the
        // row count, where the two behaviours are identical.
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();
        let m = handle(&kv, "alice", "sin90").await;
        for i in 0..20 {
            let mut b = serde_json::Map::new();
            b.insert("n".into(), i.into());
            m.remember(Remember::new("note", b)).await.unwrap();
        }
        let got: Vec<i64> = m
            .recent(3)
            .await
            .unwrap()
            .iter()
            .map(|r| r.body.get("n").and_then(|v| v.as_i64()).unwrap())
            .collect();
        assert_eq!(got, vec![19, 18, 17], "newest first — NOT the oldest three");
    }

    #[tokio::test]
    async fn a_module_cannot_write_an_unbounded_blob() {
        // Confidentiality is not the only thing that matters: modules share one
        // database, so an unbounded write is a way for A to degrade B without ever
        // reading a byte of B's data. Not a quota — a floor.
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();
        let m = handle(&kv, "alice", "sin90").await;

        let mut big = serde_json::Map::new();
        big.insert("blob".into(), "x".repeat(MAX_BODY_BYTES + 1).into());
        let err = m.remember(Remember::new("note", big)).await.unwrap_err();
        assert!(matches!(err, DomainError::Memory(_)), "{err}");

        let err = m
            .remember(Remember::new(
                "k".repeat(MAX_KIND_BYTES + 1),
                serde_json::Map::new(),
            ))
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::Memory(_)), "{err}");

        let err = m
            .remember(Remember::new("   ", serde_json::Map::new()))
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::Memory(_)), "{err}");

        // Nothing was written by any of the three.
        assert!(m.recent(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn recall_finds_a_match_older_than_one_page() {
        // The bounded working set must not become a silent truncation. `recall`
        // pages rather than loading the partition, and a match in the FIRST page of
        // a multi-page partition must still be found — the failure mode of a
        // "search only the most recent N" shortcut.
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();
        let m = handle(&kv, "alice", "sin90").await;

        let mut needle_body = serde_json::Map::new();
        needle_body.insert("text".into(), "the-needle".into());
        m.remember(Remember::new("note", needle_body))
            .await
            .unwrap();
        // Comfortably more than one RECALL_PAGE of noise on top of it.
        for _ in 0..(RECALL_PAGE + 50) {
            m.remember(Remember::new("noise", serde_json::Map::new()))
                .await
                .unwrap();
        }

        let hits = m.recall("the-needle", 10).await.unwrap();
        assert_eq!(
            hits.len(),
            1,
            "a match older than one page must still be found"
        );
        assert_eq!(hits[0].kind, "note");
    }

    #[tokio::test]
    async fn recall_returns_the_newest_matches_and_respects_its_limit() {
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();
        let m = handle(&kv, "alice", "sin90").await;
        for i in 0..10 {
            let mut b = serde_json::Map::new();
            b.insert("n".into(), i.into());
            m.remember(Remember::new("note", b)).await.unwrap();
        }
        let hits = m.recall("note", 3).await.unwrap();
        assert_eq!(hits.len(), 3, "the limit is respected");
        // Newest first: the last three written, in reverse order.
        let ns: Vec<i64> = hits
            .iter()
            .map(|r| r.body.get("n").and_then(|v| v.as_i64()).unwrap())
            .collect();
        assert_eq!(ns, vec![9, 8, 7], "newest matches, newest first");
    }

    #[tokio::test]
    async fn a_page_boundary_is_not_a_silent_truncation() {
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();
        let m = handle(&kv, "alice", "sin90").await;
        for _ in 0..(RECALL_PAGE + 20) {
            m.remember(Remember::new("note", serde_json::Map::new()))
                .await
                .unwrap();
        }
        let all = m.recent(usize::MAX).await.unwrap();
        assert_eq!(
            all.len(),
            RECALL_PAGE as usize + 20,
            "everything that exists comes back — the page size is a working-set \
             bound, NOT a silent truncation of the result"
        );
        // The same for the search path, whose page size is what RECALL_PAGE names.
        assert_eq!(m.recall("note", usize::MAX).await.unwrap().len(), all.len());
    }

    #[tokio::test]
    async fn max_results_is_the_stated_cap_and_it_is_the_newest_that_survive() {
        // The previous version of this test asserted `len() <= MAX_RESULTS` over a
        // 520-row partition, which is vacuously true — review was right that it
        // pinned nothing. The cap only means anything above it.
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();
        let m = handle(&kv, "alice", "sin90").await;
        for i in 0..(MAX_RESULTS + 5) {
            let mut b = serde_json::Map::new();
            b.insert("n".into(), i.into());
            m.remember(Remember::new("note", b)).await.unwrap();
        }
        let got = m.recent(usize::MAX).await.unwrap();
        assert_eq!(got.len(), MAX_RESULTS, "capped at the contract's ceiling");
        // And it is a NEWEST-first cap, not "the first 1000 we happened to read".
        assert_eq!(
            got[0].body.get("n").and_then(|v| v.as_i64()),
            Some(MAX_RESULTS as i64 + 4)
        );
        assert_eq!(
            m.recall("note", usize::MAX).await.unwrap().len(),
            MAX_RESULTS
        );
    }

    #[tokio::test]
    async fn a_limit_of_zero_reads_nothing_at_all() {
        // Not merely "returns nothing": a zero limit used to still walk the whole
        // partition to fill a ring it then threw away.
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();
        let m = handle(&kv, "alice", "sin90").await;
        for _ in 0..10 {
            m.remember(Remember::new("note", serde_json::Map::new()))
                .await
                .unwrap();
        }
        assert!(m.recent(0).await.unwrap().is_empty());
        assert!(m.recall("note", 0).await.unwrap().is_empty());
    }

    // There is deliberately NO test here for "a read terminates against a
    // concurrent writer", and the reason is worth more than the test was.
    //
    // Review's argument was that forward paging to a short page has no upper bound
    // on iterations: a writer keeps every page full, so the walk chases a moving
    // tail. The SHAPE of that argument is right, and it is why the reads page
    // backwards now — a decreasing cursor bounds the walk structurally.
    //
    // But the situation is not reachable at these page sizes, and the test written
    // to prove it did not: an unbounded writer, a matchless query forcing a full
    // walk, an asserted before/after overlap — and reverting to forward paging
    // still passed, in 0.18s. It has to: the writer must sustain RECALL_PAGE (500)
    // appends per page-read to keep a page full, while it actually manages one or
    // two per round trip. So the test asserted a property it could not observe,
    // which is the exact thing the last five review rounds on this repo kept
    // deleting.
    //
    // What backwards paging demonstrably buys is `recent`: one descending query
    // instead of a walk of the whole partition. That is pinned by
    // `recent_returns_the_newest_not_the_oldest` and
    // `max_results_is_the_stated_cap_and_it_is_the_newest_that_survive`. The
    // termination property is an argument about the loop, and it belongs in the
    // comment on `page` where it is, not in a test that cannot fail.

    #[test]
    fn the_partition_key_is_versioned_and_unambiguous() {
        let k = partition_key(
            &OrgId::from_store("org_1"),
            &SpaceId::module_private("sin90"),
        );
        assert!(k.starts_with("v2\u{0}"), "{k:?}");
        // The concat-collision shape this repo already paid for once (#122 B1):
        // two different (org, space) pairs must not produce one key. The v1 key
        // failed exactly here — `("a", "b\0os:c")` and `("a\0os:b", "c")` both
        // rendered as `v1\0a\0os:b\0os:c` — which is why the parts are
        // length-prefixed, and why widening the dimension did not get to drop it.
        assert_ne!(
            partition_key(&OrgId::from_store("a"), &SpaceId::raw("b\u{0}c")),
            partition_key(&OrgId::from_store("a\u{0}b"), &SpaceId::raw("c")),
        );
        // Same shape without any NUL in the inputs, so it does not rely on an
        // exotic id to be meaningful.
        assert_ne!(
            partition_key(&OrgId::from_store("ab"), &SpaceId::raw("c")),
            partition_key(&OrgId::from_store("a"), &SpaceId::raw("bc")),
        );
        // Disjoint from the agent loop's own keys, stated as what is actually
        // enforced rather than as a blanket claim (review's point: the old
        // assertion tried one literal, `"alice"`, and read as if it covered every
        // user id). Every partition key begins with `v2\0`; nothing validates
        // that a user id does not, so the property is "disjoint from any user id
        // that does not itself begin with `v2\0`". The daemon's only user id is
        // the constant `LOCAL_USER`, so today nothing can collide — but the
        // structural half is what a future multi-user id scheme must preserve,
        // and it is the half worth asserting.
        assert!(k.starts_with("v2\u{0}"));
        assert_ne!(k, "alice");

        // Two hand-picked counter-examples are not injectivity, which is what the
        // key actually has to have. Sweep a small cross product — including the
        // adversarial inputs (embedded NUL, the `os:` marker, a shared prefix) —
        // and assert the mapping is one-to-one.
        let orgs = ["", "a", "ab", "abc", "a\u{0}b", "os:a", "org_1"];
        let spaces = ["", "a", "ab", "abc", "b\u{0}os:c", "os:b", "os:sin90"];
        let mut seen = std::collections::HashMap::new();
        for o in orgs {
            for s in spaces {
                let key = partition_key(&OrgId::from_store(o), &SpaceId::raw(s));
                if let Some(prev) = seen.insert(key.clone(), (o, s)) {
                    panic!("collision: {prev:?} and {:?} both produce {key:?}", (o, s));
                }
            }
        }
        assert_eq!(seen.len(), orgs.len() * spaces.len());
    }

    #[test]
    fn a_modules_space_id_is_the_os_prefixed_module_name() {
        // HALF of a pin, and named as half. It fixes what the Rust constructor
        // produces; the other half — that 0013's SQL backfill produces the same
        // string — is asserted in `agent24-memory`'s
        // `migration_0013_gives_an_existing_0012_partition_an_org_and_a_space`,
        // which runs the real migration and compares its `space_id` against
        // `os:<module_name>`.
        //
        // Review was right that this test alone proved nothing about the SQL: it
        // was called `the_space_prefix_matches_migration_0013s_backfill` while
        // never reading the migration, so editing the SQL left it green. Together
        // the two assertions pin both ends, which matters because a drift gives a
        // migrated partition a space id the kernel never derives — its key is
        // then never recomputed and its history silently disappears.
        assert_eq!(SpaceId::module_private("sin90").as_str(), "os:sin90");
    }

    #[tokio::test]
    async fn a_v1_partition_is_rekeyed_onto_its_org_and_space() {
        // The catalog's FIRST real job. F1 built `mem_os_partitions` so that a
        // future key-version migration would have an explicit list instead of
        // prefix-matching NUL-bearing strings — and shipped without ever
        // exercising it. This is that exercise, run while the only rows in
        // existence are on machines that ran `main` since yesterday.
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();
        let org = org_of(&kv, "alice").await;
        let old_key = legacy_partition_key("alice", "sin90");

        // A partition exactly as F1 would have left it: v1 key, v1 rows.
        kv.record_os_partition(agent24_memory::OsPartitionIdentity {
            owner_key: &old_key,
            key_version: "v1",
            org_id: org.as_str(),
            space_id: "os:sin90",
            user: "alice",
            module: "sin90",
        })
        .await
        .unwrap();
        let log = kv.events();
        for i in 0..3 {
            log.append(&MemEvent::new(
                format!("osmem:legacy-{i}"),
                Scope::owner(&old_key),
                "note",
                serde_json::json!({"n": i}),
                Origin {
                    source: "os:sin90".into(),
                    trust: Trust::ToolOutput,
                },
            ))
            .await
            .unwrap();
        }

        assert_eq!(
            OsMemoryCatalog::migrate_legacy_partitions(&kv)
                .await
                .unwrap(),
            1
        );

        // The module mounts, derives its v2 key with no knowledge of any of
        // this, and finds its history where it left it. That is the whole claim.
        let sin90 = handle(&kv, "alice", "sin90").await;
        assert_eq!(
            sin90.recent(10).await.unwrap().len(),
            3,
            "a re-keyed partition must still be the module's own memory"
        );
        assert!(
            log.scan(&EventQuery::owner(&old_key))
                .await
                .unwrap()
                .is_empty(),
            "and nothing may be left behind under the old key"
        );
        // Idempotent: a second run finds no v1 rows and moves nothing.
        assert_eq!(
            OsMemoryCatalog::migrate_legacy_partitions(&kv)
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn a_refused_rekey_leaves_both_partitions_exactly_as_they_were() {
        // The failure mode that would make this migration worse than not
        // migrating: the events move onto the occupied key while the catalog
        // does not follow, silently pouring one partition's memories into
        // another's — a cross-partition leak produced by the code written to
        // keep partitions apart.
        //
        // It is the TRANSACTION that prevents that, not the up-front occupied
        // check. An earlier version of this test asserted only that an error
        // came back, and a mutation check showed it passed with the check
        // deleted — the catalog's primary key rejects the second row either way.
        // So what this asserts is the part that would actually break: after the
        // refusal, no event has moved.
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();
        let org = org_of(&kv, "alice").await;
        let space = SpaceId::module_private("sin90");
        let occupied = partition_key(&org, &space);
        let old_key = legacy_partition_key("alice", "sin90");

        // The v2 partition already exists (this daemon ran once), and a v1 row
        // for the same identity is still there (an earlier sweep failed).
        kv.record_os_partition(agent24_memory::OsPartitionIdentity {
            owner_key: &occupied,
            key_version: KEY_VERSION,
            org_id: org.as_str(),
            space_id: space.as_str(),
            user: "alice",
            module: "sin90",
        })
        .await
        .unwrap();

        // One memory on each side, so a merge would be visible as a count.
        let log = kv.events();
        for (id, owner) in [("osmem:stale", &old_key), ("osmem:live", &occupied)] {
            log.append(&MemEvent::new(
                id,
                Scope::owner(owner),
                "note",
                serde_json::json!({}),
                Origin {
                    source: "test".into(),
                    trust: Trust::ToolOutput,
                },
            ))
            .await
            .unwrap();
        }

        let err = kv
            .rekey_os_partition(&old_key, &occupied, KEY_VERSION)
            .await
            .expect_err("merging two partitions must never be automatic");
        assert!(
            matches!(err, agent24_memory::MemoryError::Conflict(_)),
            "{err}"
        );

        // THE assertion: the occupied partition still holds exactly its own
        // memory, and the stale one still holds exactly its own.
        assert_eq!(
            log.scan(&EventQuery::owner(&occupied)).await.unwrap().len(),
            1,
            "a refused re-key must not have poured the other partition in"
        );
        assert_eq!(
            log.scan(&EventQuery::owner(&old_key)).await.unwrap().len(),
            1,
            "and must not have half-moved the stale one either"
        );
    }

    #[tokio::test]
    async fn resolving_an_org_is_stable_and_the_id_is_opaque() {
        // Named for what it checks. The ambiguity path — a user in TWO orgs
        // erroring instead of picking one — needs a second membership row, which
        // no API here can create, so it is tested in `agent24-memory` where the
        // pool is reachable. A test that cannot construct the state it claims to
        // cover is the kind this repo has already deleted once.
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();
        let first = kv.ensure_org_for_user("alice").await.unwrap();
        assert_eq!(
            kv.ensure_org_for_user("alice").await.unwrap(),
            first,
            "resolving twice must be the same org, not a second one"
        );
        assert_ne!(
            kv.ensure_org_for_user("bob").await.unwrap(),
            first,
            "two users must not land in one org by accident"
        );
        // The org id is opaque: nothing may recover the user from it, which is
        // the property that makes it survivable when the org gains a second
        // member.
        assert!(!first.contains("alice"), "{first}");
    }

    #[tokio::test]
    async fn the_catalog_answers_what_a_prefix_match_should_not_have_to() {
        // The review's point: future export/erase code must not discover
        // partitions by LIKE-matching strings that contain NUL.
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();
        let mut cat = OsMemoryCatalog::default();
        record_and_mark(
            &mut cat,
            &org_of(&kv, "alice").await,
            "alice",
            &manifest("sin90"),
            &kv,
        )
        .await;
        record_and_mark(
            &mut cat,
            &org_of(&kv, "alice").await,
            "alice",
            &manifest("cos72"),
            &kv,
        )
        .await;
        record_and_mark(
            &mut cat,
            &org_of(&kv, "bob").await,
            "bob",
            &manifest("sin90"),
            &kv,
        )
        .await;

        let alice = OsMemoryCatalog::durable_for_org(&kv, &org_of(&kv, "alice").await)
            .await
            .unwrap();
        assert_eq!(alice.len(), 2);
        assert!(alice.iter().all(|r| r.logical_user == "alice"));
        assert!(alice.iter().all(|r| r.key_version == KEY_VERSION));
        assert_eq!(
            OsMemoryCatalog::durable_for_org(&kv, &org_of(&kv, "bob").await)
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(
            OsMemoryCatalog::durable_for_org(&kv, &OrgId::from_store("org_that_owns_nothing"))
                .await
                .unwrap()
                .is_empty()
        );

        // The catalog records the manifest name at MOUNT time, which is what makes
        // a later rename recoverable — the key alone cannot say.
        assert_eq!(cat.partitions()[0].module, "sin90");
    }

    #[tokio::test]
    async fn the_catalog_survives_a_restart_a_disable_and_a_rename() {
        // The three cases that make the durable table necessary. The first version
        // of this catalog was an in-memory Vec rebuilt from whichever modules
        // mounted and then dropped, so it answered NONE of them — while its doc
        // claimed it was what kept future data recoverable.
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();

        // Run 1: two modules mount and write.
        {
            let cat = OsMemoryCatalog::default();
            for name in ["sin90", "cos72"] {
                let p = cat
                    .ensure_recorded(&org_of(&kv, "alice").await, "alice", &manifest(name), &kv)
                    .await
                    .unwrap();
                OsScopedMemory::new(&p, &kv)
                    .remember(Remember::new("note", serde_json::Map::new()))
                    .await
                    .unwrap();
            }
        }
        // Run 2: cos72 has been disabled, and sin90 renamed to schedule — so the
        // fresh run's inventory knows about ONE partition while three exist.
        let mut run2 = OsMemoryCatalog::default();
        record_and_mark(
            &mut run2,
            &org_of(&kv, "alice").await,
            "alice",
            &manifest("schedule"),
            &kv,
        )
        .await;
        assert_eq!(run2.partitions().len(), 1);

        let rows = OsMemoryCatalog::durable_for_org(&kv, &org_of(&kv, "alice").await)
            .await
            .unwrap();
        let mut names: Vec<&str> = rows.iter().map(|r| r.module_name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(
            names,
            vec!["cos72", "schedule", "sin90"],
            "an export or erase path must see the partitions left behind by a \
             previous run, a disabled module and a rename — none of which are in \
             this run's mount inventory"
        );
        // sin90's data is still there under its old key, findable only through the
        // catalog. That is the migration debt design C accepted, now payable.
        let orphan = rows.iter().find(|r| r.module_name == "sin90").unwrap();
        let events = kv.events();
        let left = events
            .scan(&EventQuery::owner(&orphan.owner_key))
            .await
            .unwrap();
        assert_eq!(left.len(), 1, "the renamed module's memories still exist");
    }

    #[tokio::test]
    async fn ensure_recorded_is_idempotent_and_never_advances_last_seen_at() {
        // Restarts re-record every mounted partition, so `ensure_recorded` must
        // be idempotent. `first_seen_at` and `module_name` are write-once: a
        // rename must NOT rewrite the row that says what the key originally
        // meant. And — T8.5c-W-mount decision 5 (H1) — `ensure_recorded` runs
        // BEFORE the kernel knows whether the mount will succeed, so neither
        // the first call nor a repeat may advance `last_seen_at`: only
        // `mark_mounted` may, and only once the mount is confirmed.
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();
        let cat = OsMemoryCatalog::default();
        let p = cat
            .ensure_recorded(
                &org_of(&kv, "alice").await,
                "alice",
                &manifest("sin90"),
                &kv,
            )
            .await
            .unwrap();
        let first = OsMemoryCatalog::durable_for_org(&kv, &org_of(&kv, "alice").await)
            .await
            .unwrap();
        assert_eq!(
            first[0].last_seen_at, None,
            "a first-time ensure_recorded must not claim the partition was ever \
             seen active — the mount it precedes has not been confirmed yet"
        );

        cat.ensure_recorded(
            &org_of(&kv, "alice").await,
            "alice",
            &manifest("sin90"),
            &kv,
        )
        .await
        .unwrap();
        let again = OsMemoryCatalog::durable_for_org(&kv, &org_of(&kv, "alice").await)
            .await
            .unwrap();
        assert_eq!(again.len(), 1, "one row per partition, ever");
        assert_eq!(again[0].owner_key, p.key);
        assert_eq!(again[0].first_seen_at, first[0].first_seen_at);
        assert_eq!(again[0].module_name, "sin90");
        assert_eq!(
            again[0].last_seen_at, None,
            "a repeat ensure_recorded must not advance last_seen_at either — \
             that is the ON CONFLICT branch, and it must behave like the INSERT \
             branch on this column"
        );
    }

    struct FixedClock(u64);
    impl agent24_memory::Clock for FixedClock {
        fn now_epoch_secs(&self) -> u64 {
            self.0
        }
    }

    #[tokio::test]
    async fn mark_mounted_is_the_only_thing_that_advances_last_seen_at() {
        // Split from the test above (which pins that `ensure_recorded` never
        // advances `last_seen_at`, not even on a repeat call): this pins that
        // `mark_mounted` — and only `mark_mounted` — does, using an injected
        // clock rather than racing `SystemTime::now()`'s one-second resolution.
        // Two separate `OsMemoryCatalog`s stand in for two separate daemon
        // runs re-mounting the same module — `mark_mounted`'s own dedup
        // (M2, tested below) is a WITHIN-one-run guard, not a claim that a
        // later run's confirmed mount should leave last_seen_at alone.
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();
        let org = org_of(&kv, "alice").await;
        let p = OsMemoryCatalog::default()
            .ensure_recorded(&org, "alice", &manifest("sin90"), &kv)
            .await
            .unwrap();

        let mut run1 = OsMemoryCatalog::default();
        run1.mark_mounted(p.clone(), &kv, &FixedClock(1_700_000_000))
            .await;
        let after_first_mount = OsMemoryCatalog::durable_for_org(&kv, &org).await.unwrap();
        assert_eq!(
            after_first_mount[0].last_seen_at.as_deref(),
            Some(agent24_core::util::iso8601_from_epoch_secs(1_700_000_000)).as_deref(),
            "mark_mounted must write exactly the injected clock's value — the \
             None -> timestamp transition this decision exists to make real"
        );

        let mut run2 = OsMemoryCatalog::default();
        run2.mark_mounted(p, &kv, &FixedClock(1_700_000_100)).await;
        let after_second_mount = OsMemoryCatalog::durable_for_org(&kv, &org).await.unwrap();
        assert_eq!(
            after_second_mount[0].last_seen_at.as_deref(),
            Some(agent24_core::util::iso8601_from_epoch_secs(1_700_000_100)).as_deref(),
            "a later run's confirmed mount must advance last_seen_at again"
        );
        assert_eq!(
            after_second_mount[0].first_seen_at, after_first_mount[0].first_seen_at,
            "and first_seen_at must not move with it"
        );
    }

    #[tokio::test]
    async fn ensure_recorded_after_mark_mounted_does_not_reset_last_seen_at() {
        // Regression companion to the two tests above: those prove `ensure_recorded`
        // never advances `last_seen_at` on its own, and that `mark_mounted` is what
        // gives it its first real value. Neither pins what happens to an ALREADY
        // touched row when a later restart re-`ensure_recorded`s the same partition
        // (e.g. the next daemon start re-recording every mounted module before it
        // knows which ones will actually come up) — `ensure_recorded`'s ON CONFLICT
        // branch must leave a real timestamp exactly as `mark_mounted` left it, not
        // reset it back toward NULL and not advance it itself.
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();
        let org = org_of(&kv, "alice").await;
        let mut cat = OsMemoryCatalog::default();
        let p = cat
            .ensure_recorded(&org, "alice", &manifest("sin90"), &kv)
            .await
            .unwrap();
        cat.mark_mounted(p, &kv, &FixedClock(1_700_000_000)).await;
        let touched = OsMemoryCatalog::durable_for_org(&kv, &org).await.unwrap();

        cat.ensure_recorded(&org, "alice", &manifest("sin90"), &kv)
            .await
            .unwrap();
        let after_repeat = OsMemoryCatalog::durable_for_org(&kv, &org).await.unwrap();

        assert_eq!(
            after_repeat[0].last_seen_at, touched[0].last_seen_at,
            "a later ensure_recorded for a partition that has already been \
             mark_mounted must leave last_seen_at exactly as mark_mounted left \
             it — ensure_recorded is not allowed to reset it back toward NULL, \
             or to advance it again itself"
        );
        assert_eq!(
            after_repeat[0].first_seen_at, touched[0].first_seen_at,
            "and first_seen_at must not move with any of this"
        );
    }

    #[tokio::test]
    async fn mark_mounted_dedupes_by_partition_key_within_one_run() {
        // M2: `mark_mounted` claimed to be idempotent while its first
        // implementation was an unconditional `Vec::push`. Pinned two ways:
        // the SAME partition twice must not grow `partitions()`, and a
        // DIFFERENT partition in between must still be added normally — so
        // this is deduping by key, not "this method can only ever be called
        // once".
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();
        let mut cat = OsMemoryCatalog::default();
        let sin90 = cat
            .ensure_recorded(
                &org_of(&kv, "alice").await,
                "alice",
                &manifest("sin90"),
                &kv,
            )
            .await
            .unwrap();
        cat.mark_mounted(sin90.clone(), &kv, &FixedClock(1)).await;
        assert_eq!(cat.partitions().len(), 1);
        cat.mark_mounted(sin90, &kv, &FixedClock(2)).await;
        assert_eq!(
            cat.partitions().len(),
            1,
            "a repeat mark_mounted for the SAME partition must not grow the inventory"
        );

        let cos72 = cat
            .ensure_recorded(
                &org_of(&kv, "alice").await,
                "alice",
                &manifest("cos72"),
                &kv,
            )
            .await
            .unwrap();
        cat.mark_mounted(cos72, &kv, &FixedClock(3)).await;
        assert_eq!(
            cat.partitions().len(),
            2,
            "a DIFFERENT partition must still be added — dedup is by key, not a \
             one-call-ever limit"
        );
    }

    #[tokio::test]
    async fn a_partition_recorded_with_a_different_identity_is_a_conflict() {
        // The test the previous one could not be: re-recording the SAME metadata
        // proves nothing about what happens when the stored identity disagrees.
        // The first upsert took every conflict as success and updated only
        // `last_seen_at`, so a drifted row returned `Ok`, the handle was lent, and
        // the catalog went on attributing new data to the old identity.
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();
        let org = org_of(&kv, "alice").await;
        let space = SpaceId::module_private("sin90");
        let key = partition_key(&org, &space);
        let recorded = agent24_memory::OsPartitionIdentity {
            owner_key: &key,
            key_version: KEY_VERSION,
            org_id: org.as_str(),
            space_id: space.as_str(),
            user: "alice",
            module: "sin90",
        };
        kv.record_os_partition(recorded).await.unwrap();

        // Every column that IS the identity, drifted one at a time. `org_id` and
        // `space_id` are the two F8 adds, and they matter most: they are what the
        // key encodes, so a row claiming a different one means the encoder and the
        // catalog have diverged and the handle must not be lent.
        // Carol's, not Bob's: Bob is about to be added to ALICE's org below, and
        // `add_org_member` now refuses a user who already has one of their own —
        // which is the whole point of that refusal, and which giving Bob an org
        // here would walk straight into.
        let other_org = org_of(&kv, "carol").await;
        for drifted in [
            agent24_memory::OsPartitionIdentity {
                key_version: "v3-from-a-newer-kernel",
                ..recorded
            },
            agent24_memory::OsPartitionIdentity {
                org_id: other_org.as_str(),
                // Carol, not Alice — and the `user` field is what makes this arm
                // test the thing it names.
                //
                // Review caught that with `user` left as "alice", this case never
                // reached the org_id guard at all: `record_os_partition` checks
                // membership FIRST, Alice is not in Carol's org, and the conflict
                // came back from there. The assertion below was satisfied by a
                // different mechanism, which left `AND org_id = excluded.org_id`
                // as the ONLY one of the five identity guards with zero coverage
                // — delete that line and the whole workspace stayed green.
                //
                // Carol is a legitimate member of her own org, so membership
                // passes and the guard is what refuses her.
                user: "carol",
                ..recorded
            },
            agent24_memory::OsPartitionIdentity {
                space_id: "os:cos72",
                ..recorded
            },
            agent24_memory::OsPartitionIdentity {
                module: "cos72",
                ..recorded
            },
        ] {
            let err = kv
                .record_os_partition(drifted)
                .await
                .expect_err("a disagreeing identity must not be accepted");
            assert!(
                matches!(err, agent24_memory::MemoryError::Conflict(_)),
                "{err}"
            );
        }

        // `user` is NOT in that list, and its absence is the point. A partition
        // belongs to an (org, space), so every member of the org derives this
        // same key; demanding that the mounting user match the creator would
        // refuse the second member forever — see
        // `a_second_member_of_an_org_mounts_the_same_partition` in
        // `agent24-memory`. This test had `user: "bob"` in the loop until review
        // showed what that was really asserting.
        //
        // Bob has to be MADE a member first. The earlier version of this did not,
        // and passed — which review caught as the second half of the same
        // mistake: dropping the guard had also let the storage API record a
        // creator from outside the org entirely.
        kv.add_org_member(org.as_str(), "bob").await.unwrap();
        kv.record_os_partition(agent24_memory::OsPartitionIdentity {
            user: "bob",
            ..recorded
        })
        .await
        .expect("a different member of the same org must be able to mount it");

        // The original row is untouched by any of it — including the creator.
        let rows = OsMemoryCatalog::durable_for_org(&kv, &org_of(&kv, "alice").await)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].module_name, "sin90");
        assert_eq!(rows[0].key_version, KEY_VERSION);
        assert_eq!(rows[0].logical_user, "alice");
    }

    // ==== T8.5c-P: `recall_page`/`recent_page`/`remember_checked` ====
    // (design doc `docs/design/T8.5c-P-pagination-cursor.md`, §9's judgement
    // list) — integration-level, against a real `OsScopedMemory` backed by a
    // real SQLite pool, a real `Semaphore` and a real `RateLimiter`. The pure
    // state-machine judgements (1, 2, 2b, 5, 5b, 7c, 7d, 9c) live in
    // `os_memory_page.rs`'s own test module, next to `page_from_stream`.

    fn generous_limiter() -> Arc<RateLimiter> {
        Arc::new(RateLimiter::new(1e12, 1e12))
    }

    fn admission(permits: usize) -> Arc<Semaphore> {
        Arc::new(Semaphore::new(permits))
    }

    async fn seed(m: &OsScopedMemory, n: usize, kind: &str) {
        for i in 0..n {
            let mut b = serde_json::Map::new();
            b.insert("n".into(), i.into());
            m.remember(Remember::new(kind, b)).await.unwrap();
        }
    }

    /// Codex review round 2 (Low): a fixed `sleep` before checking
    /// `JoinHandle::is_finished()` is not a reliable way to prove a spawned
    /// call really registered as a blocked waiter (on the admission
    /// semaphore, or — for a write competing with an external SQLite lock —
    /// on the database) rather than merely "has not been scheduled onto a
    /// worker thread yet". This wraps a future so the FIRST time polling it
    /// returns `Poll::Pending`, it fires a oneshot — deterministic proof the
    /// call actually blocked, not a timing guess. Everything before that
    /// first real block in `remember_checked`/`recall_page`/`recent_page`
    /// (building the query, `Reservation::reserve`) is synchronous, so the
    /// first `Pending` genuinely corresponds to "waiting on the admission
    /// permit" or "waiting on the database", not some unrelated earlier
    /// yield point.
    struct NotifyFirstPending<F> {
        inner: F,
        notify: Option<tokio::sync::oneshot::Sender<()>>,
    }

    impl<F: std::future::Future + Unpin> std::future::Future for NotifyFirstPending<F> {
        type Output = F::Output;
        fn poll(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Self::Output> {
            match std::pin::Pin::new(&mut self.inner).poll(cx) {
                std::task::Poll::Pending => {
                    if let Some(tx) = self.notify.take() {
                        let _ = tx.send(());
                    }
                    std::task::Poll::Pending
                }
                ready => ready,
            }
        }
    }

    /// Spawns `fut` (boxed, so it is `Unpin` regardless of what it captures)
    /// wrapped in [`NotifyFirstPending`], and returns once that first real
    /// block has actually been observed — the caller's next assertion (e.g.
    /// `available_permits() == 0`, or `!task.is_finished()`) is then
    /// checking a fact that has already happened, not racing a `sleep`
    /// against the scheduler.
    async fn spawn_and_confirm_blocked<T: Send + 'static>(
        fut: std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send>>,
    ) -> tokio::task::JoinHandle<T> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(NotifyFirstPending {
            inner: fut,
            notify: Some(tx),
        });
        rx.await
            .expect("the call must have blocked at least once before finishing this fast");
        task
    }

    // ---- judgement 3a/3b/3c/6 ----

    #[tokio::test]
    async fn judgement_3a_a_full_short_page_yields_a_cursor_that_then_observes_none() {
        // Partition has EXACTLY page_size matching rows, nothing older.
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();
        let m = handle(&kv, "alice", "sin90").await;
        seed(&m, 5, "note").await;
        let needle = Needle::normalize("");
        let page = m
            .recall_page(None, generous_limiter(), admission(4), &needle, 5, None)
            .await
            .unwrap();
        assert_eq!(page.items.len(), 5);
        let cursor = page
            .cursor
            .expect("the state machine does not pull one more row to check");
        let next = m
            .recall_page(
                None,
                generous_limiter(),
                admission(4),
                &needle,
                5,
                Some(&cursor),
            )
            .await
            .unwrap();
        assert!(next.items.is_empty());
        assert!(
            next.cursor.is_none(),
            "the next call must honestly observe the end"
        );
    }

    #[tokio::test]
    async fn judgement_3b_a_full_page_with_more_data_behind_it_can_be_paged_further() {
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();
        let m = handle(&kv, "alice", "sin90").await;
        seed(&m, 8, "note").await;
        let needle = Needle::normalize("");
        let page1 = m
            .recall_page(None, generous_limiter(), admission(4), &needle, 5, None)
            .await
            .unwrap();
        assert_eq!(page1.items.len(), 5);
        let cursor = page1.cursor.expect("more data remains");
        let page2 = m
            .recall_page(
                None,
                generous_limiter(),
                admission(4),
                &needle,
                5,
                Some(&cursor),
            )
            .await
            .unwrap();
        assert_eq!(page2.items.len(), 3, "the 3 older rows, and only those");
        assert!(page2.cursor.is_none());
    }

    #[tokio::test]
    async fn judgement_3c_6_a_scan_budget_exhausted_by_non_matches_returns_empty_with_a_cursor() {
        // Every row present fails to match; the partition has MORE rows behind
        // the scan-budget boundary that DO match — proving the scan stopped at
        // MEMORY_SCAN_ROW_BUDGET, not at the partition's real end, and that the
        // cursor still lets a caller reach the match on the next call.
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();
        let m = handle(&kv, "alice", "sin90").await;
        let mut needle_body = serde_json::Map::new();
        needle_body.insert("text".into(), "the-match".into());
        m.remember(Remember::new("note", needle_body))
            .await
            .unwrap();
        seed(&m, MEMORY_SCAN_ROW_BUDGET, "noise").await;

        let needle = Needle::normalize("the-match");
        let page = m
            .recall_page(None, generous_limiter(), admission(4), &needle, 10, None)
            .await
            .unwrap();
        assert!(
            page.items.is_empty(),
            "the match is older than the scan budget"
        );
        let cursor = page.cursor.expect("must be non-empty: more to scan");

        let page2 = m
            .recall_page(
                None,
                generous_limiter(),
                admission(4),
                &needle,
                10,
                Some(&cursor),
            )
            .await
            .unwrap();
        assert_eq!(page2.items.len(), 1, "the match is found on the next call");
        assert_eq!(
            page2.items[0].body.get("text").and_then(|v| v.as_str()),
            Some("the-match")
        );
    }

    // ---- judgement 4 ----

    #[tokio::test]
    async fn judgement_4_page_size_zero_is_invalid_params_and_touches_nothing() {
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();
        let m = handle(&kv, "alice", "sin90").await;
        seed(&m, 3, "note").await;
        let needle = Needle::normalize("");
        let err = m
            .recall_page(None, generous_limiter(), admission(4), &needle, 0, None)
            .await
            .unwrap_err();
        assert!(matches!(err, MemoryRpcError::Invalid(_)));
        let err = m
            .recent_page(None, generous_limiter(), admission(4), 0, None)
            .await
            .unwrap_err();
        assert!(matches!(err, MemoryRpcError::Invalid(_)));
        // Over the cap is refused the same way.
        let err = m
            .recall_page(
                None,
                generous_limiter(),
                admission(4),
                &needle,
                MEMORY_MAX_PAGE_SIZE + 1,
                None,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, MemoryRpcError::Invalid(_)));
    }

    // ---- judgement 7 / 7b: weighted rate limiting ----

    #[tokio::test]
    async fn judgement_7_recall_and_remember_spend_different_amounts() {
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();
        let m = handle(&kv, "alice", "sin90").await;
        seed(&m, 5, "note").await;
        let limiter = Arc::new(RateLimiter::new(10_000.0, 0.0));
        let needle = Needle::normalize("");
        // One recall (full-budget reservation) vs. many remembers: the
        // reservation alone (2000) already dwarfs a single `remember` (1) by
        // three orders of magnitude — this is the "one token per call" model
        // T8.5c v1 D2 shipped, now replaced by a weighted one.
        m.recall_page(None, limiter.clone(), admission(4), &needle, 1, None)
            .await
            .unwrap();
        let mut remembers_possible = 0;
        for _ in 0..5000 {
            if m.remember_checked(
                None,
                limiter.clone(),
                admission(4),
                Remember::new("note", serde_json::Map::new()),
            )
            .await
            .is_ok()
            {
                remembers_possible += 1;
            } else {
                break;
            }
        }
        assert!(
            remembers_possible > 1000,
            "remember must cost far less than a recall's worst-case reservation: {remembers_possible}"
        );
    }

    #[tokio::test]
    async fn judgement_7b_a_cheap_first_row_hit_settles_for_far_less_than_the_worst_case() {
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();
        let m = handle(&kv, "alice", "sin90").await;
        let mut needle_body = serde_json::Map::new();
        needle_body.insert("text".into(), "hit".into());
        m.remember(Remember::new("note", needle_body))
            .await
            .unwrap();
        let limiter = Arc::new(RateLimiter::new(MEMORY_SCAN_ROW_BUDGET as f64 + 50.0, 0.0));
        let needle = Needle::normalize("hit");
        m.recall_page(None, limiter.clone(), admission(4), &needle, 1, None)
            .await
            .unwrap();
        // If the old "flat MEMORY_COST_RECALL, never refunded" model were
        // still in effect, the bucket would now have (roughly) nothing left.
        // Under the reservation/refund model, only ~1 + ROW_BUFFER_MARGIN was
        // actually kept — comfortably more than that must remain.
        assert!(
            limiter.try_acquire_weighted(crate::events_emit::ScanCost::from_rows_const(
                MEMORY_SCAN_ROW_BUDGET - 100
            )),
            "a first-row-hit recall must settle for far less than the full reservation"
        );

        // Positive control: a query that scans the full budget without a
        // match settles near the full amount (T8.5c v1's judgement 6, not
        // weakened by the reservation/refund model).
        let kv2 = agent24_memory::KvStore::open_memory().await.unwrap();
        let m2 = handle(&kv2, "alice", "sin90").await;
        seed(&m2, MEMORY_SCAN_ROW_BUDGET, "noise").await;
        let limiter2 = Arc::new(RateLimiter::new(MEMORY_SCAN_ROW_BUDGET as f64, 0.0));
        let miss_needle = Needle::normalize("never-appears");
        m2.recall_page(None, limiter2.clone(), admission(4), &miss_needle, 10, None)
            .await
            .unwrap();
        assert!(
            !limiter2.try_acquire_weighted(crate::events_emit::ScanCost::from_rows_const(50)),
            "a full-budget miss must settle near the full reservation, not be refunded like a cheap hit"
        );
    }

    // ---- method-layer admission contract (a fast, deterministic
    // complement to judgement 9b below, NOT a substitute for it — Codex
    // review round 1 on this diff: a hand-built `Arc<Semaphore>` fed
    // directly into all three methods proves they all honour WHATEVER
    // semaphore they are given and that permits/waiters are not leaked
    // across methods or modules, but it cannot prove W will actually wire
    // ONE daemon-level singleton with `max_connections - 1` headroom, or
    // that four calls really hold four physical SQLite pool connections
    // (`open_memory()` here is a single-connection pool; this test never
    // lets any call reach a real contended connection). See
    // `judgement_9b_real_sqlite_connections_prove_cross_module_sharing_and_process_internal_headroom`
    // below for the judgement that exercises the real resource, per design
    // §6.5 M4.) ----

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn method_layer_admission_contract_recall_recent_remember_share_one_semaphore() {
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();
        // Two independently mounted modules, both handed the SAME
        // `Arc<Semaphore>` by this test. This proves the three methods all
        // honour whatever admission semaphore they are given (permits are
        // not leaked, waiters are not stranded) regardless of which module
        // or method holds/awaits them — it does NOT prove W actually wires
        // one daemon-level singleton (Codex review round 2 on this diff:
        // that claim needs the real construction path, which does not exist
        // in this codebase yet — see the real-resource test below for what
        // this one still cannot show). `Arc`-wrapped so a genuine
        // `tokio::spawn`ed task (a real, independently-scheduled competitor —
        // not a future this test polls once via `select!` and then abandons,
        // which tokio's FAIR semaphore would leave permanently queued behind)
        // can hold one.
        let holder = Arc::new(handle(&kv, "alice", "sin90").await);
        let contender = Arc::new(handle(&kv, "alice", "cos72").await);
        seed(&holder, 1, "note").await;
        seed(&contender, 1, "note").await;

        let shared_admission = admission(2); // small on purpose: easy to saturate
        let limiter = generous_limiter();

        // ---- (1) recall_page queues on an exhausted shared permit, and
        // proceeds once one frees up. ----
        let p1 = shared_admission.clone().acquire_owned().await.unwrap();
        let p2 = shared_admission.clone().acquire_owned().await.unwrap();
        assert_eq!(shared_admission.available_permits(), 0);

        let recall_task = {
            let holder = holder.clone();
            let admission = shared_admission.clone();
            let limiter = limiter.clone();
            spawn_and_confirm_blocked(Box::pin(async move {
                let needle = Needle::normalize("");
                holder
                    .recall_page(None, limiter, admission, &needle, 1, None)
                    .await
            }))
            .await
        };
        assert!(
            !recall_task.is_finished(),
            "recall_page must queue while both permits are held"
        );
        drop(p1);
        let page = tokio::time::timeout(std::time::Duration::from_secs(5), recall_task)
            .await
            .expect("must eventually complete once a permit frees up")
            .unwrap()
            .unwrap();
        assert_eq!(page.items.len(), 1);
        // `recall_page`'s own permit is released when it finishes — one of
        // the two original permits (`p2`) is still held, so exactly one slot
        // is free again.
        assert_eq!(shared_admission.available_permits(), 1);

        // ---- (2) a DIFFERENT module's `remember_checked` competes for, and
        // can take, the SAME permit `p2` releases — proving the semaphore
        // itself has no notion of which `OsScopedMemory` is waiting (the
        // cross-module half of M4), given that it IS shared. ----
        let p1b = shared_admission.clone().acquire_owned().await.unwrap();
        assert_eq!(shared_admission.available_permits(), 0);
        let remember_task = {
            let contender = contender.clone();
            let admission = shared_admission.clone();
            let limiter = limiter.clone();
            spawn_and_confirm_blocked(Box::pin(async move {
                contender
                    .remember_checked(
                        None,
                        limiter,
                        admission,
                        Remember::new("note", serde_json::Map::new()),
                    )
                    .await
            }))
            .await
        };
        assert!(
            !remember_task.is_finished(),
            "remember_checked must queue behind the same exhausted permit"
        );
        drop(p2);
        let remembered = tokio::time::timeout(std::time::Duration::from_secs(5), remember_task)
            .await
            .expect("must eventually complete once a permit frees up")
            .unwrap();
        assert!(
            remembered.is_ok(),
            "a released permit must be usable by a different module: {remembered:?}"
        );
        drop(p1b);

        // ---- (3) `recent_page` shares the same pool too — M4's "not just
        // remember_checked" requirement, one more independent
        // saturate/queue/release cycle. ----
        let p3 = shared_admission.clone().acquire_owned().await.unwrap();
        let p4 = shared_admission.clone().acquire_owned().await.unwrap();
        assert_eq!(shared_admission.available_permits(), 0);
        let recent_task = {
            let contender = contender.clone();
            let admission = shared_admission.clone();
            let limiter = limiter.clone();
            spawn_and_confirm_blocked(Box::pin(async move {
                contender
                    .recent_page(None, limiter, admission, 1, None)
                    .await
            }))
            .await
        };
        assert!(
            !recent_task.is_finished(),
            "recent_page must also queue on the shared permit"
        );
        drop(p3);
        let recent_page = tokio::time::timeout(std::time::Duration::from_secs(5), recent_task)
            .await
            .expect("recent_page must proceed once the permit frees up")
            .unwrap()
            .unwrap();
        assert_eq!(recent_page.items.len(), 1);
        drop(p4);
    }

    // ---- judgement 9b proper (design §6.5/§9, M4): real SQLite pool
    // connections, a real external writer holding a real lock, and the
    // permit sized to the pool's actual `max_connections - 1` headroom —
    // the model Codex review round 1 on this diff asked for in place of
    // (or alongside) the method-layer contract test above.
    //
    // What this test does NOT prove (Codex review round 2, Low): that
    // T8.5c-W's eventual mount-time wiring creates exactly one daemon-level
    // `Semaphore` singleton, or that a real `Handler::call` → `CallFuture` →
    // `bind_to_lifecycle` chain satisfies the same constraints — this test
    // still hands a test-constructed `Arc<Semaphore>` to both modules by
    // hand. What it DOES prove, which the method-layer contract test above
    // cannot: that when the three methods share one admission permit sized
    // to a real pool's actual headroom, real concurrent writers really do
    // occupy real pool connections, a real in-process read really is not
    // starved by them, and all three OOP methods (including a queued
    // `remember_checked`, not just the two reads) really do queue on that
    // one permit rather than finding a spare pool connection to race for.
    // ----

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn judgement_9b_real_sqlite_connections_prove_cross_module_sharing_and_process_internal_headroom()
     {
        use sqlx::Connection as _;
        use std::str::FromStr as _;

        // A real, FILE-backed pool (`KvStore::open`, not `open_memory`) —
        // `max_connections(5)`, WAL mode — so "4 permits held" and "4 real
        // pool connections held" are the same fact, not two independent
        // claims that happen to agree in this test.
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("m.db");
        let kv = agent24_memory::KvStore::open(&db_path).await.unwrap();
        // Two independently mounted modules under the same user, both
        // handed the SAME `Arc<Semaphore>` by this test — the cross-module
        // sharing half of M4 (see the note above on what this does and does
        // not prove about a future daemon-level singleton).
        let holder = Arc::new(handle(&kv, "alice", "sin90").await);
        let contender = Arc::new(handle(&kv, "alice", "cos72").await);

        // An independent connection OUTSIDE the shared pool and OUTSIDE the
        // admission permit entirely — a pure lock-contention source, not
        // the thing under test — holding a real SQLite write lock on the
        // SAME database file via `BEGIN IMMEDIATE`.
        let mut lock_conn = sqlx::sqlite::SqliteConnection::connect_with(
            &sqlx::sqlite::SqliteConnectOptions::from_str(&format!(
                "sqlite://{}",
                db_path.display()
            ))
            .unwrap(),
        )
        .await
        .unwrap();
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut lock_conn)
            .await
            .unwrap();

        // `max_connections(5) - 1 = 4`: the exact headroom design §6.5's
        // MUST contract #2 requires.
        let shared_admission = admission(4);
        let limiter = generous_limiter();

        // 4 real `remember_checked` calls, split across BOTH modules — each
        // acquires a real admission permit AND a real pool connection, then
        // genuinely blocks trying to `INSERT` against the external write
        // lock (`busy_timeout`), holding both for real — not a stand-in.
        // `spawn_and_confirm_blocked` (Codex round 2, Low) waits for each
        // task's first real `Poll::Pending` — proof it actually blocked,
        // not a `sleep` guessing it probably did by now.
        let mut write_tasks = Vec::new();
        for i in 0..4u32 {
            let m = if i % 2 == 0 {
                holder.clone()
            } else {
                contender.clone()
            };
            let admission = shared_admission.clone();
            let limiter = limiter.clone();
            write_tasks.push(
                spawn_and_confirm_blocked(Box::pin(async move {
                    m.remember_checked(
                        None,
                        limiter,
                        admission,
                        Remember::new("note", serde_json::Map::new()),
                    )
                    .await
                }))
                .await,
            );
        }
        assert_eq!(
            shared_admission.available_permits(),
            0,
            "all 4 permits must be held by real writers"
        );
        for t in &write_tasks {
            assert!(
                !t.is_finished(),
                "each write must still be blocked on the external write lock"
            );
        }

        // Headroom (design §6.5 MUST contract #2): a 5th, IN-PROCESS read
        // — `ScopedMemory::recent`, which T8.5c v1 decision D4 already
        // established does not take this permit at all — must still
        // succeed on the pool's 5th connection while the other 4 sit busy.
        // WAL mode is what makes this a read against a committed snapshot
        // rather than a wait on the pending writer. Bounded by an explicit
        // timeout (Codex round 2, Low) so a regression that DOES starve it
        // fails fast with a clear message instead of hanging the suite.
        let headroom_read =
            tokio::time::timeout(std::time::Duration::from_secs(5), holder.recent(10))
                .await
                .expect("the in-process path must not be starved by 4 busy OOP writers");
        assert!(headroom_read.is_ok(), "{headroom_read:?}");

        // A 6th, 7th and 8th call — through `remember_checked`, `recall_page`
        // AND `recent_page` respectively (M4: "not just remember_checked" —
        // Codex round 2 explicitly asked for the queued `remember_checked`
        // case too, not just the two reads) — must queue on the exhausted
        // admission permit. The pool itself still has an idle connection at
        // this point (the headroom read above returned it) — the fact that
        // matters is that these three are blocked by the PERMIT, not by a
        // lack of pool connections, which is exactly what M4 asks this
        // judgement to distinguish.
        let remember_probe = {
            let holder = holder.clone();
            let admission = shared_admission.clone();
            let limiter = limiter.clone();
            spawn_and_confirm_blocked(Box::pin(async move {
                holder
                    .remember_checked(
                        None,
                        limiter,
                        admission,
                        Remember::new("note", serde_json::Map::new()),
                    )
                    .await
            }))
            .await
        };
        let recall_probe = {
            let holder = holder.clone();
            let admission = shared_admission.clone();
            let limiter = limiter.clone();
            spawn_and_confirm_blocked(Box::pin(async move {
                let needle = Needle::normalize("");
                holder
                    .recall_page(None, limiter, admission, &needle, 1, None)
                    .await
            }))
            .await
        };
        let recent_probe = {
            let contender = contender.clone();
            let admission = shared_admission.clone();
            let limiter = limiter.clone();
            spawn_and_confirm_blocked(Box::pin(async move {
                contender
                    .recent_page(None, limiter, admission, 1, None)
                    .await
            }))
            .await
        };
        assert!(
            !remember_probe.is_finished(),
            "a 5th remember_checked must queue on the same exhausted permit"
        );
        assert!(
            !recall_probe.is_finished(),
            "recall_page must queue on the same exhausted permit"
        );
        assert!(
            !recent_probe.is_finished(),
            "recent_page must queue on the same exhausted permit"
        );

        // Release the external write lock — the 4 real writes unblock and
        // commit, freeing their permits AND their pool connections, which
        // is what finally lets the three queued OOP calls proceed.
        sqlx::query("COMMIT").execute(&mut lock_conn).await.unwrap();
        drop(lock_conn);

        for t in write_tasks {
            let outcome = tokio::time::timeout(std::time::Duration::from_secs(5), t)
                .await
                .expect("each write must complete once the external lock releases")
                .unwrap();
            assert!(outcome.is_ok(), "{outcome:?}");
        }
        // The three queued probes were spawned WHILE all 4 permits were
        // held, and SQLite serializes the 4 real writes one at a time as
        // the external lock releases — so a queued probe can be admitted
        // (and run its query) as soon as just ONE of the 4 permits frees,
        // not necessarily after all 4 writes have landed. That race is
        // real and does not need suppressing: what judgement 9b actually
        // asks this test to prove is that all three calls DO complete once
        // permits become available (not stuck forever on a permit no
        // release path reaches) — not a specific row count at an
        // unspecified point in that interleaving, which `page_size=1`
        // already bounds to at most one row either way.
        let remembered = tokio::time::timeout(std::time::Duration::from_secs(5), remember_probe)
            .await
            .expect("the 5th remember_checked must proceed once a permit frees up")
            .unwrap();
        assert!(remembered.is_ok(), "{remembered:?}");
        let recall_page = tokio::time::timeout(std::time::Duration::from_secs(5), recall_probe)
            .await
            .expect("recall_page must proceed once a permit frees up")
            .unwrap()
            .unwrap();
        assert!(recall_page.items.len() <= 1);
        let recent_page = tokio::time::timeout(std::time::Duration::from_secs(5), recent_probe)
            .await
            .expect("recent_page must proceed once a permit frees up")
            .unwrap()
            .unwrap();
        assert!(recent_page.items.len() <= 1);
    }

    // ---- judgement 9b, cancellation while queued on the admission permit ----

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_call_queued_on_the_admission_permit_can_be_cancelled_cleanly() {
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();
        let m = handle(&kv, "alice", "sin90").await;
        seed(&m, 1, "note").await;
        let shared_admission = admission(1);
        let _held = shared_admission.clone().acquire_owned().await.unwrap();
        let limiter = generous_limiter();

        let generation =
            agent24_os_proto::drain::Generation::serving_at("/tmp/does-not-need-to-exist".into());
        assert!(generation.ready());
        let in_flight = generation
            .admit_request(
                "queued-cancel".to_owned(),
                [0u8; 32],
                std::time::Instant::now(),
                std::time::Duration::from_secs(3600),
            )
            .unwrap();
        let lifecycle = generation.request_lifecycle("queued-cancel").unwrap();

        let needle = Needle::normalize("");
        let call = m.recall_page(
            Some(lifecycle),
            limiter,
            shared_admission.clone(),
            &needle,
            1,
            None,
        );
        let mut call = Box::pin(call);
        tokio::select! {
            _ = &mut call => panic!("must be queued, not completed, while the permit is held"),
            () = tokio::time::sleep(std::time::Duration::from_millis(30)) => {}
        }
        // Cancel while still queued (never touched the DB) — this must
        // produce no response, and must not leave the permit count
        // corrupted.
        let _ = in_flight.finish();
        let outcome = call.await;
        assert!(
            outcome.is_err(),
            "a cancelled queued call must not produce a page"
        );
        drop(_held);
        // The permit must still be exactly usable once — proving the
        // cancelled call's (never-acquired) slot was not double-counted.
        let _p = shared_admission.clone().acquire_owned().await.unwrap();
        assert_eq!(shared_admission.available_permits(), 0);
    }
}
