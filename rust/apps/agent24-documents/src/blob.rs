//! Content-addressed blob store (ADR-DOC-02 §2).
//!
//! Layout under the OS data dir:
//!
//! ```text
//! blobs/sha256/<first 2 hex>/<remaining 62 hex>   immutable content
//! blobs/tmp/                                      in-flight writes only
//! ```
//!
//! Durability order for a new blob (§2.2): write to `blobs/tmp/` on the same
//! file system → fsync the file → publish it at its content address → fsync
//! the shard directory and its parent. The database row that references a
//! blob is written only after `put` returns, so a crash leaves at most an
//! unreferenced blob or a tmp file, never a reference to missing bytes. On
//! Apple platforms `sync_all` issues `F_FULLFSYNC`.
//!
//! Publishing uses `hard_link`, which fails if the address already exists, so
//! a stored blob is never replaced — not by a concurrent writer of the same
//! bytes, and not by another process. Both directory fsyncs run on every
//! successful `put`, including a dedup hit, so a caller never returns before
//! the entry it relies on is durable, even if an earlier writer of the same
//! content stopped between linking and syncing.
//!
//! `data_dir` must be on a file system that supports hard links (APFS, ext4,
//! …); on one that does not (exFAT, some network mounts) `put` fails rather
//! than falling back to a replacing rename.
//!
//! One `BlobStore` per data dir: `open` takes an exclusive lock on
//! `blobs/.lock` for the store's lifetime, so startup recovery cannot delete
//! another live instance's in-flight writes.
//!
//! The API is synchronous; async callers run it on `spawn_blocking`.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};

/// A stored blob: `sha256:<64 lowercase hex>` plus its length in bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobRef {
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum BlobError {
    #[error("not a content address: {0:?}")]
    InvalidAddress(String),
    #[error("blob not found: {0}")]
    NotFound(String),
    #[error("blob {address} is corrupt: {actual}")]
    Corrupt { address: String, actual: String },
    #[error("the blob store is already open in another instance or process")]
    Locked,
    #[error("blob store i/o: {0}")]
    Io(#[from] io::Error),
}

pub struct BlobStore {
    objects: PathBuf,
    tmp: PathBuf,
    /// Held (locked) for the store's lifetime; released when dropped.
    _lock: File,
}

/// Removes the in-flight tmp file on every exit path. After a successful
/// publish the bytes stay at their content address through the hard link.
struct TmpGuard<'a>(&'a Path);

impl Drop for TmpGuard<'_> {
    fn drop(&mut self) {
        if let Err(e) = fs::remove_file(self.0)
            && e.kind() != io::ErrorKind::NotFound
        {
            // Startup recovery removes it on the next open.
            tracing::warn!(path = %self.0.display(), error = %e, "blob store: could not remove tmp file");
        }
    }
}

const PREFIX: &str = "sha256:";

/// Parse `sha256:<64 lowercase hex>`. Anything else is refused before it can
/// become part of a path.
fn parse_address(address: &str) -> Result<&str, BlobError> {
    let hex = address
        .strip_prefix(PREFIX)
        .ok_or_else(|| BlobError::InvalidAddress(address.to_owned()))?;
    let ok = hex.len() == 64
        && hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if ok {
        Ok(hex)
    } else {
        Err(BlobError::InvalidAddress(address.to_owned()))
    }
}

