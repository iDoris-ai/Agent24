//! A module package's artifact identity (K1-6a-G §2.1): a digest of exactly
//! the files in a package, for a run to carry from its start, so the same
//! version with a changed file is a different artifact. It proves which
//! files these are, not where they came from (ADR-K1-03 §4).
//!
//! The walk follows no symlink. A regular file contributes its relative
//! path, length and SHA-256; a symlink its relative path and its target as
//! written. Anything else, a link that is absolute, climbs out of the
//! package (as written or as resolved) or resolves to nothing, an entry that
//! changes or is replaced while the package is read, or more than
//! [`MAX_PACKAGE_ENTRIES`](crate::launch::MAX_PACKAGE_ENTRIES) entries or
//! [`MAX_PACKAGE_BYTES`] bytes: no identity, and the caller fails closed.
//!
//! The digest is SHA-256 over the entries sorted by relative path bytes
//! (components joined by `/`), each `u64be(len(path)) ‖ path` followed by
//! `b'f' ‖ u64be(length) ‖ sha256(file)` or `b'l' ‖ u64be(len(target)) ‖
//! target`, written `sha256:` and 64 lowercase hex digits.

use rustix::fs::{AtFlags, FileType, Mode, OFlags, Stat};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// What every identity starts with.
pub const PREFIX: &str = "sha256:";

/// Most bytes the files of a package may hold and still get an identity:
/// every start reads them all, before the start timeout runs. ⚖️
pub const MAX_PACKAGE_BYTES: u64 = 1 << 30;

/// Why a package has no identity.
#[derive(Debug)]
pub enum ArtifactError {
    /// An entry could not be read or inspected.
    Unreadable(PathBuf, std::io::Error),
    /// Neither a regular file, a directory nor a symlink (a FIFO, socket or
    /// device), or the package root is not a directory.
    Unsupported(PathBuf),
    /// A symlink whose target is absolute or leads out of the package.
    EscapingLink(PathBuf),
    /// An entry changed, or was replaced, while the package was read.
    Changed(PathBuf),
    /// More entries or bytes than a start may read.
    TooLarge,
}

impl std::fmt::Display for ArtifactError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreadable(path, e) => write!(f, "{} could not be read: {e}", path.display()),
            Self::Unsupported(path) => write!(f, "{} is no file, dir or link", path.display()),
            Self::EscapingLink(path) => write!(f, "{} links outside the package", path.display()),
            Self::Changed(path) => write!(f, "{} changed while it was read", path.display()),
            Self::TooLarge => write!(f, "the package has too many entries or bytes"),
        }
    }
}

impl std::error::Error for ArtifactError {}

/// One entry: its relative path, and the rest of its encoding (`b'f' ‖ …`
/// or `b'l' ‖ …`, see the module docs).
struct Entry {
    path: Vec<u8>,
    tail: Vec<u8>,
}

