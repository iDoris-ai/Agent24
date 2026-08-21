//! MD-2c: dual-lineage reconciliation — compare the [`ArtifactStore`]'s DB
//! lineage against what is actually on disk, and classify every divergence
//! WITHOUT ever silently deleting (SPEC-MD-ME §3 MD-2 acceptance: "外部改文件→
//! 对账不静默删"; borrowed from basic-memory's two-lineage version ledger +
//! checksum move-detection).
//!
//! MD-2b's [`crate::artifact::Artifact`] carries two checksums: `db_checksum`
//! (what the DB authoritatively holds) and `file_checksum` (what the DB last
//! observed on disk). MD-2b kept them equal at write time; THIS module is where
//! they diverge — a file edited, moved, or removed outside the store no longer
//! matches its `db_checksum`. [`reconcile`] turns the two lineages into a list of
//! [`Reconciliation`] actions the caller decides on. Crucially it never emits a
//! "delete the record" action: a file gone from disk is FLAGGED
//! ([`Reconciliation::DeletedOnDisk`]), never silently applied.
//!
//! The classifier is a PURE function over `(tracked, observed)`, so the four
//! states are tested without touching a filesystem; [`observe_dir`] is the thin,
//! path-safe adapter that produces the `observed` side from a real directory.

use std::collections::BTreeMap;
use std::path::Path;

use crate::artifact::{ArtifactCas, checksum};
use crate::{MemoryError, Result};

/// A file seen on disk during reconciliation: its store-relative path and the
/// checksum of its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedFile {
    pub path: String,
    pub checksum: String,
}

/// One reconciliation outcome. There is deliberately NO variant that removes a
/// record: a vanished file is [`DeletedOnDisk`](Reconciliation::DeletedOnDisk),
/// an advisory flag the caller acts on — never a silent delete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reconciliation {
    /// On disk, checksum matches the DB — nothing to do.
    Unchanged { path: String },
    /// Same path, but the file's bytes changed outside the store. Re-ingest is
    /// the caller's call; the DB record is NOT touched here.
    ModifiedOnDisk {
        path: String,
        db_checksum: String,
        file_checksum: String,
    },
    /// A tracked file's content now lives at a NEW path (its `db_checksum` matches
    /// a new file's bytes). Detected by checksum so the artifact's id/history are
    /// preserved across the move instead of read as delete-then-create.
    ///
    /// This is an INFERENCE, only made when the content is GLOBALLY UNIQUE — the
    /// checksum appears exactly once in `tracked` and once in `observed` (review
    /// #118). Identical content is common (empty files, templates), and guessing
    /// a move there would (a) hide a real delete behind a fake move and (b) risk
    /// attaching one file's history to an unrelated file. So when a checksum has
    /// more than one candidate on either side, no move is inferred: the gone side
    /// is reported `DeletedOnDisk` and the new side `NewOnDisk`, truthfully. Even
    /// in the unique 1-to-1 case a genuine delete + an unrelated same-content
    /// create is INDISTINGUISHABLE from a move by content alone, so a consumer
    /// must treat `Moved` as a best guess, not a certainty.
    Moved {
        from_path: String,
        to_path: String,
        checksum: String,
    },
    /// A tracked path has no file on disk. FLAGGED, never auto-deleted — the whole
    /// point of the no-silent-delete rule.
    DeletedOnDisk { path: String, db_checksum: String },
    /// A file on disk the DB does not track — a candidate to ingest.
    NewOnDisk { path: String, file_checksum: String },
}

impl Reconciliation {
    fn sort_key(&self) -> (&str, u8) {
        match self {
            Reconciliation::Unchanged { path } => (path, 0),
            Reconciliation::ModifiedOnDisk { path, .. } => (path, 1),
            Reconciliation::Moved { from_path, .. } => (from_path, 2),
            Reconciliation::DeletedOnDisk { path, .. } => (path, 3),
            Reconciliation::NewOnDisk { path, .. } => (path, 4),
        }
    }
}

