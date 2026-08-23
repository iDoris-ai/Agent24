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
use agent24_domain::{DomainError, DomainOsManifest};
use agent24_memory::event::{EventQuery, EventStore, MemEvent, Origin, Scope, Trust};

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
const MAX_KIND_BYTES: usize = 128;

/// The largest serialized body a module may remember in one call.
///
/// Well under the 1 MiB HTTP body cap, because this is one memory rather than
/// one request.
const MAX_BODY_BYTES: usize = 64 * 1024;

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
/// Those four are the entire reason a catalog was required. So [`Self::record`]
/// now WRITES, and the `Vec` is what it says it is: this run's mount inventory,
/// used for the startup log and for tests. Anything asking "which partitions
/// exist for this org" must ask the table — [`Self::durable_for_org`] — not this.
#[derive(Debug, Clone, Default)]
pub struct OsMemoryCatalog {
    partitions: Vec<OsMemoryPartition>,
}

impl OsMemoryCatalog {
    /// Durably record the partition for `(user, manifest)` and note it in this
    /// run's inventory.
    ///
    /// Fallible ON PURPOSE, and the caller must not lend a partition it could not
    /// record: an unrecorded partition is precisely the orphaned data this exists
    /// to prevent — rows under a NUL-containing owner key that nothing can later
    /// attribute to a user or a module.
    pub async fn record(
        &mut self,
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
        self.partitions.push(p.clone());
        Ok(p)
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
                // UNIQUE in the catalog, so `record` fails and `lend` withholds
                // the capability entirely. Losing memory for a run is the
                // correct outcome; silently starting a fresh partition beside
                // the old one is not.
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

fn to_recollection(s: agent24_memory::event::StoredEvent) -> Recollection {
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

#[async_trait::async_trait]
impl ScopedMemory for OsScopedMemory {
    async fn remember(&self, what: Remember) -> agent24_domain::Result<Remembered> {
        // Bounded BEFORE anything is written. See `MAX_KIND_BYTES`: the partition
        // stops A from reading B, and these stop A from crowding B out of the
        // database they share.
        if what.kind.trim().is_empty() {
            return Err(DomainError::Memory("kind must not be empty".into()));
        }
        if what.kind.len() > MAX_KIND_BYTES {
            return Err(DomainError::Memory(format!(
                "kind exceeds {MAX_KIND_BYTES} bytes"
            )));
        }
        let body = serde_json::Value::Object(what.body);
        let encoded = serde_json::to_string(&body)
            .map_err(|e| DomainError::Memory(format!("body is not serialisable: {e}")))?;
        if encoded.len() > MAX_BODY_BYTES {
            return Err(DomainError::Memory(format!(
                "body is {} bytes, over the {MAX_BODY_BYTES}-byte limit for one memory",
                encoded.len()
            )));
        }
        let ev = MemEvent::new(
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
        );
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

/// A [`KernelCtx`](agent24_domain::KernelCtx) that also lends memory.
pub struct MemoryCtx {
    pub sink: Option<agent24_domain::EventSink>,
    pub memory: Option<Arc<OsScopedMemory>>,
}

impl agent24_domain::KernelCtx for MemoryCtx {
    fn events(&self) -> Option<&agent24_domain::EventSink> {
        self.sink.as_ref()
    }
    fn memory(&self) -> Option<&dyn ScopedMemory> {
        self.memory.as_deref().map(|m| m as &dyn ScopedMemory)
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
        let mut cat = OsMemoryCatalog::default();
        let org = org_of(kv, user).await;
        let p = cat.record(&org, user, &manifest(name), kv).await.unwrap();
        OsScopedMemory::new(&p, kv)
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
        cat.record(
            &org_of(&kv, "alice").await,
            "alice",
            &manifest("sin90"),
            &kv,
        )
        .await
        .unwrap();
        cat.record(
            &org_of(&kv, "alice").await,
            "alice",
            &manifest("cos72"),
            &kv,
        )
        .await
        .unwrap();
        cat.record(&org_of(&kv, "bob").await, "bob", &manifest("sin90"), &kv)
            .await
            .unwrap();

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
            let mut cat = OsMemoryCatalog::default();
            for name in ["sin90", "cos72"] {
                let p = cat
                    .record(&org_of(&kv, "alice").await, "alice", &manifest(name), &kv)
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
        run2.record(
            &org_of(&kv, "alice").await,
            "alice",
            &manifest("schedule"),
            &kv,
        )
        .await
        .unwrap();
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
    async fn recording_the_same_partition_twice_advances_only_last_seen_at() {
        // Restarts re-record every mounted partition, so `record` must be
        // idempotent. `first_seen_at` and `module_name` are write-once: a rename
        // must NOT rewrite the row that says what the key originally meant.
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();
        let mut cat = OsMemoryCatalog::default();
        let p = cat
            .record(
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
        cat.record(
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
    }

    #[tokio::test]
    async fn a_repeat_recording_advances_last_seen_at() {
        // Split from the test above, which asserted row count and the immutable
        // columns and would therefore have passed with the upsert changed to
        // `DO NOTHING` — leaving every repeatedly-mounted partition with a
        // `last_seen_at` frozen at its first sighting, which is the one column an
        // operator would use to tell a live partition from an abandoned one.
        //
        // `now_iso8601` has second resolution, so the wait is what makes the two
        // stamps distinguishable at all. It is the price of asserting the thing
        // rather than asserting around it.
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();
        let mut cat = OsMemoryCatalog::default();
        cat.record(
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

        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        cat.record(
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

        assert!(
            again[0].last_seen_at > first[0].last_seen_at,
            "last_seen_at must advance: {} -> {}",
            first[0].last_seen_at,
            again[0].last_seen_at
        );
        assert_eq!(
            again[0].first_seen_at, first[0].first_seen_at,
            "and first_seen_at must not move with it"
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
}
