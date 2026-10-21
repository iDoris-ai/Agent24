//! A module package's artifact identity (K1-6a-G §2.1): a digest of exactly
//! the files in a package, for a run to carry from its start, so the same
//! version with a changed file is a different artifact. It proves which
//! files these are, not where they came from (ADR-K1-03 §4).
//!
//! The walk follows nothing: each directory and file is opened from its
//! parent's descriptor, refusing a link, and must be the inode the parent
//! listed. A regular file contributes its relative path, length and
//! SHA-256. A package with a symlink anywhere has no identity (installing
//! never copies one; K1-6a-G §2.1 as revised in A2b): what a link means
//! depends on what it resolves to while the package runs, which no digest
//! of the tree can pin. Nor does a package with anything else that is not a
//! file or directory, an entry that changes or is replaced while it is
//! read, or more than [`MAX_PACKAGE_ENTRIES`](crate::launch::MAX_PACKAGE_ENTRIES)
//! entries, [`MAX_PACKAGE_BYTES`] bytes or [`MAX_PACKAGE_DEPTH`] levels.
//! The caller fails closed.
//!
//! The digest is SHA-256 over the files sorted by relative path bytes
//! (components joined by `/`), each `u64be(len(path)) ‖ path ‖ b'f' ‖
//! u64be(length) ‖ sha256(file)`, written `sha256:` and 64 lowercase hex
//! digits.

use rustix::fs::{AtFlags, FileType, Mode, OFlags, Stat};
use std::ffi::OsStr;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// What every identity starts with.
pub const PREFIX: &str = "sha256:";

/// Most bytes the files of a package may hold and still get an identity:
/// every start reads them all, before the start timeout runs. ⚖️
pub const MAX_PACKAGE_BYTES: u64 = 1 << 30;

/// Deepest directory nesting walked. The walk holds one descriptor per
/// level with directories still to read, so this bounds them too. ⚖️
pub const MAX_PACKAGE_DEPTH: usize = 64;

/// Why a package has no identity.
#[derive(Debug)]
pub enum ArtifactError {
    /// An entry could not be read or inspected.
    Unreadable(PathBuf, std::io::Error),
    /// A symlink, or anything else that is neither a regular file nor a
    /// directory; or the package root is not a directory.
    Unsupported(PathBuf),
    /// An entry changed, or was replaced, while the package was read.
    Changed(PathBuf),
    /// More entries, bytes or levels than a start may read.
    TooLarge,
}

impl std::fmt::Display for ArtifactError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreadable(path, e) => write!(f, "{} could not be read: {e}", path.display()),
            Self::Unsupported(path) => write!(f, "{} is no file or directory", path.display()),
            Self::Changed(path) => write!(f, "{} changed while it was read", path.display()),
            Self::TooLarge => write!(f, "the package has too many entries, bytes or levels"),
        }
    }
}

impl std::error::Error for ArtifactError {}

/// One file: its relative path, and the rest of its encoding (`b'f' ‖ …`,
/// see the module docs).
struct Entry {
    path: Vec<u8>,
    tail: Vec<u8>,
}

impl Entry {
    fn file(rel: &Path, len: u64, digest: &[u8]) -> Self {
        let path = rel.as_os_str().as_bytes().to_vec();
        let tail = [&b"f"[..], &len.to_be_bytes(), digest].concat();
        Self { path, tail }
    }
}

/// A hashed file as it was read: (dev, ino, length, mtime, mtime ns, ctime,
/// ctime ns), compared again once the walk ends. `ctime` moves on any write
/// and, unlike `mtime`, cannot be set back.
type Stamp = (u64, u64, u64, i64, i64, i64, i64);

fn stamp(m: &std::fs::Metadata) -> Stamp {
    use std::os::unix::fs::MetadataExt;
    let (mtime, ctime) = ((m.mtime(), m.mtime_nsec()), (m.ctime(), m.ctime_nsec()));
    (
        m.dev(),
        m.ino(),
        m.len(),
        mtime.0,
        mtime.1,
        ctime.0,
        ctime.1,
    )
}

/// The artifact identity of the package at `package_dir`.
///
/// Blocking: it reads every file. Call it off the async runtime.
///
/// # Errors
///
/// [`ArtifactError`]: see the module docs.
pub fn identity(package_dir: &Path) -> Result<String, ArtifactError> {
    let caps = (crate::launch::MAX_PACKAGE_ENTRIES, MAX_PACKAGE_BYTES);
    Ok(digest(collect(package_dir, caps)?))
}