/// Classify every divergence between the DB lineage (`tracked`: `(path,
/// db_checksum)`) and the disk lineage (`observed`). Pure and DETERMINISTIC: the
/// result is sorted, so the same inputs always yield the same actions (the
/// `memory rebuild` determinism the acceptance asks for).
///
/// Move detection: a tracked path that vanished, whose `db_checksum` equals a NEW
/// file's bytes, is a [`Reconciliation::Moved`] ONLY when that content is
/// GLOBALLY UNIQUE — its checksum occurs exactly once in `tracked` AND once in
/// `observed`. When content is duplicated, no move is guessed: the gone side is
/// `DeletedOnDisk` and the new side `NewOnDisk`, so a real delete is never hidden
/// behind a fabricated move and no file's history is attached to the wrong file
/// (review #118).
pub fn reconcile(tracked: &[(String, String)], observed: &[ObservedFile]) -> Vec<Reconciliation> {
    let tracked_by_path: BTreeMap<&str, &str> = tracked
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_str()))
        .collect();
    let observed_by_path: BTreeMap<&str, &str> = observed
        .iter()
        .map(|o| (o.path.as_str(), o.checksum.as_str()))
        .collect();

    // How many times each checksum occurs on each side, over ALL entries — the
    // uniqueness test that gates a move inference.
    let mut tracked_count: BTreeMap<&str, usize> = BTreeMap::new();
    for c in tracked_by_path.values() {
        *tracked_count.entry(*c).or_default() += 1;
    }
    let mut observed_count: BTreeMap<&str, usize> = BTreeMap::new();
    for c in observed_by_path.values() {
        *observed_count.entry(*c).or_default() += 1;
    }

    let mut out = Vec::new();
    let mut gone: Vec<(&str, &str)> = Vec::new(); // (path, db_checksum)

    for (path, db_sum) in &tracked_by_path {
        match observed_by_path.get(path) {
            Some(file_sum) if file_sum == db_sum => out.push(Reconciliation::Unchanged {
                path: (*path).to_owned(),
            }),
            Some(file_sum) => out.push(Reconciliation::ModifiedOnDisk {
                path: (*path).to_owned(),
                db_checksum: (*db_sum).to_owned(),
                file_checksum: (*file_sum).to_owned(),
            }),
            None => gone.push((path, db_sum)),
        }
    }
    // Fresh = observed paths the DB doesn't track. Track which are consumed by a
    // move so they aren't also reported NewOnDisk.
    let fresh: Vec<(&str, &str)> = observed_by_path
        .iter()
        .filter(|(path, _)| !tracked_by_path.contains_key(*path))
        .map(|(p, c)| (*p, *c))
        .collect();
    let mut fresh_used = vec![false; fresh.len()];

    for (from_path, db_sum) in &gone {
        // A move is inferred only for globally-unique content. tracked_count==1 is
        // guaranteed (this gone path holds it) but assert it for clarity; the
        // real gate is observed_count==1 AND a fresh landing spot exists.
        let unique = tracked_count.get(db_sum).copied().unwrap_or(0) == 1
            && observed_count.get(db_sum).copied().unwrap_or(0) == 1;
        let landing = if unique {
            fresh
                .iter()
                .enumerate()
                .find(|(i, (_, fsum))| !fresh_used[*i] && fsum == db_sum)
        } else {
            None
        };
        if let Some((i, (to_path, _))) = landing {
            fresh_used[i] = true;
            out.push(Reconciliation::Moved {
                from_path: (*from_path).to_owned(),
                to_path: (*to_path).to_owned(),
                checksum: (*db_sum).to_owned(),
            });
        } else {
            out.push(Reconciliation::DeletedOnDisk {
                path: (*from_path).to_owned(),
                db_checksum: (*db_sum).to_owned(),
            });
        }
    }
    for (i, (path, file_sum)) in fresh.iter().enumerate() {
        if !fresh_used[i] {
            out.push(Reconciliation::NewOnDisk {
                path: (*path).to_owned(),
                file_checksum: (*file_sum).to_owned(),
            });
        }
    }

    out.sort_by(|a, b| a.sort_key().cmp(&b.sort_key()));
    out
}

/// Reconcile an owner's tracked artifacts against an observed disk snapshot.
pub async fn reconcile_store(
    store: &ArtifactCas,
    owner: &str,
    observed: &[ObservedFile],
) -> Result<Vec<Reconciliation>> {
    let tracked = store.tracked_paths(owner).await?;
    Ok(reconcile(&tracked, observed))
}

