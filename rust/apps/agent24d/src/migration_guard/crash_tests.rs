//! T9 / design section 8 R3: power-loss exhaustive replay.
//!
//! One scenario (guard `Preparing`, snapshot, `Snapshotted`, `Migrating`, a
//! failed migration scribbling on the databases, then `restore`) runs under
//! [`CrashingOps`]. A crash image is taken after EVERY `FileOps` call (and at
//! two non-`FileOps` points). Each image is "rebooted" into a fresh directory
//! and `startup_replay` runs on it, again under `CrashingOps`; every call of
//! that replay is a crash point too, and so on for [`DEPTH`] nested crashes.
//! Identical images are explored once. The outcome is derived from the phase
//! found on the rebooted disk and must be exactly:
//!
//! | durable log          | required                                   |
//! |----------------------|--------------------------------------------|
//! | absent               | `Proceed`, pair == original                |
//! | `Committed`          | `Proceed`, pair == the migrated bytes      |
//! | `Preparing`/`Snapshotted` | `Docked(Interrupted(same))`, pair == original |
//! | `Migrating`/`Restoring`/`Restored` | `Restored{from: same}`, pair == snapshot, no sidecar, log `Restored`, replay again is stable |
//!
//! so a dock with an intact snapshot (nothing external failed) is a failure
//! (T9). Invariants on every durable image, before replay runs:
//!
//! - I1  phase >= `Migrating` implies the snapshot verifies; `Snapshotted`
//!   implies a complete manifest;
//! - I3  guard log and every published manifest are never torn;
//! - I4  evidence: once an image holds the failed migration's bytes of a
//!   database, every later image and every replay descendant still holds
//!   them somewhere (db directory or quarantine).
//!
//! Model boundary (#1427 F2): a rebooted image is treated as fully durable.
//! The window "process crashes, page cache not yet on disk, then power is
//! lost as well" is NOT modelled: dropping an `fsync_file` on the accepted
//! main file (restore.rs shortcut) leaves every `t9_*` green and is guarded
//! only by the unit test `low2_shortcut_fsyncs_the_accepted_main_file`.
//! `RollbackFailed` is also outside the crash matrix: it is only written by
//! an injected-error path, never by a crash, and replay never touches files
//! for it (`rollback_failed_phase_docks_and_is_never_retried`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashSet;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode};
use sqlx::{Connection, SqliteConnection};

use super::atomic::{FileOps, RealOps};
use super::crash_ops_tests::{
    CrashingOps, Image, Mutation, Node, Policy, Pred, Unsynced, write_image,
};
use super::guard_log::{self, DbRestore, GuardLog, Phase, advance};
use super::manifest::{BinaryIdentity, MEMORY_FILE, STORE_FILE, read_manifest, verify_complete};
use super::replay::{Dock, ReplayOutcome, startup_replay};
use super::restore::{RestoreRequest, restore};
use super::snapshot::{SnapshotRequest, take_snapshot};

const DEPTH: u32 = 2;
/// Independent per-directory / per-inode outcomes sampled at the first crash.
const SEEDS: u64 = 2;

const fn pol(content: Unsynced, entries_current: bool) -> Policy {
    Policy {
        content,
        entries_current,
        seed: None,
    }
}
const MIN: Policy = pol(Unsynced::Drop, false);

/// POSIX minimum (spec), content/entry variants, plus seeded independent ones.
fn first_crash_policies() -> Vec<Policy> {
    let mut v = vec![
        MIN,
        pol(Unsynced::Keep, false),
        pol(Unsynced::Half, false),
        pol(Unsynced::Drop, true),
        pol(Unsynced::Keep, true),
        pol(Unsynced::Half, true),
    ];
    v.extend((1..=SEEDS).map(|s| Policy {
        seed: Some(s),
        ..MIN
    }));
    v
}

/// Crashes during replay: the minimum, plus one independent sample for the
/// first replay.
fn replay_crash_policies(depth: u32) -> Vec<Policy> {
    let mut v = vec![MIN];
    if depth == 0 {
        v.push(Policy {
            seed: Some(100),
            ..MIN
        });
    }
    v
}

type Pair = [Option<Vec<u8>>; 2];

#[derive(Clone, Copy)]
struct Layout {
    /// Databases live in `<data_dir>/db` (their directory differs from the
    /// guard's, so the db directory's own fsync is observable).
    nested: bool,
    memory_existed: bool,
}

struct Fx {
    _td: tempfile::TempDir,
    root: PathBuf,
    store: PathBuf,
    memory: PathBuf,
    original: Pair,
}