/// A directory still to read: its parent's descriptor and what the parent
/// listed it as (`None` for the root), its path and its relative path.
type Pending = (Option<(Arc<OwnedFd>, Stat)>, PathBuf, PathBuf);

/// Every file under `root`, within `(entries, bytes)` caps and
/// [`MAX_PACKAGE_DEPTH`]. Directories and files are opened from their
/// parent's descriptor, refusing a link, and must be the inode the parent
/// listed; every hashed file is checked again at the end.
fn collect(
    root: &Path,
    (max_entries, max_bytes): (usize, u64),
) -> Result<Vec<Entry>, ArtifactError> {
    let unreadable =
        |path: &Path, e: rustix::io::Errno| ArtifactError::Unreadable(path.to_owned(), e.into());
    let dir_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut pending: Vec<Pending> = vec![(None, root.to_owned(), PathBuf::new())];
    let (mut found, mut bytes, mut entries, mut stamps) = (0usize, 0u64, Vec::new(), Vec::new());
    let mut dirs = Vec::new();
    while let Some((listed, dir, rel_dir)) = pending.pop() {
        let fd = match listed {
            // The parent's descriptor is dropped here unless a sibling
            // still holds it: one descriptor per level, at most.
            Some((parent, walked)) => {
                #[cfg(test)]
                tests::between_listing_and_opening(&dir);
                let name = dir.file_name().unwrap_or_default();
                let fd = rustix::fs::openat(&*parent, name, dir_flags, Mode::empty())
                    .map_err(|e| open_failed(&dir, e))?;
                if !same_inode(
                    &rustix::fs::fstat(&fd).map_err(|e| unreadable(&dir, e))?,
                    &walked,
                ) {
                    return Err(ArtifactError::Changed(dir));
                }
                fd
            }
            None => rustix::fs::open(&dir, dir_flags, Mode::empty()).map_err(|e| match e {
                rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR => {
                    ArtifactError::Unsupported(dir.clone())
                }
                e => unreadable(&dir, e),
            })?,
        };
        dirs.push((
            dir.clone(),
            rustix::fs::fstat(&fd).map_err(|e| unreadable(&dir, e))?,
        ));
        let fd = Arc::new(fd);
        for item in rustix::fs::Dir::read_from(&*fd).map_err(|e| unreadable(&dir, e))? {
            let item = item.map_err(|e| unreadable(&dir, e))?;
            let name = OsStr::from_bytes(item.file_name().to_bytes()).to_owned();
            if name == "." || name == ".." {
                continue;
            }
            found += 1;
            if found > max_entries {
                return Err(ArtifactError::TooLarge);
            }
            let (abs, rel) = (dir.join(&name), rel_dir.join(&name));
            let walked = rustix::fs::statat(&*fd, &name, AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|e| unreadable(&abs, e))?;
            match FileType::from_raw_mode(walked.st_mode) {
                FileType::Directory if rel.components().count() > MAX_PACKAGE_DEPTH => {
                    return Err(ArtifactError::TooLarge);
                }
                FileType::Directory => pending.push((Some((Arc::clone(&fd), walked)), abs, rel)),
                FileType::RegularFile => {
                    let (len, digest, hashed) =
                        hash_file(&*fd, &name, &abs, &walked, max_bytes - bytes)?;
                    bytes += len;
                    entries.push(Entry::file(&rel, len, &digest));
                    stamps.push((abs, hashed));
                }
                _ => return Err(ArtifactError::Unsupported(abs)),
            }
        }
    }
    unchanged(&stamps, &dirs)?;
    Ok(entries)
}

/// An entry that would not open: replaced (now a link, no longer a
/// directory, or gone), else unreadable.
fn open_failed(path: &Path, e: rustix::io::Errno) -> ArtifactError {
    use rustix::io::Errno;
    match e {
        Errno::LOOP | Errno::NOTDIR | Errno::NOENT => ArtifactError::Changed(path.to_owned()),
        e => ArtifactError::Unreadable(path.to_owned(), e.into()),
    }
}

fn same_inode(a: &Stat, b: &Stat) -> bool {
    a.st_dev == b.st_dev && a.st_ino == b.st_ino
}