fn to_hex(digest: &[u8]) -> String {
    use std::fmt::Write as _;
    digest.iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Unix only, like the OS itself (ADR-DOC-01 D2/D10): on Windows, opening a
/// directory needs `FILE_FLAG_BACKUP_SEMANTICS`, to be added with Windows support.
fn fsync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

impl BlobStore {
    /// Open (creating if needed) the store under `data_dir`, take its lock,
    /// and remove leftover in-flight writes from a previous run (§7 crash
    /// recovery). The kernel creates `data_dir` itself; this makes the
    /// store's own directories durable before any `put` can rely on them.
    pub fn open(data_dir: &Path) -> Result<Self, BlobError> {
        let root = data_dir.join("blobs");
        let objects = root.join("sha256");
        let tmp = root.join("tmp");
        fs::create_dir_all(&objects)?;
        fs::create_dir_all(&tmp)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(root.join(".lock"))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => return Err(BlobError::Locked),
            Err(std::fs::TryLockError::Error(e)) => return Err(BlobError::Io(e)),
        }
        fsync_dir(&root)?;
        fsync_dir(data_dir)?;
        let store = Self {
            objects,
            tmp,
            _lock: lock,
        };
        store.clear_tmp()?;
        Ok(store)
    }

    fn clear_tmp(&self) -> io::Result<()> {
        for entry in fs::read_dir(&self.tmp)? {
            let path = entry?.path();
            if path.is_dir() {
                fs::remove_dir_all(&path)?;
            } else {
                fs::remove_file(&path)?;
            }
        }
        Ok(())
    }

    fn object_path(&self, hex: &str) -> (PathBuf, PathBuf) {
        let shard = self.objects.join(&hex[..2]);
        let file = shard.join(&hex[2..]);
        (shard, file)
    }

    fn tmp_path(&self) -> PathBuf {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        self.tmp.join(format!(
            "{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ))
    }

    /// Store everything `reader` yields. Identical content is stored once and
    /// never rewritten.
    ///
    /// When the address already exists, the stored file is kept and only its
    /// length is checked (a mismatch is reported as [`BlobError::Corrupt`]);
    /// its bytes are not re-hashed. Callers that need that guarantee read with
    /// [`BlobStore::read_verified`].
    pub fn put(&self, mut reader: impl Read) -> Result<BlobRef, BlobError> {
        let tmp = self.tmp_path();
        let mut file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        let _guard = TmpGuard(&tmp);
        let mut hasher = Sha256::new();
        let mut size = 0u64;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(BlobError::Io(e)),
            };
            hasher.update(&buf[..n]);
            file.write_all(&buf[..n])?;
            size += n as u64;
        }
        file.sync_all()?;
        drop(file);
        let hex = to_hex(&hasher.finalize());
        let (shard, path) = self.object_path(&hex);
        fs::create_dir_all(&shard)?;
        match fs::hard_link(&tmp, &path) {
            Ok(()) => {}
            // Already stored (by us earlier, a concurrent writer, or another
            // process): keep the existing file untouched, but do not report
            // success over a stored file that cannot be this content.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                let stored = fs::metadata(&path)?.len();
                if stored != size {
                    return Err(BlobError::Corrupt {
                        address: format!("{PREFIX}{hex}"),
                        actual: format!("a stored file of {stored} bytes, expected {size}"),
                    });
                }
            }
            Err(e) => return Err(BlobError::Io(e)),
        }
        fsync_dir(&shard)?;
        fsync_dir(&self.objects)?;
        Ok(BlobRef {
            sha256: format!("{PREFIX}{hex}"),
            size,
        })
    }

    pub fn put_bytes(&self, bytes: &[u8]) -> Result<BlobRef, BlobError> {
        self.put(bytes)
    }

    pub fn contains(&self, address: &str) -> Result<bool, BlobError> {
        let hex = parse_address(address)?;
        Ok(self.object_path(hex).1.is_file())
    }

    /// Where a stored blob's file is, for an engine that reads it by path.
    /// Blob files are never rewritten, so the path stays valid while the
    /// blob is referenced.
    pub fn path_of(&self, address: &str) -> Result<PathBuf, BlobError> {
        let path = self.object_path(parse_address(address)?).1;
        if path.is_file() {
            Ok(path)
        } else {
            Err(BlobError::NotFound(address.to_owned()))
        }
    }

    /// Open a blob for streaming. The caller gets the bytes as stored; use
    /// [`BlobStore::read_verified`] when the content must be re-checked.
    pub fn open_blob(&self, address: &str) -> Result<File, BlobError> {
        let hex = parse_address(address)?;
        File::open(self.object_path(hex).1).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => BlobError::NotFound(address.to_owned()),
            _ => BlobError::Io(e),
        })
    }

    /// Read a whole blob and check that it still hashes to its address.
    pub fn read_verified(&self, address: &str) -> Result<Vec<u8>, BlobError> {
        let mut bytes = Vec::new();
        self.open_blob(address)?.read_to_end(&mut bytes)?;
        let actual = format!("{PREFIX}{}", to_hex(&Sha256::digest(&bytes)));
        if actual != address {
            return Err(BlobError::Corrupt {
                address: address.to_owned(),
                actual: format!("content hashes to {actual}"),
            });
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    const HELLO: &str = "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

    fn files_under(dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for entry in fs::read_dir(dir).unwrap() {
            let p = entry.unwrap().path();
            if p.is_dir() {
                out.extend(files_under(&p));
            } else {
                out.push(p);
            }
        }
        out
    }

    #[test]
    fn put_stores_under_the_content_address_and_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::open(dir.path()).unwrap();
        let r = store.put_bytes(b"hello").unwrap();
        assert_eq!(
            r,
            BlobRef {
                sha256: HELLO.into(),
                size: 5
            }
        );
        let expected = dir.path().join("blobs/sha256/2c").join(&HELLO[7 + 2..]);
        assert!(expected.is_file(), "layout is blobs/sha256/<2>/<62>");
        assert_eq!(store.read_verified(HELLO).unwrap(), b"hello");
        assert!(files_under(&dir.path().join("blobs/tmp")).is_empty());
    }

    #[test]
    fn identical_content_is_stored_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::open(dir.path()).unwrap();
        let a = store.put_bytes(b"same bytes").unwrap();
        let b = store.put(&b"same bytes"[..]).unwrap();
        assert_eq!(a, b);
        assert_eq!(files_under(&dir.path().join("blobs/sha256")).len(), 1);
        assert!(files_under(&dir.path().join("blobs/tmp")).is_empty());
    }

    /// §2.2 "skip the write when the hash exists": an existing blob may be
    /// open in another request, so it is never replaced, not even by equal bytes.
    #[cfg(unix)]
    #[test]
    fn storing_existing_content_does_not_replace_the_stored_file() {
        use std::os::unix::fs::MetadataExt;
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::open(dir.path()).unwrap();
        store.put_bytes(b"hello").unwrap();
        let path = dir.path().join("blobs/sha256/2c").join(&HELLO[7 + 2..]);
        let before = fs::metadata(&path).unwrap().ino();
        store.put_bytes(b"hello").unwrap();
        assert_eq!(fs::metadata(&path).unwrap().ino(), before);
    }

    /// Codex review of #21: two writers that both see the address as absent
    /// must not both publish; the first stored file stays in place.
    #[cfg(unix)]
    #[test]
    fn concurrent_writers_of_the_same_content_never_replace_the_stored_file() {
        use std::os::unix::fs::MetadataExt;
        use std::sync::{Arc, Barrier};
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(BlobStore::open(dir.path()).unwrap());
        let content = vec![7u8; 256 * 1024];
        let first = store.put_bytes(&content).unwrap();
        let path = dir
            .path()
            .join("blobs/sha256")
            .join(&first.sha256[7..9])
            .join(&first.sha256[9..]);
        let inode = fs::metadata(&path).unwrap().ino();
        let barrier = Arc::new(Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let (store, barrier, content) =
                    (Arc::clone(&store), Arc::clone(&barrier), content.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    store.put_bytes(&content).unwrap()
                })
            })
            .collect();
        for h in handles {
            assert_eq!(h.join().unwrap(), first);
        }
        assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
        assert_eq!(files_under(&dir.path().join("blobs/sha256")).len(), 1);
        assert!(files_under(&dir.path().join("blobs/tmp")).is_empty());
    }

    /// A failure after the tmp file is complete (here: the shard path is a
    /// regular file, so the shard directory cannot be created) still removes it.
    /// #804 review: a dedup hit must not report success over a stored file
    /// that cannot hold this content.
    #[test]
    fn a_dedup_hit_on_a_truncated_stored_file_is_reported_as_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::open(dir.path()).unwrap();
        store.put_bytes(b"hello").unwrap();
        let path = dir.path().join("blobs/sha256/2c").join(&HELLO[7 + 2..]);
        fs::write(&path, b"hel").unwrap();
        assert!(matches!(
            store.put_bytes(b"hello"),
            Err(BlobError::Corrupt { .. })
        ));
        assert!(files_under(&dir.path().join("blobs/tmp")).is_empty());
    }

    #[test]
    fn a_failure_after_the_tmp_file_is_written_leaves_no_tmp_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::open(dir.path()).unwrap();
        fs::write(dir.path().join("blobs/sha256/2c"), b"not a directory").unwrap();
        assert!(store.put_bytes(b"hello").is_err());
        assert!(files_under(&dir.path().join("blobs/tmp")).is_empty());
    }

    #[test]
    fn a_second_live_instance_cannot_open_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let first = BlobStore::open(dir.path()).unwrap();
        assert!(matches!(
            BlobStore::open(dir.path()),
            Err(BlobError::Locked)
        ));
        drop(first);
        assert!(
            BlobStore::open(dir.path()).is_ok(),
            "the lock is released on drop"
        );
    }

    #[test]
    fn open_clears_leftover_tmp_writes_and_never_promotes_them() {
        let dir = tempfile::tempdir().unwrap();
        drop(BlobStore::open(dir.path()).unwrap());
        // A crash between "write tmp" and the hard_link publish leaves this behind.
        fs::write(dir.path().join("blobs/tmp/123-0"), b"half a file").unwrap();
        let store = BlobStore::open(dir.path()).unwrap();
        assert!(files_under(&dir.path().join("blobs/tmp")).is_empty());
        assert!(files_under(&dir.path().join("blobs/sha256")).is_empty());
        assert!(!store.contains(HELLO).unwrap());
    }

    #[test]
    fn addresses_that_are_not_sha256_hex_are_refused_before_touching_the_disk() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::open(dir.path()).unwrap();
        for bad in [
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
            "sha256:../../../../etc/passwd",
            "sha256:2CF24DBA5FB0A30E26E83B2AC5B9E29E1B161E5C1FA7425E73043362938B9824",
            "sha256:2cf24d",
            "md5:5d41402abc4b2a76b9719d911017c592",
        ] {
            assert!(
                matches!(store.open_blob(bad), Err(BlobError::InvalidAddress(_))),
                "{bad} must be refused"
            );
        }
    }

    #[test]
    fn missing_and_corrupt_blobs_are_reported_as_such() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::open(dir.path()).unwrap();
        assert!(matches!(
            store.open_blob(HELLO),
            Err(BlobError::NotFound(_))
        ));
        store.put_bytes(b"hello").unwrap();
        let path = dir.path().join("blobs/sha256/2c").join(&HELLO[7 + 2..]);
        fs::write(&path, b"tampered").unwrap();
        assert!(matches!(
            store.read_verified(HELLO),
            Err(BlobError::Corrupt { .. })
        ));
    }

    #[test]
    fn a_failing_reader_leaves_no_tmp_file_and_no_blob() {
        struct Broken(u8);
        impl Read for Broken {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if self.0 == 0 {
                    return Err(io::Error::other("disk on fire"));
                }
                self.0 -= 1;
                buf[0] = b'x';
                Ok(1)
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::open(dir.path()).unwrap();
        assert!(store.put(Broken(3)).is_err());
        assert!(files_under(&dir.path().join("blobs/tmp")).is_empty());
        assert!(files_under(&dir.path().join("blobs/sha256")).is_empty());
    }
}
