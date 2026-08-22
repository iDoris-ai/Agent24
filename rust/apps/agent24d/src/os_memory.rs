//! The kernel side of [`ScopedMemory`] — a module's view of the shared memory
//! base, keyed so that two domain OSes under one user cannot reach each other.
//!
//! # The key
//!
//! A module's partition key is derived by the KERNEL:
//!
//! ```text
//!   v1\0<len(user)>\0<user>\0os:<len(module)>\0<module>
//! ```
//!
//! Three properties, each of which is load-bearing:
//!
//! - **LENGTH-PREFIXED**, so two different `(user, module)` pairs cannot produce
//!   one key. The first version of this was merely NUL-separated and its own test
//!   found the collision: `("a", "b\0os:c")` and `("a\0os:b", "c")` both rendered
//!   as `v1\0a\0os:b\0os:c`. Neither input is reachable today — the module name
//!   is validated ASCII — but this repo has already paid once for a concat
//!   identity two pairs could produce (MD-5's `consol-{owner}-{key}`, review #122
//!   B1), and "unreachable" is an argument where a length prefix is a property.
//! - **Version-prefixed.** The adversarial review of this design was blunt about
//!   why: baking the manifest name into storage identity creates semantic
//!   migration debt. After a module is renamed, the database alone cannot say
//!   whether `…os:calendar` should become `calendar`, become `schedule`, merge,
//!   or stay separate as an uninstalled historical module. The version does not
//!   remove that debt — [`OsMemoryCatalog`] is what makes it payable — but it
//!   stops a future migration from having to guess which encoding it is reading.
//! - **Disjoint from the user's own key.** The agent loop's memory is keyed by
//!   the bare user id, which can never equal a `v1\0…` string, so a module cannot
//!   reach the user's own memory and the user's memory is not polluted by
//!   modules.
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
//!   ids at all: the kernel mints them here, prefixed with the partition key, so
//!   a collision between modules is not representable.

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
const KEY_VERSION: &str = "v1";