/// Distinct rows and watermark per database, so restoring one database's
/// snapshot into the other can never pass.
async fn make_db(p: &Path, rows: i64, version: i64) {
    let o = SqliteConnectOptions::new()
        .filename(p)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal);
    let mut c = SqliteConnection::connect_with(&o).await.unwrap();
    for sql in [
        "CREATE TABLE t(x INTEGER)".to_owned(),
        "CREATE TABLE _sqlx_migrations(version INTEGER, success BOOLEAN)".to_owned(),
        format!("INSERT INTO _sqlx_migrations VALUES ({version}, 1)"),
    ] {
        sqlx::query(&sql).execute(&mut c).await.unwrap();
    }
    for r in 0..rows {
        sqlx::query("INSERT INTO t VALUES (?)")
            .bind(r * version)
            .execute(&mut c)
            .await
            .unwrap();
    }
    c.close().await.unwrap();
}

fn pair(store: &Path, memory: &Path) -> Pair {
    [fs::read(store).ok(), fs::read(memory).ok()]
}

async fn fixture(l: Layout) -> Fx {
    let td = tempfile::tempdir().unwrap();
    let root = td.path().to_path_buf();
    let dbdir = if l.nested {
        root.join("db")
    } else {
        root.clone()
    };
    fs::create_dir_all(&dbdir).unwrap();
    let (store, memory) = (dbdir.join(STORE_FILE), dbdir.join(MEMORY_FILE));
    make_db(&store, 300, 5).await;
    if l.memory_existed {
        make_db(&memory, 120, 7).await;
    }
    let original = pair(&store, &memory);
    assert_ne!(original[0], original[1]);
    Fx {
        _td: td,
        root,
        store,
        memory,
        original,
    }
}

fn target() -> BinaryIdentity {
    BinaryIdentity {
        version: "2".into(),
        sha256: "y".into(),
    }
}