/// Walk `root` (one level, non-recursive is intentional for the flat artifact
/// namespace) and hash each regular file into an [`ObservedFile`] whose `path` is
/// the file name. PATH-SAFE (borrowed from codex): symlinks are refused, not
/// followed, so a symlink cannot smuggle content from outside `root` into the
/// reconciled set. Returns entries sorted by path for determinism.
pub fn observe_dir(root: &Path) -> Result<Vec<ObservedFile>> {
    let mut out = Vec::new();
    let entries = std::fs::read_dir(root)
        .map_err(|e| MemoryError::Io(format!("read_dir {}: {e}", root.display())))?;
    for entry in entries {
        let entry = entry.map_err(|e| MemoryError::Io(format!("dir entry: {e}")))?;
        // `DirEntry::file_type()` reports the entry's OWN type without following
        // the link (unlike `fs::metadata`), so a symlink is seen as a symlink and
        // refused BEFORE any dereference — even a dangling one.
        let ft = entry
            .file_type()
            .map_err(|e| MemoryError::Io(format!("file_type: {e}")))?;
        if ft.is_symlink() {
            return Err(MemoryError::Io(format!(
                "refusing to follow symlink in artifact dir: {:?}",
                entry.file_name()
            )));
        }
        if !ft.is_file() {
            continue;
        }
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| MemoryError::Io("non-UTF8 file name".to_owned()))?;
        let bytes = std::fs::read(entry.path())
            .map_err(|e| MemoryError::Io(format!("read {name}: {e}")))?;
        let text = String::from_utf8(bytes)
            .map_err(|_| MemoryError::Io(format!("non-UTF8 file body: {name}")))?;
        out.push(ObservedFile {
            path: name,
            checksum: checksum(&text),
        });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::KvStore;
    use crate::artifact::{Artifact, ArtifactStore};
    use crate::event::Scope;

    fn tracked(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(p, c)| (p.to_string(), c.to_string()))
            .collect()
    }
    fn observed(pairs: &[(&str, &str)]) -> Vec<ObservedFile> {
        pairs
            .iter()
            .map(|(p, c)| ObservedFile {
                path: p.to_string(),
                checksum: c.to_string(),
            })
            .collect()
    }

    #[test]
    fn unchanged_when_checksums_match() {
        let r = reconcile(&tracked(&[("a.md", "h1")]), &observed(&[("a.md", "h1")]));
        assert_eq!(
            r,
            vec![Reconciliation::Unchanged {
                path: "a.md".into()
            }]
        );
    }

    #[test]
    fn modified_in_place_when_same_path_different_checksum() {
        let r = reconcile(&tracked(&[("a.md", "h1")]), &observed(&[("a.md", "h2")]));
        assert_eq!(
            r,
            vec![Reconciliation::ModifiedOnDisk {
                path: "a.md".into(),
                db_checksum: "h1".into(),
                file_checksum: "h2".into(),
            }]
        );
    }

    #[test]
    fn moved_when_checksum_reappears_at_new_path() {
        // a.md (h1) gone; b.md (h1) new → a MOVE, not delete + create.
        let r = reconcile(&tracked(&[("a.md", "h1")]), &observed(&[("b.md", "h1")]));
        assert_eq!(
            r,
            vec![Reconciliation::Moved {
                from_path: "a.md".into(),
                to_path: "b.md".into(),
                checksum: "h1".into(),
            }]
        );
    }

    #[test]
    fn deleted_on_disk_is_flagged_never_silently_removed() {
        let r = reconcile(&tracked(&[("a.md", "h1")]), &observed(&[]));
        // The ONLY signal for a vanished file is the advisory flag — there is no
        // "remove record" action in the result type at all.
        assert_eq!(
            r,
            vec![Reconciliation::DeletedOnDisk {
                path: "a.md".into(),
                db_checksum: "h1".into(),
            }]
        );
    }

    #[test]
    fn new_on_disk_when_untracked_file_appears() {
        let r = reconcile(&tracked(&[]), &observed(&[("new.md", "h9")]));
        assert_eq!(
            r,
            vec![Reconciliation::NewOnDisk {
                path: "new.md".into(),
                file_checksum: "h9".into(),
            }]
        );
    }

    #[test]
    fn move_is_not_misread_as_delete_plus_create() {
        // Distinguishes MD-2c's whole point: the same content at a new path is one
        // Moved, not a DeletedOnDisk + a NewOnDisk.
        let r = reconcile(
            &tracked(&[("old.md", "hx")]),
            &observed(&[("new.md", "hx")]),
        );
        assert_eq!(r.len(), 1);
        assert!(matches!(r[0], Reconciliation::Moved { .. }));
        assert!(
            !r.iter()
                .any(|x| matches!(x, Reconciliation::DeletedOnDisk { .. }))
        );
        assert!(
            !r.iter()
                .any(|x| matches!(x, Reconciliation::NewOnDisk { .. }))
        );
    }

    #[test]
    fn different_checksums_are_a_real_delete_plus_a_real_new() {
        // Different checksums → NOT a move: a real delete + a real new.
        let r = reconcile(
            &tracked(&[("gone.md", "h1")]),
            &observed(&[("fresh.md", "h2")]),
        );
        assert!(r.contains(&Reconciliation::DeletedOnDisk {
            path: "gone.md".into(),
            db_checksum: "h1".into(),
        }));
        assert!(r.contains(&Reconciliation::NewOnDisk {
            path: "fresh.md".into(),
            file_checksum: "h2".into(),
        }));
    }

    #[test]
    fn duplicated_content_across_tracked_is_not_confidently_paired() {
        // Review #118 case 2: a.md and b.md have the SAME content; a.md vanishes,
        // b.md stays, c.md (same content) appears. b→c is as plausible as a→c, so
        // no move is guessed: a is DeletedOnDisk, c is NewOnDisk, b is Unchanged.
        let r = reconcile(
            &tracked(&[("a.md", "S"), ("b.md", "S")]),
            &observed(&[("b.md", "S"), ("c.md", "S")]),
        );
        assert!(r.contains(&Reconciliation::Unchanged {
            path: "b.md".into()
        }));
        assert!(r.contains(&Reconciliation::DeletedOnDisk {
            path: "a.md".into(),
            db_checksum: "S".into(),
        }));
        assert!(r.contains(&Reconciliation::NewOnDisk {
            path: "c.md".into(),
            file_checksum: "S".into(),
        }));
        assert!(!r.iter().any(|x| matches!(x, Reconciliation::Moved { .. })));
    }

    #[test]
    fn one_gone_two_same_content_new_is_not_paired() {
        // Review #118 case 3: which of x/y did a move to? Ambiguous → no guess.
        let r = reconcile(
            &tracked(&[("a.md", "S")]),
            &observed(&[("x.md", "S"), ("y.md", "S")]),
        );
        assert!(r.contains(&Reconciliation::DeletedOnDisk {
            path: "a.md".into(),
            db_checksum: "S".into(),
        }));
        assert_eq!(
            r.iter()
                .filter(|x| matches!(x, Reconciliation::NewOnDisk { .. }))
                .count(),
            2
        );
        assert!(!r.iter().any(|x| matches!(x, Reconciliation::Moved { .. })));
    }

    #[test]
    fn unique_content_1to1_still_infers_a_move_as_a_documented_best_guess() {
        // Review #118 case 1: unique content, 1-to-1. A genuine delete + an
        // unrelated create with identical bytes is INDISTINGUISHABLE from a move
        // by content, so we take the move as a best guess (documented on the
        // `Moved` variant). This is the accepted default; the ambiguous cases
        // above are the ones that must NOT be guessed.
        let r = reconcile(&tracked(&[("gone.md", "U")]), &observed(&[("new.md", "U")]));
        assert_eq!(
            r,
            vec![Reconciliation::Moved {
                from_path: "gone.md".into(),
                to_path: "new.md".into(),
                checksum: "U".into(),
            }]
        );
    }

    #[test]
    fn reconcile_is_deterministic_regardless_of_input_order() {
        let t = tracked(&[("b.md", "h2"), ("a.md", "h1")]);
        let o1 = observed(&[("a.md", "h1"), ("b.md", "hX")]);
        let o2 = observed(&[("b.md", "hX"), ("a.md", "h1")]);
        assert_eq!(reconcile(&t, &o1), reconcile(&t, &o2), "order-independent");
    }

    #[tokio::test]
    async fn reconcile_store_reads_the_db_lineage() {
        let store = KvStore::open_memory().await.unwrap();
        let cas = store.artifacts();
        cas.cas_write(
            Artifact::draft("core.md", "hello", Scope::owner("u1"), "a", "w"),
            0,
        )
        .await
        .unwrap();
        // Disk still matches → Unchanged.
        let obs = vec![ObservedFile {
            path: "core.md".into(),
            checksum: checksum("hello"),
        }];
        let r = reconcile_store(&cas, "u1", &obs).await.unwrap();
        assert_eq!(
            r,
            vec![Reconciliation::Unchanged {
                path: "core.md".into()
            }]
        );

        // Disk edited outside the store → ModifiedOnDisk, DB untouched.
        let obs = vec![ObservedFile {
            path: "core.md".into(),
            checksum: checksum("hello EDITED"),
        }];
        let r = reconcile_store(&cas, "u1", &obs).await.unwrap();
        assert!(matches!(r[0], Reconciliation::ModifiedOnDisk { .. }));
        // The stored artifact is still version 1 with the original body.
        assert_eq!(
            cas.read("core.md", "u1").await.unwrap().unwrap().body,
            "hello"
        );
    }

    #[tokio::test]
    async fn observe_dir_hashes_files_and_refuses_symlinks() {
        let dir = std::env::temp_dir().join(format!("a24mem-obs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.md"), "hello").unwrap();
        let obs = observe_dir(&dir).unwrap();
        assert_eq!(
            obs,
            vec![ObservedFile {
                path: "a.md".into(),
                checksum: checksum("hello"),
            }]
        );

        // A symlink must be refused, not followed.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc/hostname", dir.join("link.md")).unwrap();
            let err = observe_dir(&dir).unwrap_err();
            assert!(matches!(err, MemoryError::Io(_)), "{err}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
