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
/// Move detection: a tracked path that vanished, whose `db_checksum` equals the
/// bytes of a NEW file at another path, is a [`Reconciliation::Moved`] — matched
/// one-to-one and greedily by checksum, so a rename is not misread as a delete
/// plus an unrelated create. Duplicate checksums are matched in sorted order for
/// determinism.
pub fn reconcile(tracked: &[(String, String)], observed: &[ObservedFile]) -> Vec<Reconciliation> {
    let tracked_by_path: BTreeMap<&str, &str> = tracked
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_str()))
        .collect();
    let observed_by_path: BTreeMap<&str, &str> = observed
        .iter()
        .map(|o| (o.path.as_str(), o.checksum.as_str()))
        .collect();

    let mut out = Vec::new();
    // Gone-from-disk tracked entries and new-on-disk observed entries, collected
    // first so move-detection can pair them by checksum.
    let mut gone: Vec<(&str, &str)> = Vec::new(); // (path, db_checksum)
    let mut fresh: Vec<(&str, &str)> = Vec::new(); // (path, file_checksum)

    for (path, db_sum) in &tracked_by_path {
        match observed_by_path.get(path) {
            Some(file_sum) if file_sum == db_sum => {
                out.push(Reconciliation::Unchanged {
                    path: (*path).to_owned(),
                });
            }
            Some(file_sum) => out.push(Reconciliation::ModifiedOnDisk {
                path: (*path).to_owned(),
                db_checksum: (*db_sum).to_owned(),
                file_checksum: (*file_sum).to_owned(),
            }),
            None => gone.push((path, db_sum)),
        }
    }
    for (path, file_sum) in &observed_by_path {
        if !tracked_by_path.contains_key(path) {
            fresh.push((path, file_sum));
        }
    }

    // Pair a gone entry with a fresh entry of the SAME checksum → a move. Both
    // vecs are already in sorted (BTreeMap) order, so pairing is deterministic.
    let mut fresh_used = vec![false; fresh.len()];
    for (from_path, db_sum) in &gone {
        let matched = fresh
            .iter()
            .enumerate()
            .find(|(i, (_, fsum))| !fresh_used[*i] && fsum == db_sum);
        if let Some((i, (to_path, _))) = matched {
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
        // symlink_metadata does NOT follow the link, so a symlink is seen as a
        // symlink and refused rather than dereferenced.
        let meta = entry
            .metadata()
            .map_err(|e| MemoryError::Io(format!("metadata: {e}")))?;
        let ft = entry
            .file_type()
            .map_err(|e| MemoryError::Io(format!("file_type: {e}")))?;
        if ft.is_symlink() {
            return Err(MemoryError::Io(format!(
                "refusing to follow symlink in artifact dir: {:?}",
                entry.file_name()
            )));
        }
        if !meta.is_file() {
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
    fn genuine_delete_and_unrelated_new_are_not_paired() {
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