/// Length, SHA-256 and [`Stamp`] of the regular file `name` in `dir`, which
/// the walk listed as `walked`, reading at most `budget` bytes. Opened
/// without following a link or waiting on a FIFO; [`ArtifactError::Changed`]
/// unless it is that same regular file, unchanged across the read, and the
/// bytes read are its length.
fn hash_file(
    dir: impl std::os::fd::AsFd,
    name: &OsStr,
    path: &Path,
    walked: &Stat,
    budget: u64,
) -> Result<(u64, [u8; 32], Stamp), ArtifactError> {
    use sha2::Digest;
    use std::io::Read;

    let changed = || ArtifactError::Changed(path.to_owned());
    let unreadable = |e: std::io::Error| ArtifactError::Unreadable(path.to_owned(), e);
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let fd =
        rustix::fs::openat(dir, name, flags, Mode::empty()).map_err(|e| open_failed(path, e))?;
    let opened = rustix::fs::fstat(&fd).map_err(|e| unreadable(e.into()))?;
    if FileType::from_raw_mode(opened.st_mode) != FileType::RegularFile
        || !same_inode(&opened, walked)
    {
        return Err(changed());
    }
    let file = std::fs::File::from(fd);
    let before = stamp(&file.metadata().map_err(unreadable)?);
    if before.2 > budget {
        return Err(ArtifactError::TooLarge);
    }
    let mut hasher = sha2::Sha256::new();
    let read = std::io::copy(&mut (&file).take(budget.saturating_add(1)), &mut hasher)
        .map_err(unreadable)?;
    if read != before.2 || stamp(&file.metadata().map_err(unreadable)?) != before {
        return Err(changed());
    }
    Ok((before.2, hasher.finalize().into(), before))
}

/// Every hashed file is still at its path, as it was when it was read, and
/// every directory read is still that directory at its path, not a link.
fn unchanged(stamps: &[(PathBuf, Stamp)], dirs: &[(PathBuf, Stat)]) -> Result<(), ArtifactError> {
    for (path, read) in dirs {
        if !rustix::fs::lstat(path).is_ok_and(|now| {
            FileType::from_raw_mode(now.st_mode) == FileType::Directory && same_inode(&now, read)
        }) {
            return Err(ArtifactError::Changed(path.clone()));
        }
    }
    for (path, hashed) in stamps {
        let now = std::fs::symlink_metadata(path)
            .map_err(|e| ArtifactError::Unreadable(path.clone(), e))?;
        if stamp(&now) != *hashed {
            return Err(ArtifactError::Changed(path.clone()));
        }
    }
    Ok(())
}