/// A migration: scribble on the live databases, leave sidecars (or create a
/// brand-new memory.db when there was none).
fn migrate_badly(fx: &Fx) {
    for (i, p) in [&fx.store, &fx.memory].into_iter().enumerate() {
        if let Ok(mut b) = fs::read(p) {
            b[100] ^= 0xff;
            b.extend(std::iter::repeat_n(7 + i as u8, 4096));
            fs::write(p, b).unwrap(); // in place: same inode
        } else {
            fs::write(p, vec![9u8; 4096]).unwrap();
        }
        for (sfx, n) in [("-wal", 8192), ("-shm", 4096), ("-journal", 512)] {
            fs::write(format!("{}{sfx}", p.display()), vec![1u8; n]).unwrap();
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// The migration fails; `restore` runs.
    Fail,
    /// The migration succeeds (SQLite syncs its own files) and commits.
    Commit,
}

struct Run {
    /// The migration's bytes (store, memory) and the first image index taken
    /// after it wrote them.
    dirty: Pair,
    dirty_from: usize,
    images: Vec<(String, Policy, Image)>,
    snap: Pair,
    calls: usize,
}

async fn run_scenario(l: Layout, m: Mutation, mode: Mode) -> (Fx, Run) {
    let fx = fixture(l).await;
    let ops = CrashingOps::new(&fx.root, &first_crash_policies(), m);
    let req = SnapshotRequest {
        data_dir: &fx.root,
        store_db: &fx.store,
        memory_db: &fx.memory,
        attempt_id: "a1",
        compact_utc: "20261027T000000Z",
        created_at: "2026-10-27T00:00:00Z",
        source: None,
        target: target(),
        migrations_digest: "dd",
    };
    let mut log = GuardLog::new("a1".into(), target());
    guard_log::persist(&ops, &fx.root, &log).unwrap();
    let (id, _) = take_snapshot(&ops, &req).await.unwrap();
    log.snapshot_id = Some(id.clone());
    for to in [Phase::Snapshotted, Phase::Migrating] {
        advance(&ops, &fx.root, &mut log, to).unwrap();
    }
    ops.boundary("migration start");
    let dirty_from = ops.image_count();
    migrate_badly(&fx);
    let dirty = pair(&fx.store, &fx.memory);
    ops.boundary("migration wrote");
    if mode == Mode::Commit {
        // a successful migration made its files durable itself (SQLite fsyncs)
        let dbdir = fx.store.parent().unwrap();
        for p in [&fx.store, &fx.memory] {
            for sfx in ["", "-wal", "-shm", "-journal"] {
                let f = PathBuf::from(format!("{}{sfx}", p.display()));
                ops.fsync_file(&f).unwrap();
            }
        }
        ops.fsync_dir(dbdir).unwrap();
        advance(&ops, &fx.root, &mut log, Phase::Committed).unwrap();
    } else {
        let rr = RestoreRequest {
            data_dir: &fx.root,
            store_db: &fx.store,
            memory_db: &fx.memory,
        };
        restore(&ops, &rr, &mut log).unwrap();
    }
    let sd = fx.root.join("migration-snapshots").join(&id);
    let snap = [
        fs::read(sd.join(STORE_FILE)).ok(),
        fs::read(sd.join(MEMORY_FILE)).ok(),
    ];
    let calls = ops.calls().len();
    let run = Run {
        dirty,
        dirty_from,
        images: ops.take_images(),
        snap,
        calls,
    };
    (fx, run)
}

#[derive(Default)]
struct Explorer {
    original: Pair,
    snap: Pair,
    dirty: Pair,
    commit: bool,
    nested: bool,
    mutation: Mutation,
    depth: u32,
    seen: HashSet<(u64, u32, [bool; 2])>,
    violations: Vec<String>,
    replays: usize,
    replay_calls: usize,
    /// Mutant runs only need the first violation.
    stop_early: bool,
}

fn digest(img: &Image) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    img.hash(&mut h);
    h.finish()
}

fn db_paths(root: &Path, nested: bool) -> (PathBuf, PathBuf) {
    let d = if nested {
        root.join("db")
    } else {
        root.to_path_buf()
    };
    (d.join(STORE_FILE), d.join(MEMORY_FILE))
}

fn sidecars(p: &Path) -> bool {
    ["-wal", "-shm", "-journal"]
        .iter()
        .any(|s| fs::symlink_metadata(format!("{}{s}", p.display())).is_ok())
}

/// Is `bytes` held by any file of the image?
fn holds(img: &Image, bytes: &Option<Vec<u8>>) -> bool {
    bytes.as_ref().is_some_and(|b| {
        img.values()
            .any(|v| matches!(v, Node::File(c, _) if c == b))
    })
}

impl Explorer {
    fn new(fx: &Fx, run: &Run, l: Layout, m: Mutation, depth: u32, mode: Mode) -> Self {
        Self {
            original: fx.original.clone(),
            snap: run.snap.clone(),
            dirty: run.dirty.clone(),
            commit: mode == Mode::Commit,
            nested: l.nested,
            mutation: m,
            depth,
            stop_early: depth < DEPTH,
            ..Self::default()
        }
    }

    fn bad(&mut self, tag: &str, msg: String) {
        self.violations.push(format!("[{tag}] {msg}"));
    }

    /// I4 for one image; returns the obligations it hands to later images.
    fn evidence(&mut self, img: &Image, must: [bool; 2], tag: &str) -> [bool; 2] {
        let mut next = must;
        if self.commit {
            return next;
        }
        for n in 0..2 {
            let here = holds(img, &self.dirty[n]);
            if must[n] && !here {
                self.bad(tag, format!("I4 failed migration's database #{n} was lost"));
            }
            next[n] |= here;
        }
        next
    }

    fn explore(&mut self, img: &Image, d: u32, tag: &str, must: [bool; 2]) {
        if (self.stop_early && !self.violations.is_empty())
            || !self.seen.insert((digest(img), d, must))
        {
            return;
        }
        let must = self.evidence(img, must, tag);
        let td = tempfile::tempdir().unwrap();
        let root = td.path();
        write_image(img, root);
        let (s, m) = db_paths(root, self.nested);
        let phase = self.check_durable_state(root, tag);
        let pols = if d < self.depth {
            replay_crash_policies(d)
        } else {
            vec![]
        };
        let ops = CrashingOps::new(root, &pols, self.mutation.clone());
        let rr = RestoreRequest {
            data_dir: root,
            store_db: &s,
            memory_db: &m,
        };
        let out = startup_replay(&ops, &rr);
        self.replays += 1;
        self.replay_calls += ops.calls().len();
        self.check(root, &s, &m, &rr, &out, phase, tag);
        let must_after = self.evidence_after(root, tag, must);
        for (label, p, sub) in ops.take_images() {
            let t = format!(
                "{tag} > {label} {}",
                p.seed.map_or("min".into(), |x| format!("seed{x}"))
            );
            self.explore(&sub, d + 1, &t, must_after);
        }
    }

    /// The finished replay must not have lost evidence either.
    fn evidence_after(&mut self, root: &Path, tag: &str, must: [bool; 2]) -> [bool; 2] {
        let mut img = Image::new();
        read_tree(root, Path::new(""), &mut img);
        self.evidence(&img, must, &format!("{tag} (after replay)"))
    }

    /// I1 / I3 on the durable image before anything runs. Returns the phase
    /// found, `None` when there is no log.
    fn check_durable_state(&mut self, root: &Path, tag: &str) -> Option<Phase> {
        let snaps = root.join("migration-snapshots");
        for ent in fs::read_dir(&snaps).into_iter().flatten().flatten() {
            let mf = ent.path().join("manifest.json");
            if mf.exists()
                && let Err(e) = read_manifest(&mf)
            {
                self.bad(tag, format!("I3 published manifest is torn: {e:?}"));
            }
        }
        match guard_log::read(root) {
            Err(e) => {
                self.bad(tag, format!("I3 guard log torn or unreadable: {e:?}"));
                None
            }
            Ok(None) => None,
            Ok(Some(g)) => {
                if !matches!(g.phase, Phase::Preparing | Phase::Committed) {
                    let id = g.snapshot_id.clone().unwrap_or_default();
                    if let Err(e) = verify_complete(root, &id, &g.attempt_id) {
                        self.bad(
                            tag,
                            format!("I1 durable {:?} but snapshot fails: {e:?}", g.phase),
                        );
                    }
                }
                Some(g.phase)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn check(
        &mut self,
        root: &Path,
        s: &Path,
        m: &Path,
        rr: &RestoreRequest<'_>,
        out: &ReplayOutcome,
        phase: Option<Phase>,
        tag: &str,
    ) {
        let got = pair(s, m);
        let committed = [self.dirty[0].clone(), self.dirty[1].clone()];
        match (phase, out) {
            (None, ReplayOutcome::Proceed) => {
                if got != self.original {
                    self.bad(tag, "Proceed without a log but the pair changed".into());
                }
            }
            (Some(Phase::Committed), ReplayOutcome::Proceed) => {
                if !self.commit || got != committed {
                    self.bad(tag, "Proceed on Committed with the wrong pair".into());
                }
            }
            (
                Some(p @ (Phase::Preparing | Phase::Snapshotted)),
                ReplayOutcome::Docked(Dock::Interrupted(q)),
            ) if p == *q => {
                if got != self.original {
                    self.bad(tag, "Interrupted but databases are not the original".into());
                }
            }
            (
                Some(p @ (Phase::Migrating | Phase::Restoring | Phase::Restored)),
                ReplayOutcome::Restored { from, .. },
            ) if p == *from => self.check_restored(root, s, m, rr, tag),
            _ => self.bad(tag, format!("phase {phase:?} but replay said {out:?}")),
        }
    }

    fn check_restored(
        &mut self,
        root: &Path,
        s: &Path,
        m: &Path,
        rr: &RestoreRequest<'_>,
        tag: &str,
    ) {
        if pair(s, m) != self.snap {
            self.bad(tag, "Restored but pair != snapshot bytes".into());
        }
        if sidecars(s) || sidecars(m) {
            self.bad(tag, "Restored but a sidecar survived".into());
        }
        match guard_log::read(root) {
            Ok(Some(g))
                if g.phase == Phase::Restored
                    && g.store_restore == DbRestore::Done
                    && g.memory_restore == DbRestore::Done => {}
            other => self.bad(tag, format!("Restored but log is {other:?}")),
        }
        let again = startup_replay(&CrashingOps::new(root, &[], Mutation::default()), rr);
        if !matches!(
            again,
            ReplayOutcome::Restored {
                from: Phase::Restored,
                ..
            }
        ) || pair(s, m) != self.snap
        {
            self.bad(tag, format!("replay not idempotent: {again:?}"));
        }
    }
}

/// Whole tree as an image (volatile state of a finished replay).
fn read_tree(dir: &Path, rel: &Path, out: &mut Image) {
    for ent in fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = rel.join(ent.file_name());
        if ent.path().is_dir() {
            out.insert(p.clone(), Node::Dir);
            read_tree(&ent.path(), &p, out);
        } else if let Ok(b) = fs::read(ent.path()) {
            out.insert(p, Node::File(b, 0));
        }
    }
}

struct Report {
    crash_points: usize,
    images: usize,
    replays: usize,
    replay_calls: usize,
    violations: Vec<String>,
}

async fn explore_all(l: Layout, m: Mutation, depth: u32) -> Report {
    explore_mode(l, m, depth, Mode::Fail).await
}

async fn explore_mode(l: Layout, m: Mutation, depth: u32, mode: Mode) -> Report {
    let (fx, run) = run_scenario(l, m.clone(), mode).await;
    let mut ex = Explorer::new(&fx, &run, l, m, depth, mode);
    let n = run.images.len();
    // evidence is monotone along one fully-persisting policy's timeline
    let mut timeline: std::collections::HashMap<(Unsynced, bool), [bool; 2]> = Default::default();
    for (i, (label, p, img)) in run.images.iter().enumerate() {
        let tag = format!("{label} {p:?}");
        let mut must = [false; 2];
        if p.seed.is_none() && p.content == Unsynced::Keep && i >= run.dirty_from {
            let t = timeline.entry((p.content, p.entries_current)).or_default();
            must = *t;
            *t = ex.evidence(img, must, &tag);
        }
        ex.explore(img, 0, &tag, must);
    }
    Report {
        crash_points: run.calls,
        images: n,
        replays: ex.replays,
        replay_calls: ex.replay_calls,
        violations: ex.violations,
    }
}

fn show(name: &str, r: &Report) {
    println!(
        "{name}: scenario calls={} images={} unique replays={} replay calls={} violations={}",
        r.crash_points,
        r.images,
        r.replays,
        r.replay_calls,
        r.violations.len()
    );
    for v in r.violations.iter().take(6) {
        println!("  {v}");
    }
}

const LAYOUTS: [Layout; 3] = [
    Layout {
        nested: false,
        memory_existed: true,
    },
    Layout {
        nested: true,
        memory_existed: true,
    },
    Layout {
        nested: true,
        memory_existed: false,
    },
];

async fn exhaustive(l: Layout) {
    let r = explore_all(l, Mutation::default(), DEPTH).await;
    show(&format!("nested={} mem={}", l.nested, l.memory_existed), &r);
    assert!(r.crash_points > 30, "{}", r.crash_points);
    assert!(
        r.violations.is_empty(),
        "{:#?}",
        &r.violations[..r.violations.len().min(10)]
    );
}

#[tokio::test]
async fn t9_power_loss_flat_layout_two_nested_crashes() {
    exhaustive(LAYOUTS[0]).await;
}

#[tokio::test]
async fn t9_power_loss_nested_db_dir_two_nested_crashes() {
    exhaustive(LAYOUTS[1]).await;
}

#[tokio::test]
async fn t9_power_loss_memory_db_absent_two_nested_crashes() {
    exhaustive(LAYOUTS[2]).await;
}

/// The success path: `Committed` must replay as `Proceed` with the migrated
/// bytes; every earlier crash must still restore or dock exactly.
#[tokio::test]
async fn t9_power_loss_committed_path_one_nested_crash() {
    let r = explore_mode(LAYOUTS[1], Mutation::default(), 1, Mode::Commit).await;
    show("commit path", &r);
    assert!(
        r.violations.is_empty(),
        "{:#?}",
        &r.violations[..r.violations.len().min(10)]
    );
}

fn pred(f: impl Fn(&str) -> bool + 'static) -> Pred {
    Rc::new(move |p: &Path| f(&p.to_string_lossy()))
}

async fn caught(name: &str, m: Mutation) {
    for l in LAYOUTS {
        let r = explore_all(l, m.clone(), 1).await;
        show(
            &format!("MUTANT {name} nested={} mem={}", l.nested, l.memory_existed),
            &r,
        );
        if !r.violations.is_empty() {
            return;
        }
    }
    panic!("mutant {name} survived: the model missed it");
}

#[tokio::test]
async fn mutant_snapshot_file_not_fsynced_is_caught() {
    let m = Mutation {
        skip_file_sync: Some(pred(|p| {
            p.contains("migration-snapshots")
                && !p.ends_with("manifest.json")
                && !p.contains(".manifest.json")
        })),
        ..Mutation::default()
    };
    caught("snapshot-file-fsync", m).await;
}

#[tokio::test]
async fn mutant_snapshot_parent_dir_not_fsynced_is_caught() {
    let m = Mutation {
        skip_dir_sync: Some(pred(|p| p.ends_with("migration-snapshots"))),
        ..Mutation::default()
    };
    caught("snapshots-dir-fsync", m).await;
}

#[tokio::test]
async fn mutant_guard_log_not_fsynced_is_caught() {
    let m = Mutation {
        skip_file_sync: Some(pred(|p| p.contains(".migration-guard.json.tmp"))),
        ..Mutation::default()
    };
    caught("guard-log-fsync", m).await;
}

#[tokio::test]
async fn mutant_staging_copy_not_fsynced_is_caught() {
    let m = Mutation {
        skip_file_sync: Some(pred(|p| p.contains(".restore.a1"))),
        ..Mutation::default()
    };
    caught("staging-fsync", m).await;
}

#[tokio::test]
async fn mutant_db_dir_not_fsynced_after_install_is_caught() {
    // Only observable when the db directory differs from the guard's.
    let m = Mutation {
        skip_dir_sync: Some(pred(|p| p.ends_with("/db"))),
        ..Mutation::default()
    };
    let r = explore_all(LAYOUTS[1], m, 1).await;
    show("MUTANT db-dir-fsync", &r);
    assert!(!r.violations.is_empty(), "mutant survived");
}

#[tokio::test]
async fn mutant_quarantine_dir_not_fsynced_is_caught() {
    let m = Mutation {
        skip_dir_sync: Some(pred(|p| p.ends_with("migration-quarantine/a1"))),
        ..Mutation::default()
    };
    caught("quarantine-dir-fsync", m).await;
}

#[tokio::test]
async fn mutant_manifest_not_fsynced_is_caught() {
    let m = Mutation {
        skip_file_sync: Some(pred(|p| p.contains(".manifest.json.tmp"))),
        ..Mutation::default()
    };
    caught("manifest-fsync", m).await;
}

// ---- model faithfulness ----------------------------------------------------

#[test]
fn model_rename_needs_directory_fsync_and_content_needs_file_fsync() {
    let td = tempfile::tempdir().unwrap();
    let root = td.path();
    fs::write(root.join("old"), b"OLD").unwrap();
    let ops = CrashingOps::new(root, &[], Mutation::default());
    ops.write_new_synced(&root.join("tmp"), b"NEW").unwrap();
    ops.rename(&root.join("tmp"), &root.join("old")).unwrap();
    let get = |i: &Image, n: &str| i.get(Path::new(n)).cloned();
    let before = ops.image(MIN);
    assert_eq!(
        get(&before, "old"),
        Some(Node::File(b"OLD".to_vec(), 0)),
        "rename not durable"
    );
    assert_eq!(get(&before, "tmp"), None, "new entry not durable");
    ops.fsync_dir(root).unwrap();
    let after = ops.image(MIN);
    assert_eq!(get(&after, "old"), Some(Node::File(b"NEW".to_vec(), 0)));
    assert_eq!(get(&after, "tmp"), None);
}

#[test]
fn model_unsynced_content_is_lost_but_entry_survives_dir_fsync() {
    let td = tempfile::tempdir().unwrap();
    let root = td.path();
    let ops = CrashingOps::new(root, &[], Mutation::default());
    fs::write(root.join("f"), b"DATA").unwrap(); // not via FileOps
    ops.fsync_dir(root).unwrap();
    let get = |u: Unsynced| ops.image(pol(u, false)).get(Path::new("f")).cloned();
    assert_eq!(get(Unsynced::Drop), Some(Node::File(vec![], 0)));
    assert_eq!(get(Unsynced::Keep), Some(Node::File(b"DATA".to_vec(), 0)));
    assert_eq!(get(Unsynced::Half), Some(Node::File(b"DA".to_vec(), 0)));
    ops.fsync_file(&root.join("f")).unwrap();
    assert_eq!(get(Unsynced::Drop), Some(Node::File(b"DATA".to_vec(), 0)));
}

#[test]
fn model_cross_directory_rename_needs_both_directories() {
    let td = tempfile::tempdir().unwrap();
    let root = td.path();
    fs::create_dir(root.join("a")).unwrap();
    fs::create_dir(root.join("b")).unwrap();
    fs::write(root.join("a/f"), b"X").unwrap();
    let ops = CrashingOps::new(root, &[], Mutation::default());
    ops.rename(&root.join("a/f"), &root.join("b/f")).unwrap();
    let has = |n: &str| ops.image(MIN).contains_key(Path::new(n));
    assert!(has("a/f") && !has("b/f"));
    ops.fsync_dir(&root.join("b")).unwrap();
    assert!(has("a/f") && has("b/f"), "file under both names is legal");
    ops.fsync_dir(&root.join("a")).unwrap();
    assert!(!has("a/f") && has("b/f"));
}

#[test]
fn model_new_directory_content_needs_its_own_fsync() {
    let td = tempfile::tempdir().unwrap();
    let root = td.path();
    let ops = CrashingOps::new(root, &[], Mutation::default());
    fs::create_dir(root.join("d")).unwrap();
    ops.write_new_synced(&root.join("d/f"), b"X").unwrap();
    ops.fsync_dir(root).unwrap();
    let img = ops.image(MIN);
    assert_eq!(img.get(Path::new("d")), Some(&Node::Dir));
    assert!(
        !img.contains_key(Path::new("d/f")),
        "d itself was never fsynced"
    );
    ops.fsync_dir(&root.join("d")).unwrap();
    assert!(ops.image(MIN).contains_key(Path::new("d/f")));
}

#[test]
fn model_current_entries_policy_keeps_unfsynced_names_but_not_content() {
    let td = tempfile::tempdir().unwrap();
    let root = td.path();
    let ops = CrashingOps::new(root, &[], Mutation::default());
    ops.write_new_synced(&root.join("tmp"), b"NEW").unwrap();
    ops.rename(&root.join("tmp"), &root.join("final")).unwrap();
    let img = ops.image(pol(Unsynced::Drop, true));
    assert_eq!(
        img.get(Path::new("final")),
        Some(&Node::File(b"NEW".to_vec(), 0)),
        "synced content"
    );
    fs::write(root.join("raw"), b"ABCD").unwrap();
    let img = ops.image(pol(Unsynced::Drop, true));
    assert_eq!(
        img.get(Path::new("raw")),
        Some(&Node::File(vec![], 1)),
        "never synced"
    );
    assert!(
        !ops.image(MIN).contains_key(Path::new("final")),
        "durable entries stay pessimistic"
    );
}

#[test]
fn model_hard_link_aliases_survive_the_reboot() {
    use std::os::unix::fs::MetadataExt;
    let td = tempfile::tempdir().unwrap();
    let root = td.path();
    fs::create_dir(root.join("a")).unwrap();
    fs::create_dir(root.join("b")).unwrap();
    fs::write(root.join("a/f"), b"X").unwrap();
    let ops = CrashingOps::new(root, &[], Mutation::default());
    ops.rename(&root.join("a/f"), &root.join("b/f")).unwrap();
    ops.fsync_dir(&root.join("b")).unwrap();
    let img = ops.image(MIN);
    assert_eq!(
        img.get(Path::new("a/f")),
        img.get(Path::new("b/f")),
        "one alias class"
    );
    let out = tempfile::tempdir().unwrap();
    write_image(&img, out.path());
    let (a, b) = (out.path().join("a/f"), out.path().join("b/f"));
    assert_eq!(
        fs::metadata(a).unwrap().ino(),
        fs::metadata(b).unwrap().ino()
    );
}

#[test]
fn model_seeded_policy_can_keep_one_of_two_new_files() {
    let td = tempfile::tempdir().unwrap();
    let root = td.path();
    fs::create_dir(root.join("a")).unwrap();
    fs::create_dir(root.join("b")).unwrap();
    let ops = CrashingOps::new(root, &[], Mutation::default());
    ops.write_new_synced(&root.join("a/x"), b"1").unwrap();
    ops.write_new_synced(&root.join("b/y"), b"2").unwrap();
    let mut seen = HashSet::new();
    for seed in 0..64 {
        let img = ops.image(Policy {
            seed: Some(seed),
            ..MIN
        });
        seen.insert((
            img.contains_key(Path::new("a/x")),
            img.contains_key(Path::new("b/y")),
        ));
    }
    assert!(
        seen.contains(&(true, false)) && seen.contains(&(false, true)),
        "{seen:?}"
    );
}

#[test]
fn model_seeded_cross_directory_rename_is_atomic() {
    let td = tempfile::tempdir().unwrap();
    let root = td.path();
    fs::create_dir(root.join("a")).unwrap();
    fs::create_dir(root.join("b")).unwrap();
    fs::write(root.join("a/f"), b"X").unwrap();
    let ops = CrashingOps::new(root, &[], Mutation::default());
    ops.rename(&root.join("a/f"), &root.join("b/f")).unwrap();
    let mut outcomes = HashSet::new();
    for seed in 0..64 {
        let img = ops.image(Policy {
            seed: Some(seed),
            ..MIN
        });
        let (a, b) = (
            img.contains_key(Path::new("a/f")),
            img.contains_key(Path::new("b/f")),
        );
        assert!(a != b, "rename must be all or nothing, got a={a} b={b}");
        outcomes.insert(a);
    }
    assert_eq!(outcomes.len(), 2, "both outcomes are sampled");
}

#[test]
fn model_half_keeps_the_durable_prefix() {
    let td = tempfile::tempdir().unwrap();
    let root = td.path();
    fs::write(root.join("f"), b"ABCD").unwrap();
    let ops = CrashingOps::new(root, &[], Mutation::default());
    fs::write(root.join("f"), b"ABCDEFGH").unwrap(); // appended in place
    let img = ops.image(pol(Unsynced::Half, false));
    assert_eq!(
        img.get(Path::new("f")),
        Some(&Node::File(b"ABCDEF".to_vec(), 0))
    );
}

// ---- startup_replay decision table ----------------------------------------

async fn state_at(phase: Phase) -> (Fx, GuardLog) {
    let fx = fixture(LAYOUTS[1]).await;
    let req = SnapshotRequest {
        data_dir: &fx.root,
        store_db: &fx.store,
        memory_db: &fx.memory,
        attempt_id: "a1",
        compact_utc: "20261027T000000Z",
        created_at: "x",
        source: None,
        target: target(),
        migrations_digest: "dd",
    };
    let mut log = GuardLog::new("a1".into(), target());
    guard_log::persist(&RealOps, &fx.root, &log).unwrap();
    if phase == Phase::Preparing {
        return (fx, log);
    }
    let (id, _) = take_snapshot(&RealOps, &req).await.unwrap();
    log.snapshot_id = Some(id);
    advance(&RealOps, &fx.root, &mut log, Phase::Snapshotted).unwrap();
    if phase != Phase::Snapshotted {
        advance(&RealOps, &fx.root, &mut log, Phase::Migrating).unwrap();
    }
    if phase == Phase::Committed {
        advance(&RealOps, &fx.root, &mut log, Phase::Committed).unwrap();
    }
    (fx, log)
}

fn replay(fx: &Fx) -> ReplayOutcome {
    let rr = RestoreRequest {
        data_dir: &fx.root,
        store_db: &fx.store,
        memory_db: &fx.memory,
    };
    startup_replay(&RealOps, &rr)
}

#[tokio::test]
async fn replay_decision_table() {
    let (fx, _) = state_at(Phase::Preparing).await;
    assert!(matches!(
        replay(&fx),
        ReplayOutcome::Docked(Dock::Interrupted(Phase::Preparing))
    ));
    let (fx, _) = state_at(Phase::Snapshotted).await;
    assert!(matches!(
        replay(&fx),
        ReplayOutcome::Docked(Dock::Interrupted(Phase::Snapshotted))
    ));
    let (fx, _) = state_at(Phase::Committed).await;
    assert!(replay(&fx).may_proceed());
    let fresh = fixture(LAYOUTS[0]).await;
    assert!(replay(&fresh).may_proceed(), "no log at all");

    let (fx, _) = state_at(Phase::Migrating).await;
    migrate_badly(&fx);
    let ReplayOutcome::Restored { from, .. } = replay(&fx) else {
        panic!()
    };
    assert_eq!(from, Phase::Migrating);
    let ReplayOutcome::Restored { from, .. } = replay(&fx) else {
        panic!()
    };
    assert_eq!(from, Phase::Restored, "second start only re-checks");
}

/// Carry-over (5f): a pre-5f `Restoring` + `last_error` log is a restore that
/// already failed. It docks (nothing touched) instead of resuming.
#[tokio::test]
async fn restoring_with_last_error_docks_after_restart_and_touches_nothing() {
    let (fx, mut log) = state_at(Phase::Migrating).await;
    migrate_badly(&fx);
    advance(&RealOps, &fx.root, &mut log, Phase::Restoring).unwrap();
    log.last_error = Some("earlier failure".into());
    guard_log::persist(&RealOps, &fx.root, &log).unwrap();
    let dirty = pair(&fx.store, &fx.memory);
    for _ in 0..2 {
        assert!(matches!(
            replay(&fx),
            ReplayOutcome::Docked(Dock::RollbackFailed { persisted: true, ref reason })
                if reason == "earlier failure"
        ));
        assert_eq!(pair(&fx.store, &fx.memory), dirty, "nothing may be touched");
    }
    assert!(!fx.root.join("migration-quarantine").exists());
}

/// The same crash without a recorded error is just an interrupted restore.
#[tokio::test]
async fn restoring_without_error_is_still_resumed() {
    let (fx, mut log) = state_at(Phase::Migrating).await;
    migrate_badly(&fx);
    advance(&RealOps, &fx.root, &mut log, Phase::Restoring).unwrap();
    assert!(matches!(
        replay(&fx),
        ReplayOutcome::Restored {
            from: Phase::Restoring,
            ..
        }
    ));
}

#[tokio::test]
async fn rollback_failed_phase_docks_and_is_never_retried() {
    let (fx, mut log) = state_at(Phase::Migrating).await;
    migrate_badly(&fx);
    advance(&RealOps, &fx.root, &mut log, Phase::Restoring).unwrap();
    let failed = log.failed_restore("disk full".into()).unwrap();
    guard_log::persist(&RealOps, &fx.root, &failed).unwrap();
    let dirty = pair(&fx.store, &fx.memory);
    for _ in 0..2 {
        let out = replay(&fx);
        assert!(!out.may_proceed());
        assert!(matches!(
            out,
            ReplayOutcome::Docked(Dock::RollbackFailed { persisted: true, ref reason })
                if reason == "disk full"
        ));
        assert_eq!(pair(&fx.store, &fx.memory), dirty);
        assert_eq!(
            guard_log::read(&fx.root).unwrap().unwrap().phase,
            Phase::RollbackFailed
        );
    }
}

#[tokio::test]
async fn replay_docks_on_unusable_guard_and_touches_nothing() {
    let (fx, _) = state_at(Phase::Migrating).await;
    migrate_badly(&fx);
    let dirty = pair(&fx.store, &fx.memory);
    let g = guard_log::guard_path(&fx.root);
    for bad in [&b"{trunc"[..], b"", b"\xff\xfe"] {
        fs::write(&g, bad).unwrap();
        assert!(matches!(
            replay(&fx),
            ReplayOutcome::Docked(Dock::GuardUnusable(_))
        ));
        assert_eq!(pair(&fx.store, &fx.memory), dirty);
    }
    // present but unreadable is not "no log"
    fs::remove_file(&g).unwrap();
    fs::create_dir(&g).unwrap();
    assert!(matches!(
        replay(&fx),
        ReplayOutcome::Docked(Dock::GuardUnusable(_))
    ));
    fs::remove_dir(&g).unwrap();
    std::os::unix::fs::symlink(fx.root.join("nowhere"), &g).unwrap();
    assert!(matches!(
        replay(&fx),
        ReplayOutcome::Docked(Dock::GuardUnusable(_))
    ));
}

#[tokio::test]
async fn replay_docks_when_snapshot_is_really_corrupt() {
    let (fx, log) = state_at(Phase::Migrating).await;
    migrate_badly(&fx);
    let dirty = pair(&fx.store, &fx.memory);
    let sd = fx
        .root
        .join("migration-snapshots")
        .join(log.snapshot_id.unwrap());
    fs::write(sd.join(MEMORY_FILE), b"garbage").unwrap();
    assert!(matches!(
        replay(&fx),
        ReplayOutcome::Docked(Dock::RollbackFailed {
            persisted: true,
            ..
        })
    ));
    assert_eq!(pair(&fx.store, &fx.memory), dirty, "nothing may be touched");
    assert!(!replay(&fx).may_proceed());
    // The failure is now durable as RollbackFailed: a restart docks on it.
    assert_eq!(
        guard_log::read(&fx.root).unwrap().unwrap().phase,
        Phase::RollbackFailed
    );
}