impl Entry {
    fn new(rel: &Path, tag: u8, len: u64, rest: &[u8]) -> Self {
        let path = rel.as_os_str().as_bytes().to_vec();
        Self {
            path,
            tail: [&[tag][..], &len.to_be_bytes(), rest].concat(),
        }
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

/// Every file and symlink under `root`, within `(entries, bytes)` caps.
/// Each directory is opened from its parent's descriptor, refusing a link,
/// and must be the inode its parent listed; so must each file. A swap
/// while the walk runs is never followed, and every hashed file is checked
/// again at the end.
fn collect(
    root: &Path,
    (max_entries, max_bytes): (usize, u64),
) -> Result<Vec<Entry>, ArtifactError> {
    let unreadable =
        |path: &Path, e: rustix::io::Errno| ArtifactError::Unreadable(path.to_owned(), e.into());
    let dir_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let root_fd = rustix::fs::open(root, dir_flags, Mode::empty()).map_err(|e| match e {
        rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR => {
            ArtifactError::Unsupported(root.to_owned())
        }
        e => unreadable(root, e),
    })?;
    // Where links must land: the root as the filesystem resolves it.
    let real_root =
        std::fs::canonicalize(root).map_err(|e| ArtifactError::Unreadable(root.to_owned(), e))?;
    // Directories still to read, each opened only when its turn comes.
    let mut pending = vec![(
        std::sync::Arc::new(root_fd),
        None,
        root.to_owned(),
        PathBuf::new(),
    )];
    let (mut found, mut bytes, mut entries, mut stamps) = (0usize, 0u64, Vec::new(), Vec::new());
    while let Some((parent, listed, dir, rel_dir)) = pending.pop() {
        let fd = match listed {
            None => parent,
            Some((name, walked)) => {
                let fd = rustix::fs::openat(&*parent, &name, dir_flags, Mode::empty())
                    .map_err(|_| ArtifactError::Changed(dir.clone()))?;
                if !same_inode(
                    &rustix::fs::fstat(&fd).map_err(|e| unreadable(&dir, e))?,
                    &walked,
                ) {
                    return Err(ArtifactError::Changed(dir));
                }
                std::sync::Arc::new(fd)
            }
        };
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
                FileType::Directory => {
                    pending.push((std::sync::Arc::clone(&fd), Some((name, walked)), abs, rel))
                }
                FileType::RegularFile => {
                    let (len, digest, hashed) =
                        hash_file(&*fd, &name, &abs, &walked, max_bytes - bytes)?;
                    bytes += len;
                    entries.push(Entry::new(&rel, b'f', len, &digest));
                    stamps.push((abs, hashed));
                }
                FileType::Symlink => {
                    let target = rustix::fs::readlinkat(&*fd, &name, Vec::new())
                        .map_err(|e| unreadable(&abs, e))?;
                    let target = target.into_bytes();
                    // Lexically inside is not enough: through another link
                    // (`s -> .`, then `l -> s/s/../..`) it can still climb
                    // out, so it must also resolve under the root; a
                    // dangling link resolves nowhere.
                    let lands_inside =
                        std::fs::canonicalize(&abs).is_ok_and(|real| real.starts_with(&real_root));
                    if !stays_inside(&rel, Path::new(OsStr::from_bytes(&target))) || !lands_inside {
                        return Err(ArtifactError::EscapingLink(abs));
                    }
                    entries.push(Entry::new(&rel, b'l', target.len() as u64, &target));
                }
                _ => return Err(ArtifactError::Unsupported(abs)),
            }
        }
    }
    unchanged(&stamps)?;
    Ok(entries)
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
    let fd = rustix::fs::openat(dir, name, flags, Mode::empty()).map_err(|e| match e {
        rustix::io::Errno::LOOP => changed(),
        e => unreadable(e.into()),
    })?;
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

/// Every hashed file is still at its path, as it was when it was read.
fn unchanged(stamps: &[(PathBuf, Stamp)]) -> Result<(), ArtifactError> {
    for (path, hashed) in stamps {
        let now = std::fs::symlink_metadata(path)
            .map_err(|e| ArtifactError::Unreadable(path.clone(), e))?;
        if stamp(&now) != *hashed {
            return Err(ArtifactError::Changed(path.clone()));
        }
    }
    Ok(())
}

/// Whether a symlink at `link` (relative to the root) with `target` stays
/// inside the package: `target` is relative, and resolving it lexically from
/// the link's directory never climbs above the root.
fn stays_inside(link: &Path, target: &Path) -> bool {
    use std::path::Component;
    let mut depth = link
        .parent()
        .map_or(0, |parent| parent.components().count());
    for component in target.components() {
        depth = match component {
            Component::CurDir => depth,
            Component::Normal(_) => depth + 1,
            Component::ParentDir => match depth.checked_sub(1) {
                Some(depth) => depth,
                None => return false,
            },
            Component::RootDir | Component::Prefix(_) => return false,
        };
    }
    true
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
    use std::os::unix::fs::symlink;

    /// `a.txt` = "hello", `dir/b` = "x", `l` → `a.txt`.
    fn sample() -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join("a.txt"), "hello").unwrap();
        std::fs::create_dir(t.path().join("dir")).unwrap();
        std::fs::write(t.path().join("dir/b"), "x").unwrap();
        symlink("a.txt", t.path().join("l")).unwrap();
        t
    }

    /// The encoding, pinned by a value computed outside this code (Python,
    /// `hashlib` and `struct.pack('>Q', …)` over the module docs' layout).
    #[test]
    fn the_sample_has_the_independently_computed_identity() {
        let t = sample();
        assert_eq!(
            identity(t.path()).unwrap(),
            "sha256:73e3e7d5a1622f120e4007f0d0e993485adcdd8c5f4e13794d5cbc1c272fcfd6"
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
        let changes: [&dyn Fn(&Path); 5] = [
            &|p| std::fs::write(p.join("a.txt"), "jello").unwrap(),
            &|p| std::fs::write(p.join("dir/b"), "xx").unwrap(),
            &|p| std::fs::rename(p.join("dir/b"), p.join("dir/c")).unwrap(),
            &|p| std::fs::write(p.join("new"), "").unwrap(),
            &|p| {
                std::fs::remove_file(p.join("l")).unwrap();
                symlink("dir/b", p.join("l")).unwrap();
            },
        ];
        let mut seen = std::collections::BTreeSet::from([base]);
        for change in changes {
            let t = sample();
            change(t.path());
            assert!(seen.insert(identity(t.path()).unwrap()));
        }
    }

    /// A link is recorded, not followed: a link into a directory does not
    /// walk it, and a link inside the package may point at a sibling.
    #[test]
    fn links_are_recorded_not_followed() {
        let t = sample();
        symlink("dir", t.path().join("to_dir")).unwrap();
        symlink("../a.txt", t.path().join("dir/up")).unwrap();
        let mut found: Vec<(String, char)> = collect(t.path(), CAPS)
            .unwrap()
            .into_iter()
            .map(|Entry { path, tail }| match tail[0] {
                b'f' => (String::from_utf8(path).unwrap(), 'f'),
                _ => {
                    let target = &tail[9..];
                    let path = String::from_utf8(path).unwrap();
                    let expected = if path == "to_dir" {
                        "dir"
                    } else if path == "l" {
                        "a.txt"
                    } else {
                        "../a.txt"
                    };
                    assert_eq!(target, expected.as_bytes(), "{path}");
                    (path, 'l')
                }
            })
            .collect();
        found.sort();
        assert_eq!(
            found,
            [
                ("a.txt".to_owned(), 'f'),
                ("dir/b".to_owned(), 'f'),
                ("dir/up".to_owned(), 'l'),
                ("l".to_owned(), 'l'),
                ("to_dir".to_owned(), 'l'),
            ]
        );
    }

    /// A link out of the package, however it is spelled, is no identity.
    #[test]
    fn a_link_out_of_the_package_has_no_identity() {
        for (at, target) in [
            ("out", "/etc/hosts"),
            ("out", ".."),
            ("out", "../x"),
            ("dir/out", "../../x"),
            ("dir/out", "../dir/../../x"),
        ] {
            let t = sample();
            symlink(target, t.path().join(at)).unwrap();
            assert!(
                matches!(identity(t.path()), Err(ArtifactError::EscapingLink(_))),
                "{at} -> {target}"
            );
        }
        // Lexically inside, but through a link to the root it climbs out.
        let t = sample();
        symlink(".", t.path().join("s")).unwrap();
        symlink("s/s/../..", t.path().join("through")).unwrap();
        assert!(stays_inside(Path::new("through"), Path::new("s/s/../..")));
        assert!(
            matches!(identity(t.path()), Err(ArtifactError::EscapingLink(_))),
            "a link through a link out of the package"
        );
        // A link to nothing is refused too.
        let t = sample();
        symlink("missing", t.path().join("dangling")).unwrap();
        assert!(matches!(
            identity(t.path()),
            Err(ArtifactError::EscapingLink(_))
        ));
        let t = sample();
        symlink("../a.txt", t.path().join("dir/inside")).unwrap();
        assert!(identity(t.path()).is_ok(), "dir/../a.txt stays inside");
        assert!(stays_inside(Path::new("dir/x"), Path::new("./../a.txt")));
        assert!(!stays_inside(Path::new("x"), Path::new("./..")));
    }

    #[test]
    fn a_fifo_or_a_missing_or_linked_root_has_no_identity() {
        let t = sample();
        let status = std::process::Command::new("mkfifo")
            .arg(t.path().join("pipe"))
            .status()
            .unwrap();
        assert!(status.success());
        assert!(matches!(
            identity(t.path()),
            Err(ArtifactError::Unsupported(_))
        ));
        assert!(identity(&t.path().join("missing")).is_err());
        let s = sample();
        let link = tempfile::tempdir().unwrap();
        symlink(s.path(), link.path().join("pkg")).unwrap();
        assert!(
            identity(&link.path().join("pkg")).is_err(),
            "the root is not followed"
        );
        assert!(
            identity(&s.path().join("a.txt")).is_err(),
            "the root is a directory"
        );
    }

    const CAPS: (usize, u64) = (crate::launch::MAX_PACKAGE_ENTRIES, MAX_PACKAGE_BYTES);

    /// A link that is out of the package as written is refused even when it
    /// resolves back inside: both checks run.
    #[test]
    fn a_link_out_as_written_is_refused_even_if_it_resolves_inside() {
        let t = sample();
        symlink(t.path().join("a.txt"), t.path().join("absolute")).unwrap();
        assert!(matches!(
            identity(t.path()),
            Err(ArtifactError::EscapingLink(_))
        ));
        let t = sample();
        let name = t.path().file_name().unwrap().to_str().unwrap().to_owned();
        symlink(format!("../{name}/a.txt"), t.path().join("round_trip")).unwrap();
        assert!(matches!(
            identity(t.path()),
            Err(ArtifactError::EscapingLink(_))
        ));
    }

    /// Both caps, entries and bytes, end the walk with no identity.
    #[test]
    fn a_package_over_either_cap_has_no_identity() {
        let t = sample(); // 3 entries at the root and 1 in `dir`; 6 bytes
        assert!(collect(t.path(), (4, 6)).is_ok());
        assert!(matches!(
            collect(t.path(), (3, 6)),
            Err(ArtifactError::TooLarge)
        ));
        assert!(matches!(
            collect(t.path(), (4, 5)),
            Err(ArtifactError::TooLarge)
        ));
    }

    /// A file is hashed only if it is the one the walk listed; a FIFO in its
    /// place is refused at once, never waited on.
    #[test]
    fn a_file_swapped_after_it_was_listed_is_refused() {
        use rustix::fs::{AtFlags, Mode, OFlags};
        let t = sample();
        let status = std::process::Command::new("mkfifo")
            .arg(t.path().join("pipe"))
            .status()
            .unwrap();
        assert!(status.success());
        let dir =
            rustix::fs::open(t.path(), OFlags::RDONLY | OFlags::DIRECTORY, Mode::empty()).unwrap();
        let stat = |name: &str| rustix::fs::statat(&dir, name, AtFlags::SYMLINK_NOFOLLOW).unwrap();
        let hash = |name: &str, walked: &rustix::fs::Stat| {
            hash_file(
                &dir,
                std::ffi::OsStr::new(name),
                &t.path().join(name),
                walked,
                u64::MAX,
            )
        };
        assert!(hash("a.txt", &stat("a.txt")).is_ok());
        // Listed as `l` (a link), opened as `a.txt`: not the same inode.
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

    /// A hashed file written again before the walk ends, even back to the
    /// same length and modification time, is caught by the closing check.
    #[test]
    fn a_file_rewritten_after_it_was_hashed_is_caught() {
        let t = sample();
        let path = t.path().join("a.txt");
        let meta = std::fs::symlink_metadata(&path).unwrap();
        let stamps = vec![(path.clone(), stamp(&meta))];
        assert!(unchanged(&stamps).is_ok());
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&path, "jello").unwrap();
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_modified(meta.modified().unwrap()).unwrap();
        let again = std::fs::symlink_metadata(&path).unwrap();
        assert_eq!(
            (again.len(), again.modified().unwrap()),
            (meta.len(), meta.modified().unwrap())
        );
        assert!(matches!(unchanged(&stamps), Err(ArtifactError::Changed(_))));
        std::fs::remove_file(&path).unwrap();
        assert!(unchanged(&stamps).is_err(), "gone is not unchanged");
    }

    #[test]
    fn an_unreadable_file_has_no_identity() {
        use std::os::unix::fs::PermissionsExt;
        if rustix::process::geteuid().is_root() {
            return; // root reads it anyway
        }
        let t = sample();
        let file = t.path().join("dir/b");
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000)).unwrap();
        assert!(matches!(
            identity(t.path()),
            Err(ArtifactError::Unreadable(..))
        ));
    }

    /// The digest sorts by path bytes, not by walk order or `Path` order.
    #[test]
    fn the_digest_sorts_by_path_bytes() {
        let file = |path: &str| Entry::new(Path::new(path), b'f', 0, &[0; 32]);
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