/// The encoding of the module docs over `entries` sorted by path.
fn digest(mut entries: Vec<Entry>) -> String {
    use sha2::Digest;
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    let mut hasher = sha2::Sha256::new();
    for entry in &entries {
        hasher.update((entry.path.len() as u64).to_be_bytes());
        hasher.update(&entry.path);
        hasher.update(&entry.tail);
    }
    let hex: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("{PREFIX}{hex}")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::cell::RefCell;
    use std::os::unix::fs::symlink;

    type Swap = Box<dyn Fn(&Path)>;

    thread_local! {
        /// What a test does to a directory after its parent listed it and
        /// before `collect` opens it.
        static BETWEEN: RefCell<Option<Swap>> = const { RefCell::new(None) };
    }

    pub(super) fn between_listing_and_opening(dir: &Path) {
        BETWEEN.with(|hook| {
            if let Some(hook) = hook.borrow().as_ref() {
                hook(dir);
            }
        });
    }

    /// `collect` over `root`, with `swap` run on each directory between its
    /// listing and its opening.
    fn collect_swapping(
        root: &Path,
        swap: impl Fn(&Path) + 'static,
    ) -> Result<Vec<Entry>, ArtifactError> {
        BETWEEN.with(|hook| *hook.borrow_mut() = Some(Box::new(swap)));
        let result = collect(
            root,
            (crate::launch::MAX_PACKAGE_ENTRIES, MAX_PACKAGE_BYTES),
        );
        BETWEEN.with(|hook| *hook.borrow_mut() = None);
        result
    }

    /// `a.txt` = "hello", `dir/b` = "x".
    fn sample() -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join("a.txt"), "hello").unwrap();
        std::fs::create_dir(t.path().join("dir")).unwrap();
        std::fs::write(t.path().join("dir/b"), "x").unwrap();
        t
    }

    fn mkfifo(path: &Path) {
        let status = std::process::Command::new("mkfifo")
            .arg(path)
            .status()
            .unwrap();
        assert!(status.success());
    }

    /// The encoding, pinned by a value computed outside this code (Python,
    /// `hashlib` and `struct.pack('>Q', …)` over the module docs' layout).
    #[test]
    fn the_sample_has_the_independently_computed_identity() {
        let t = sample();
        assert_eq!(
            identity(t.path()).unwrap(),
            "sha256:83748820712137b3768fc766c5f94ca68daaaf2d381695a716d32f3d1adab084"
        );
    }

    #[test]
    fn an_empty_package_hashes_nothing() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("empty")).unwrap();
        assert_eq!(
            identity(t.path()).unwrap(),
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    /// Any change to what a run would execute moves the identity; reading
    /// it twice does not.
    #[test]
    fn every_change_to_the_files_moves_the_identity() {
        let t = sample();
        let base = identity(t.path()).unwrap();
        assert_eq!(identity(t.path()).unwrap(), base);
        let changes: [&dyn Fn(&Path); 4] = [
            &|p| std::fs::write(p.join("a.txt"), "jello").unwrap(),
            &|p| std::fs::write(p.join("dir/b"), "xx").unwrap(),
            &|p| std::fs::rename(p.join("dir/b"), p.join("dir/c")).unwrap(),
            &|p| std::fs::write(p.join("new"), "").unwrap(),
        ];
        let mut seen = std::collections::BTreeSet::from([base]);
        for change in changes {
            let t = sample();
            change(t.path());
            assert!(seen.insert(identity(t.path()).unwrap()));
        }
    }

    /// A symlink anywhere, wherever it points (inside or out, to a file, a
    /// directory or nothing) means no identity; so does a linked root.
    #[test]
    fn a_symlink_anywhere_has_no_identity() {
        for (at, target) in [
            ("l", "a.txt"),
            ("to_dir", "dir"),
            ("dir/up", "../a.txt"),
            ("out", "/etc/hosts"),
            ("dangling", "missing"),
        ] {
            let t = sample();
            symlink(target, t.path().join(at)).unwrap();
            assert!(
                matches!(identity(t.path()), Err(ArtifactError::Unsupported(_))),
                "{at} -> {target}"
            );
        }
        let s = sample();
        let link = tempfile::tempdir().unwrap();
        symlink(s.path(), link.path().join("pkg")).unwrap();
        assert!(matches!(
            identity(&link.path().join("pkg")),
            Err(ArtifactError::Unsupported(_))
        ));
    }

    #[test]
    fn a_fifo_or_a_missing_or_file_root_has_no_identity() {
        let t = sample();
        mkfifo(&t.path().join("pipe"));
        assert!(matches!(
            identity(t.path()),
            Err(ArtifactError::Unsupported(_))
        ));
        assert!(identity(&t.path().join("missing")).is_err());
        assert!(
            identity(&sample().path().join("a.txt")).is_err(),
            "the root is a directory"
        );
    }

    /// The entry, byte and depth caps each end the walk with no identity;
    /// the depth cap is what bounds the descriptors held open at once.
    #[test]
    fn a_package_over_any_cap_has_no_identity() {
        let t = sample(); // 2 entries at the root and 1 in `dir`; 6 bytes
        assert!(collect(t.path(), (3, 6)).is_ok());
        assert!(matches!(
            collect(t.path(), (2, 6)),
            Err(ArtifactError::TooLarge)
        ));
        assert!(matches!(
            collect(t.path(), (3, 5)),
            Err(ArtifactError::TooLarge)
        ));
        let deep = tempfile::tempdir().unwrap();
        let mut at = deep.path().to_owned();
        for _ in 0..MAX_PACKAGE_DEPTH {
            at.push("d");
        }
        std::fs::create_dir_all(&at).unwrap();
        assert!(identity(deep.path()).is_ok(), "exactly the cap");
        std::fs::create_dir(at.join("d")).unwrap();
        assert!(matches!(
            identity(deep.path()),
            Err(ArtifactError::TooLarge)
        ));
    }

    /// A file is hashed only if it is the one the walk listed; a FIFO in its
    /// place is refused at once, never waited on, and a link is not opened.
    #[test]
    fn a_file_swapped_after_it_was_listed_is_refused() {
        let t = sample();
        mkfifo(&t.path().join("pipe"));
        symlink("a.txt", t.path().join("l")).unwrap();
        let dir =
            rustix::fs::open(t.path(), OFlags::RDONLY | OFlags::DIRECTORY, Mode::empty()).unwrap();
        let stat = |name: &str| rustix::fs::statat(&dir, name, AtFlags::SYMLINK_NOFOLLOW).unwrap();
        let hash = |name: &str, walked: &Stat| {
            hash_file(
                &dir,
                OsStr::new(name),
                &t.path().join(name),
                walked,
                u64::MAX,
            )
        };
        assert!(hash("a.txt", &stat("a.txt")).is_ok());
        // Listed as `dir`, opened as `a.txt`: not the same inode.
        assert!(matches!(
            hash("a.txt", &stat("dir")),
            Err(ArtifactError::Changed(_))
        ));
        assert!(matches!(
            hash("pipe", &stat("pipe")),
            Err(ArtifactError::Changed(_))
        ));
        assert!(matches!(
            hash("l", &stat("l")),
            Err(ArtifactError::Changed(_))
        ));
    }

    /// A directory replaced after its parent listed it, by another
    /// directory or by a link to the original, is not read: `collect` opens
    /// it from the parent's descriptor and checks the inode listed.
    #[test]
    fn a_directory_swapped_after_it_was_listed_is_refused() {
        let swaps: [fn(&Path, &Path); 2] = [
            |dir, away| {
                std::fs::rename(dir, away).unwrap();
                std::fs::create_dir(dir).unwrap();
                std::fs::write(dir.join("b"), "x").unwrap();
            },
            |dir, away| {
                std::fs::rename(dir, away).unwrap();
                symlink(away, dir).unwrap();
            },
        ];
        for swap in swaps {
            let t = sample();
            let away = tempfile::tempdir().unwrap().keep().join("dir");
            let result = collect_swapping(t.path(), move |dir| {
                if dir.ends_with("dir") && !away.exists() {
                    swap(dir, &away);
                }
            });
            assert!(
                matches!(result, Err(ArtifactError::Changed(_))),
                "{:?}",
                result.err()
            );
        }
    }

    /// A hashed file written again before the walk ends, even back to the
    /// same length and modification time, is caught by the closing check.
    #[test]
    fn a_file_rewritten_after_it_was_hashed_is_caught() {
        let t = sample();
        let path = t.path().join("a.txt");
        let meta = std::fs::symlink_metadata(&path).unwrap();
        let stamps = vec![(path.clone(), stamp(&meta))];
        assert!(unchanged(&stamps, &[]).is_ok());
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&path, "jello").unwrap();
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_modified(meta.modified().unwrap()).unwrap();
        let again = std::fs::symlink_metadata(&path).unwrap();
        assert_eq!(
            (again.len(), again.modified().unwrap()),
            (meta.len(), meta.modified().unwrap())
        );
        assert!(matches!(
            unchanged(&stamps, &[]),
            Err(ArtifactError::Changed(_))
        ));
        std::fs::remove_file(&path).unwrap();
        assert!(unchanged(&stamps, &[]).is_err(), "gone is not unchanged");
    }

    /// A directory read, then moved away and replaced by a link to where it
    /// went (so paths through it still reach the same files), is caught by
    /// the closing check (review: directories are checked like files).
    #[test]
    fn a_directory_replaced_by_a_link_after_it_was_read_is_caught() {
        let t = sample();
        let dir = t.path().join("dir");
        let dirs = vec![(dir.clone(), rustix::fs::lstat(&dir).unwrap())];
        assert!(unchanged(&[], &dirs).is_ok());
        let away = tempfile::tempdir().unwrap();
        std::fs::rename(&dir, away.path().join("dir")).unwrap();
        symlink(away.path().join("dir"), &dir).unwrap();
        assert!(
            std::fs::metadata(dir.join("b")).is_ok(),
            "the file is still reachable"
        );
        assert!(matches!(
            unchanged(&[], &dirs),
            Err(ArtifactError::Changed(_))
        ));
    }

    /// A directory that will not open for want of permission is unreadable,
    /// not "changed"; so is such a file.
    #[test]
    fn unreadable_entries_are_not_reported_as_changed() {
        use std::os::unix::fs::PermissionsExt;
        if rustix::process::geteuid().is_root() {
            return; // root opens them anyway
        }
        for entry in ["dir", "dir/b"] {
            let t = sample();
            let path = t.path().join(entry);
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
            let result = identity(t.path());
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert!(
                matches!(result, Err(ArtifactError::Unreadable(..))),
                "{entry}: {result:?}"
            );
        }
    }

    /// The digest sorts by path bytes, not by walk order or `Path` order.
    #[test]
    fn the_digest_sorts_by_path_bytes() {
        let file = |path: &str| Entry::file(Path::new(path), 0, &[0; 32]);
        let forward = digest(vec![file("a-b"), file("a/b"), file("a0")]);
        let backward = digest(vec![file("a0"), file("a/b"), file("a-b")]);
        assert_eq!(forward, backward);
        assert!(forward.starts_with(PREFIX));
        assert_eq!(forward.len(), PREFIX.len() + 64);
        assert!(
            forward[PREFIX.len()..]
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
    }
}