/// Derive a module's partition key. Kernel-only: nothing a module can call.
///
/// LENGTH-PREFIXED, not merely separated. A plain `v1\0{user}\0os:{module}` looks
/// unambiguous and is not: `("a", "b\0os:c")` and `("a\0os:b", "c")` both render
/// as `v1\0a\0os:b\0os:c`. The module name is validated ASCII so today neither
/// input is reachable — but "unreachable" is an argument, and this repo has
/// already paid once for a concat identity that two different pairs could produce
/// (MD-5's `consol-{owner}-{key}`, review #122 B1). A length prefix makes the
/// collision unrepresentable for ANY input, which is a property rather than an
/// argument.
///
/// (Found by this file's own test, which asserted the collision was impossible
/// and discovered it was not.)
pub(crate) fn partition_key(user: &str, module: &str) -> String {
    format!(
        "{KEY_VERSION}\u{0}{}\u{0}{user}\u{0}os:{}\u{0}{module}",
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
/// the authenticated user and the VALIDATED manifest — rather than parsed back
/// out of the key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OsMemoryPartition {
    /// The physical `scope_owner` value used in storage.
    pub key: String,
    /// The logical user the partition belongs to.
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
/// exist for this user" must ask the table — [`Self::durable_for`] — not this.
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
        user: &str,
        manifest: &DomainOsManifest,
        kv: &agent24_memory::KvStore,
    ) -> Result<OsMemoryPartition, String> {
        let p = OsMemoryPartition {
            key: partition_key(user, manifest.name()),
            user: user.to_owned(),
            module: manifest.name().to_owned(),
        };
        kv.record_os_partition(&p.key, KEY_VERSION, &p.user, &p.module)
            .await
            .map_err(|e| e.to_string())?;
        self.partitions.push(p.clone());
        Ok(p)
    }

    /// What mounted this run. NOT the answer to "what exists" — see the type docs.
    pub fn partitions(&self) -> &[OsMemoryPartition] {
        &self.partitions
    }

    /// Every partition EVER recorded for `user`, from the durable table.
    ///
    /// The answer an export or erase path needs, and the reason it must not be a
    /// `LIKE` query over keys that contain NUL. Includes partitions belonging to
    /// modules that are disabled, uninstalled or renamed — which is the whole
    /// point, and the thing this run's [`Self::partitions`] cannot tell you.
    pub async fn durable_for(
        kv: &agent24_memory::KvStore,
        user: &str,
    ) -> Result<Vec<agent24_memory::OsPartitionRow>, String> {
        kv.os_partitions_for(user).await.map_err(|e| e.to_string())
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

    async fn handle(kv: &agent24_memory::KvStore, user: &str, name: &str) -> OsScopedMemory {
        let mut cat = OsMemoryCatalog::default();
        let p = cat.record(user, &manifest(name), kv).await.unwrap();
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
        // the other working. Ids are kernel-minted and partition-prefixed, so the
        // collision is not representable.
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
        log.append(&ev(&partition_key("alice", "sin90")))
            .await
            .unwrap();
        let err = log
            .append(&ev(&partition_key("alice", "cos72")))
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
        let k = partition_key("alice", "sin90");
        assert!(k.starts_with("v1\u{0}"), "{k:?}");
        // The concat-collision shape this repo already paid for once (#122 B1):
        // two different (user, module) pairs must not produce one key. The FIRST
        // version of this key failed exactly here — both of these rendered as
        // `v1\0a\0os:b\0os:c` — which is why the parts are length-prefixed.
        assert_ne!(
            partition_key("a", "b\u{0}os:c"),
            partition_key("a\u{0}os:b", "c"),
        );
        // Same shape without any NUL in the inputs, so it does not rely on an
        // exotic user id to be meaningful.
        assert_ne!(partition_key("ab", "c"), partition_key("a", "bc"));
        // And a module's key can never equal a bare user id.
        assert_ne!(k, "alice");

        // Two hand-picked counter-examples are not injectivity, which is what the
        // key actually has to have. Sweep a small cross product — including the
        // adversarial inputs (embedded NUL, the `os:` marker, a shared prefix) —
        // and assert the mapping is one-to-one.
        let users = ["", "a", "ab", "abc", "a\u{0}b", "os:a", "alice"];
        let modules = ["", "a", "ab", "abc", "b\u{0}os:c", "os:b", "sin90"];
        let mut seen = std::collections::HashMap::new();
        for u in users {
            for m in modules {
                let key = partition_key(u, m);
                if let Some(prev) = seen.insert(key.clone(), (u, m)) {
                    panic!("collision: {prev:?} and {:?} both produce {key:?}", (u, m));
                }
            }
        }
        assert_eq!(seen.len(), users.len() * modules.len());
    }

    #[tokio::test]
    async fn the_catalog_answers_what_a_prefix_match_should_not_have_to() {
        // The review's point: future export/erase code must not discover
        // partitions by LIKE-matching strings that contain NUL.
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();
        let mut cat = OsMemoryCatalog::default();
        cat.record("alice", &manifest("sin90"), &kv).await.unwrap();
        cat.record("alice", &manifest("cos72"), &kv).await.unwrap();
        cat.record("bob", &manifest("sin90"), &kv).await.unwrap();

        let alice = OsMemoryCatalog::durable_for(&kv, "alice").await.unwrap();
        assert_eq!(alice.len(), 2);
        assert!(alice.iter().all(|r| r.logical_user == "alice"));
        assert!(alice.iter().all(|r| r.key_version == KEY_VERSION));
        assert_eq!(
            OsMemoryCatalog::durable_for(&kv, "bob")
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(
            OsMemoryCatalog::durable_for(&kv, "nobody")
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
                let p = cat.record("alice", &manifest(name), &kv).await.unwrap();
                OsScopedMemory::new(&p, &kv)
                    .remember(Remember::new("note", serde_json::Map::new()))
                    .await
                    .unwrap();
            }
        }
        // Run 2: cos72 has been disabled, and sin90 renamed to schedule — so the
        // fresh run's inventory knows about ONE partition while three exist.
        let mut run2 = OsMemoryCatalog::default();
        run2.record("alice", &manifest("schedule"), &kv)
            .await
            .unwrap();
        assert_eq!(run2.partitions().len(), 1);

        let rows = OsMemoryCatalog::durable_for(&kv, "alice").await.unwrap();
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
    async fn recording_the_same_partition_twice_keeps_the_first_sighting() {
        // Restarts re-record every mounted partition, so `record` must be
        // idempotent. `first_seen_at` and `module_name` are write-once: a rename
        // must NOT rewrite the row that says what the key originally meant.
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();
        let mut cat = OsMemoryCatalog::default();
        let p = cat.record("alice", &manifest("sin90"), &kv).await.unwrap();
        let first = OsMemoryCatalog::durable_for(&kv, "alice").await.unwrap();
        cat.record("alice", &manifest("sin90"), &kv).await.unwrap();
        let again = OsMemoryCatalog::durable_for(&kv, "alice").await.unwrap();
        assert_eq!(again.len(), 1, "one row per partition, ever");
        assert_eq!(again[0].owner_key, p.key);
        assert_eq!(again[0].first_seen_at, first[0].first_seen_at);
        assert_eq!(again[0].module_name, "sin90");
    }

    #[tokio::test]
    async fn a_partition_recorded_with_a_different_identity_is_a_conflict() {
        // The test the previous one could not be: re-recording the SAME metadata
        // proves nothing about what happens when the stored identity disagrees.
        // The first upsert took every conflict as success and updated only
        // `last_seen_at`, so a drifted row returned `Ok`, the handle was lent, and
        // the catalog went on attributing new data to the old identity.
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();
        let key = partition_key("alice", "sin90");
        kv.record_os_partition(&key, KEY_VERSION, "alice", "sin90")
            .await
            .unwrap();

        for (ver, user, module) in [
            ("v2", "alice", "sin90"),
            (KEY_VERSION, "bob", "sin90"),
            (KEY_VERSION, "alice", "cos72"),
        ] {
            let err = kv
                .record_os_partition(&key, ver, user, module)
                .await
                .expect_err("a disagreeing identity must not be accepted");
            assert!(
                matches!(err, agent24_memory::MemoryError::Conflict(_)),
                "{err}"
            );
        }
        // The original row is untouched by any of the three attempts.
        let rows = OsMemoryCatalog::durable_for(&kv, "alice").await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].module_name, "sin90");
        assert_eq!(rows[0].key_version, KEY_VERSION);
    }
}
