//! ME-3a — installing a package, and the two properties that make it worth
//! having a module of its own.
//!
//! **Atomic**: a package is either fully installed or not installed. There is no
//! state in which the packages root holds half of one.
//!
//! **Clean on failure**: a refused install leaves no ENTRY behind in the packages
//! root — not a partial directory, not a `.tmp` — and if the root itself did not
//! exist beforehand, it does not exist afterwards either.
//!
//! An earlier version of this sentence also promised an unchanged mtime on the
//! parent. That was never true and nothing tested it: staging is created INSIDE
//! the packages root (which is what makes the final `rename` atomic), so creating
//! and removing it necessarily touches the root's mtime. The claim is dropped
//! rather than weakened, because the property that matters — nothing left over —
//! is the one the tests actually check.
//!
//! The second is the one that is easy to write a decorative test for. Asserting
//! that this function returned `Err` says nothing about what it left on disk, so
//! the tests here snapshot the whole tree before and after and compare sets. The
//! snapshot function is the INSTRUMENT for those tests, so it has negative
//! controls of its own — see `the_snapshot_sees_each_kind_of_difference` in this
//! file's test module. (Not a rustdoc link: the test module does not exist in a
//! doc build, so a link there is one that can never resolve.)

use std::path::{Path, PathBuf};

use crate::discovery::MANIFEST_FILE;
use agent24_domain::DomainOsManifest;

/// Why an install did not happen. Every variant means the packages root was left
/// untouched.
#[derive(Debug)]
pub enum InstallError {
    /// The source is not a package this kernel will accept.
    Source(String),
    /// The string given is not a module name at all, so it names nothing that
    /// could be installed. Separate from `Source` because `uninstall` has no
    /// source: reporting a refused NAME as a refused PACKAGE told the operator to
    /// go and look at a directory that was never the problem.
    InvalidName(String),
    /// A package by that name is already installed.
    AlreadyInstalled(String),
    /// The filesystem refused, or the staging directory and the destination are
    /// not on the same device (see [`install`] for why that matters).
    Filesystem(String),
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Source(s) => write!(f, "source package rejected: {s}"),
            Self::InvalidName(n) => write!(
                f,
                "{n:?} is not a valid module name, so it cannot name an installed package"
            ),
            Self::AlreadyInstalled(n) => write!(
                f,
                "a domain OS named {n:?} is already installed; uninstall it first"
            ),
            Self::Filesystem(s) => write!(f, "filesystem: {s}"),
        }
    }
}

/// Install the package directory `src` into `packages_root`.
///
/// The destination name comes from the **validated manifest**, never from the
/// source directory's name. A directory called `sin90/` whose manifest says
/// `cos72` installs as `cos72` — the manifest is the module's sole identity
/// (`DomainOsManifest`), and letting the enclosing directory name decide would
/// give a package two identities that can disagree.
///
/// # Atomicity, and the assumption underneath it
///
/// The package is copied into a staging directory and then moved with a single
/// `rename`, so the only window in which anything can be half-done is that one
/// call — and `rename` of a directory is atomic **within one filesystem**.
///
/// Across filesystems it is not atomic at all: the implementation degrades to
/// copy-then-delete, and a failure mid-copy leaves exactly the partial state this
/// design exists to prevent. That is a CONFIGURATION property, not a code one —
/// it depends on where the staging directory landed on the operator's machine, so
/// a test passing here says nothing about their disk. Hence `same_device` is
/// checked at runtime, not merely covered by a test.
pub fn install(src: &Path, packages_root: &Path) -> Result<PathBuf, InstallError> {
    // Read and validate BEFORE touching the destination. A source that will be
    // rejected must never have caused a directory to be created.
    let manifest_path = src.join(MANIFEST_FILE);
    let bytes = std::fs::read(&manifest_path)
        .map_err(|e| InstallError::Source(format!("no readable {MANIFEST_FILE}: {e}")))?;
    let text = String::from_utf8(bytes)
        .map_err(|e| InstallError::Source(format!("{MANIFEST_FILE} is not valid UTF-8: {e}")))?;
    let manifest =
        DomainOsManifest::from_yaml(&text).map_err(|e| InstallError::Source(e.to_string()))?;

    let dest = packages_root.join(manifest.name());
    if entry_exists(&dest)? {
        return Err(InstallError::AlreadyInstalled(manifest.name().to_owned()));
    }

    // What this call creates decides what a failure has to clean up. If it
    // created the root — or any parent of it — a later failure must take that
    // away again: the promise is that a refused install leaves no trace, and an
    // empty `packages/` directory that only exists because someone tried once is
    // a trace.
    //
    // So the directories are created HERE, one level at a time from the top,
    // and each one that this call created is recorded as it is made: then any
    // failure — including one part-way down the path — can take back exactly
    // what was made. (Handing the whole path to a recursive create, as the
    // round-3 fix did, made parents that a failure deeper down left behind:
    // review of ME3-SUP slice 1, round 3″.) `check_packages_root` then checks
    // the root as a daemon start would — owned by us, nobody else can write it —
    // NOW rather than after the install claimed success (under `umask 002` a
    // plain `create_dir_all` made it `0775`, the install succeeded, and the next
    // daemon start refused the whole root). The check CREATES nothing: with a
    // root a concurrent install's cleanup has just removed, it fails, rather
    // than re-creating directories nobody recorded (round 3⁗).
    let created = create_missing_dirs(packages_root)?;
    let undo_root = || {
        for dir in created.iter().rev() {
            // `remove_dir` (not `_all`): it only succeeds while the directory is
            // still empty, so a concurrent install that already put something
            // there is never destroyed by our cleanup. Deepest first, so each
            // parent is empty by the time it is tried.
            let _ = std::fs::remove_dir(dir);
        }
    };
    if let Err(e) = crate::check_packages_root(packages_root) {
        undo_root();
        return Err(InstallError::Filesystem(e.to_string()));
    }

    // Staging lives INSIDE the packages root, not in the system temp directory.
    // That is the whole point: `/tmp` is frequently a different volume (it is on
    // stock macOS), and a cross-device rename is not atomic.
    // The name carries a per-call counter as well as the pid. Without it, two
    // concurrent installs of the SAME package in one process share a staging path,
    // and the cleanup below has the second caller delete the first caller's
    // half-written tree. Measured, that still ends cleanly — one wins, one fails,
    // nothing partial is installed — but it ends cleanly for the WRONG REASON:
    // safety comes from the interrupted caller failing ENTIRELY, not from the two
    // never touching each other. The day this function grows a resumable path,
    // that borrowed guarantee disappears with no symptom except packages missing a
    // few files. Cheaper not to share the path in the first place.
    let staging = staging_path(packages_root, manifest.name());
    // There is deliberately NO sweep of stale staging directories here. An earlier
    // version had one, scoped to this pid, and it was removed because its set of
    // correct behaviours is EMPTY while its set of incorrect ones is catastrophic:
    //
    //   - Same process, failed install: every failure path below already removes
    //     its own staging. Nothing to sweep.
    //   - Same process, crashed: the process is gone, so the next daemon has a
    //     different pid and cannot recognise the debris as its own anyway.
    //   - Same process, a CONCURRENT install still running: same pid, different
    //     seq — indistinguishable by name from the dead debris above. The sweep
    //     deletes a live sibling's half-written tree. Measured: the victim then
    //     completes and `rename`s a package that is missing a hundred files, and
    //     `install` returns Ok. A half package, installed atomically, with no error
    //     signal anywhere.
    //
    // The identification could not be fixed by tightening it, because the two cases
    // it must separate carry identical names. Crash debris is instead left for the
    // operator: it is inert (the scanner refuses dot-prefixed directories) and it is
    // visible by name in the scan's refusal list.
    copy_tree(src, &staging).inspect_err(|_| {
        remove_quietly(&staging);
        undo_root();
    })?;

    // NOTE, so nobody mistakes where the guarantee comes from: today's atomicity
    // comes from staging being CONSTRUCTED inside the destination's parent, not
    // from this check — the check cannot fire as long as that construction holds,
    // and the tests exercise the function, not the gate. Its value is entirely in
    // the future: the first time someone moves staging elsewhere, this is what
    // turns a silent loss of atomicity into a refused install.
    //
    // Belt and braces: staging is inside the destination's parent by construction,
    // so this should always hold. It is checked anyway because the cost of being
    // wrong is silent partial state on somebody else's machine, and because a
    // future change to where staging lives would otherwise break atomicity with no
    // visible symptom.
    if !same_device(&staging, packages_root).unwrap_or(false) {
        remove_quietly(&staging);
        undo_root();
        return Err(InstallError::Filesystem(
            "staging directory is on a different filesystem from the packages root; \
             a rename across filesystems is not atomic"
                .to_owned(),
        ));
    }

    std::fs::rename(&staging, &dest).map_err(|e| {
        remove_quietly(&staging);
        undo_root();
        InstallError::Filesystem(format!("could not move the package into place: {e}"))
    })?;
    Ok(dest)
}

/// Remove an installed package. Missing is not an error the caller has to handle
/// differently from removed — both end with "it is not installed".
///
/// # Why the name is validated here and not left to the caller
///
/// `install` never takes a name: it reads one out of a validated manifest. So
/// until this function existed, "what may be a package name" was decided in
/// exactly one place. `uninstall` takes a name as a STRING, and a string joined
/// onto a path is not a name — `Path::join` replaces the whole path when given an
/// absolute one, and `..` walks out of the root. With `remove_dir_all` on the
/// other end, `uninstall("/somewhere/else")` deletes `/somewhere/else` and reports
/// success.
///
/// The rule used is [`agent24_domain::is_valid_module_name`] — the same one the
/// manifest is held to, deliberately not a new "does it look like a path" check.
/// A second, weaker definition of "package name" is how the two drift apart.
pub fn uninstall(name: &str, packages_root: &Path) -> Result<bool, InstallError> {
    if !agent24_domain::is_valid_module_name(name) {
        return Err(InstallError::InvalidName(name.to_owned()));
    }
    let dest = packages_root.join(name);
    if !entry_exists(&dest)? {
        return Ok(false);
    }
    // Removal is staged the same way installation is, and for the same reason:
    // `remove_dir_all` is not atomic. Half way through it can hit a permission
    // error or an I/O error and return `Err` having already deleted files — and
    // what is left behind is neither installed nor uninstalled. A daemon starting
    // at that moment scans a half package. Renaming first makes the package
    // disappear in one step; whatever the cleanup then fails to delete is inert
    // debris the scanner refuses by name.
    let doomed = staging_path(packages_root, name);
    // `rename` moves the ENTRY, so this works on a symlink (dangling or not) just
    // as it does on a directory — which is why the removal below has to handle
    // both kinds too.
    std::fs::rename(&dest, &doomed).map_err(|e| {
        InstallError::Filesystem(format!("could not remove {}: {e}", dest.display()))
    })?;
    remove_quietly(&doomed);
    Ok(true)
}

/// Is there an ENTRY at this path — regardless of whether it can be followed?
///
/// Neither `exists` nor `try_exists` answers that question. Both follow symlinks,
/// so a package directory that is a symlink to a deleted target reads as absent,
/// and `exists` additionally turns "I could not find out" (no search permission
/// on a parent, an I/O error) into `false`. Either way `uninstall` would report
/// "was not installed" about an entry the scanner DOES see and refuse — a package
/// the operator can neither use nor remove.
///
/// `symlink_metadata` asks about the entry itself. `NotFound` is the only error
/// that means absent; every other one is propagated, because not knowing is a
/// third outcome and hiding it inside `false` is what created this bug class.
fn entry_exists(path: &Path) -> Result<bool, InstallError> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(InstallError::Filesystem(format!(
            "could not determine whether {} exists: {e}",
            path.display()
        ))),
    }
}

/// A staging path that NO other call will ever produce.
///
/// Extracted so the property can be observed. It is not testable through
/// `install`: sequential calls each clean up after themselves, so a test written
/// against `install` passes whether or not the paths collide — which is exactly
/// what a mutation showed when this was first written as an assertion about
/// leftover debris.
fn staging_path(packages_root: &Path, name: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    packages_root.join(format!(".staging-{name}-{}-{seq}", std::process::id()))
}

/// Best-effort cleanup. A failure here is not reported: the caller is already
/// returning an error, and replacing "the install failed because X" with "cleanup
/// failed" would hide the reason the operator needs.
fn remove_quietly(p: &Path) {
    // A `remove_file` fallback was added here for the case where the entry is a
    // symlink rather than a directory, and then removed: `remove_dir_all` "does
    // not follow symbolic links and will simply remove the symbolic link itself"
    // (std docs, and measured — a mutation that dropped the fallback killed no
    // test because there is nothing for it to do). Defensive code whose failure
    // case cannot be reached is not free: it says a hazard exists where none does,
    // and the next reader budgets for it.
    let _ = std::fs::remove_dir_all(p);
}

/// Create `path` and every missing directory above it, top-down, each `0700`.
/// Returns the directories this call created, top-down. On failure, removes
/// what it created before returning the error.
///
/// "Missing" is "not confirmed to exist": an ancestor whose `symlink_metadata`
/// fails for any reason (not found, a component too long, a file where a
/// directory should be) is attempted, so that the create reports the real
/// error — and an ancestor that does exist (even as a dangling symlink) ends
/// the walk up, so nothing that was there before is ever recorded as ours.
#[cfg(unix)]
fn create_missing_dirs(path: &Path) -> Result<Vec<PathBuf>, InstallError> {
    use std::os::unix::fs::DirBuilderExt;
    let mut missing: Vec<&Path> = path
        .ancestors()
        .take_while(|p| !p.as_os_str().is_empty() && std::fs::symlink_metadata(p).is_err())
        .collect();
    missing.reverse();
    let mut created = Vec::new();
    for dir in missing {
        match std::fs::DirBuilder::new().mode(0o700).create(dir) {
            Ok(()) => created.push(dir.to_path_buf()),
            // Made by a concurrent install between the look and the create:
            // not ours to remove.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => {
                for made in created.iter().rev() {
                    let _ = std::fs::remove_dir(made);
                }
                return Err(InstallError::Filesystem(format!(
                    "could not create {}: {e}",
                    dir.display()
                )));
            }
        }
    }
    Ok(created)
}

#[cfg(not(unix))]
fn create_missing_dirs(path: &Path) -> Result<Vec<PathBuf>, InstallError> {
    std::fs::create_dir_all(path)
        .map_err(|e| InstallError::Filesystem(format!("could not create packages root: {e}")))?;
    Ok(Vec::new())
}

/// Copy a package tree. Symlinks are refused rather than followed, for the reason
/// `os_discovery` refuses a symlinked manifest: a package's identity is decided by
/// a file inside it, and a link lets that file live outside the tree being
/// validated.
///
/// Every directory and file in the copy loses its group and other WRITE bits.
/// Starting a module refuses a package anyone but its owner can write
/// (`agent24_os_proto::launch::resolve`), and without this a daemon running
/// with the common `umask 002` would create `0775` directories — and a source
/// file that is `0664` is copied as `0664` — so the install would succeed and
/// every start of the module fail (review of ME3-SUP slice 1, round 2).
///
/// `dst` is created NON-recursively: its parent must already be there. For the
/// top call that parent is the packages root, checked by `check_packages_root`
/// a moment earlier — and if a concurrent install's cleanup has since removed
/// it (it was empty and that install created it), this install must fail
/// rather than quietly re-create a root nobody recorded or checked (review of
/// ME3-SUP slice 1, round 3‴).
fn copy_tree(src: &Path, dst: &Path) -> Result<(), InstallError> {
    std::fs::create_dir(dst).map_err(|e| {
        InstallError::Filesystem(format!("could not create {}: {e}", dst.display()))
    })?;
    owner_write_only(dst)?;
    let entries = std::fs::read_dir(src)
        .map_err(|e| InstallError::Source(format!("could not read {}: {e}", src.display())))?;
    for e in entries {
        let e = e.map_err(|e| InstallError::Source(format!("could not read entry: {e}")))?;
        let ty = e
            .file_type()
            .map_err(|e| InstallError::Source(format!("could not stat entry: {e}")))?;
        let to = dst.join(e.file_name());
        if ty.is_symlink() {
            return Err(InstallError::Source(format!(
                "{} is a symlink; refusing to copy it",
                e.path().display()
            )));
        } else if ty.is_dir() {
            copy_tree(&e.path(), &to)?;
        } else {
            std::fs::copy(e.path(), &to).map_err(|err| {
                InstallError::Filesystem(format!("could not copy {}: {err}", e.path().display()))
            })?;
            owner_write_only(&to)?;
        }
    }
    Ok(())
}

/// Clear the group and other write bits of `path` (not a symlink: `copy_tree`
/// refuses those).
#[cfg(unix)]
fn owner_write_only(path: &Path) -> Result<(), InstallError> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(path)
        .map_err(|e| InstallError::Filesystem(format!("could not stat {}: {e}", path.display())))?;
    let mode = meta.permissions().mode();
    if mode & 0o022 != 0 {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode & !0o022)).map_err(
            |e| InstallError::Filesystem(format!("could not restrict {}: {e}", path.display())),
        )?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn owner_write_only(_path: &Path) -> Result<(), InstallError> {
    Ok(())
}

/// Whether two paths sit on the same filesystem.
#[cfg(unix)]
fn same_device(a: &Path, b: &Path) -> Option<bool> {
    use std::os::unix::fs::MetadataExt;
    let (a, b) = (std::fs::metadata(a).ok()?, std::fs::metadata(b).ok()?);
    Some(a.dev() == b.dev())
}

#[cfg(not(unix))]
fn same_device(_a: &Path, _b: &Path) -> Option<bool> {
    // No portable device id. Returning None means the caller's `unwrap_or(false)`
    // refuses the install rather than assuming atomicity it cannot verify.
    None
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::collections::BTreeSet;

    /// An installed package is writable by its owner only, whatever the source
    /// allowed: a module is refused at start if anyone else can write its
    /// package, so an install that kept a `0664` file (or made `0775`
    /// directories under `umask 002`) would succeed and leave a package that
    /// can never run.
    /// A refused install leaves no trace — including parents of the packages
    /// root that the install itself created (undoing only the root left them
    /// behind).
    #[cfg(unix)]
    #[test]
    fn a_refused_install_removes_the_parents_it_created() {
        let t = tempfile::tempdir().unwrap();
        let src = src_pkg(t.path(), "src", "shared");
        // Refused mid-copy: a symlink in the source (copy_tree refuses those),
        // which is after the root and its parents have been created.
        std::os::unix::fs::symlink("/etc/hosts", src.join("link")).unwrap();
        let root = t.path().join("a/b/packages");
        install(&src, &root).expect_err("a symlink in the source");
        assert!(
            !t.path().join("a").exists(),
            "the refused install left {} behind",
            t.path().join("a").display()
        );
    }

    /// The same when the failure is part-way down the path to the root itself:
    /// `a` can be created, the next component cannot (it is longer than any
    /// filesystem allows a name to be) — `a` must not be left behind.
    #[cfg(unix)]
    #[test]
    fn a_root_that_cannot_be_created_leaves_no_parents_behind() {
        let t = tempfile::tempdir().unwrap();
        let src = src_pkg(t.path(), "src", "shared");
        let root = t.path().join("a").join("x".repeat(300)).join("packages");
        install(&src, &root).expect_err("an uncreatable root");
        assert!(
            !t.path().join("a").exists(),
            "the refused install left {} behind",
            t.path().join("a").display()
        );
    }

    /// The copy never creates the directory it is copying INTO: if the packages
    /// root is gone — a concurrent install's cleanup removed it after this one
    /// checked it — the copy fails instead of re-creating a root that nothing
    /// recorded or checked.
    #[test]
    fn copying_into_a_root_that_is_gone_fails_and_does_not_recreate_it() {
        let t = tempfile::tempdir().unwrap();
        let src = src_pkg(t.path(), "src", "shared");
        let gone = t.path().join("gone");
        copy_tree(&src, &gone.join(".staging-x")).expect_err("the root is gone");
        assert!(!gone.exists(), "the copy re-created the packages root");
    }

    /// The packages root is created `0700`, and an existing one anyone else can
    /// write is refused at install time — not accepted by the install and then
    /// refused by the next daemon start.
    #[cfg(unix)]
    #[test]
    fn the_packages_root_is_created_private_and_a_shared_one_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let t = tempfile::tempdir().unwrap();
        let src = src_pkg(t.path(), "src", "shared");

        let fresh = t.path().join("fresh/packages");
        install(&src, &fresh).expect("install into a new root");
        for dir in [&fresh, &t.path().join("fresh")] {
            let mode = std::fs::metadata(dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "{} is {mode:o}", dir.display());
        }

        let shared = t.path().join("shared");
        std::fs::create_dir(&shared).unwrap();
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o775)).unwrap();
        let err = install(&src, &shared).expect_err("a group-writable root");
        assert!(matches!(err, InstallError::Filesystem(_)), "{err:?}");
        assert_eq!(
            std::fs::read_dir(&shared).unwrap().count(),
            0,
            "the refused install left something in the root"
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_installed_package_is_writable_by_its_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let t = tempfile::tempdir().unwrap();
        let src = src_pkg(t.path(), "src", "shared");
        std::fs::create_dir_all(src.join("lib")).unwrap();
        std::fs::write(src.join("lib/code.js"), "// code\n").unwrap();
        std::fs::set_permissions(
            src.join("lib/code.js"),
            std::fs::Permissions::from_mode(0o666),
        )
        .unwrap();
        std::fs::set_permissions(src.join("lib"), std::fs::Permissions::from_mode(0o777)).unwrap();

        let dest = install(&src, &t.path().join("pkgs")).expect("install");

        let mut seen = 0;
        let mut pending = vec![dest];
        while let Some(p) = pending.pop() {
            let mode = std::fs::metadata(&p).unwrap().permissions().mode();
            assert_eq!(mode & 0o022, 0, "{} is {:o}", p.display(), mode);
            seen += 1;
            if p.is_dir() {
                pending.extend(std::fs::read_dir(&p).unwrap().map(|e| e.unwrap().path()));
            }
        }
        // The walk saw the tree: the package dir, lib/, the manifest and the code.
        assert_eq!(seen, 4);
    }

    // ---- the instrument, and its own negative controls -----------------------
    //
    // These tests exist because the "left nothing behind" assertions below are
    // only as good as this function. A snapshot that quietly sees nothing makes
    // "the tree is unchanged" pass for every possible bug.

    /// One entry in a filesystem snapshot: path, size, and a content hash.
    ///
    /// The hash is not redundant with the size. A half-written file can have the
    /// size the finished one would have had (a truncated copy that stopped on a
    /// block boundary, a file pre-allocated then partially filled), and without a
    /// hash "the set is equal" would hold across exactly the partial state this
    /// module exists to prevent. mtime is deliberately NOT in the tuple: it makes
    /// the snapshot unstable on fast filesystems — and, unlike what an earlier
    /// version of this comment said, nothing below asks the parent-mtime question
    /// separately. It cannot be asked: staging lives inside the root by design.
    #[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
    struct Entry {
        rel: String,
        len: u64,
        sha: String,
    }

    fn snapshot(root: &Path) -> BTreeSet<Entry> {
        fn walk(base: &Path, dir: &Path, out: &mut BTreeSet<Entry>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for e in entries.flatten() {
                let p = e.path();
                let rel = p
                    .strip_prefix(base)
                    .unwrap_or(&p)
                    .to_string_lossy()
                    .into_owned();
                let Ok(ty) = e.file_type() else { continue };
                if ty.is_dir() {
                    // Directories are recorded too: an empty leftover directory is
                    // exactly the kind of debris a size-only file scan misses.
                    out.insert(Entry {
                        rel: format!("{rel}/"),
                        len: 0,
                        sha: String::new(),
                    });
                    walk(base, &p, out);
                } else {
                    let bytes = std::fs::read(&p).unwrap_or_default();
                    use sha2::{Digest, Sha256};
                    let sha = format!("{:x}", Sha256::digest(&bytes));
                    out.insert(Entry {
                        rel,
                        len: bytes.len() as u64,
                        sha,
                    });
                }
            }
        }
        let mut out = BTreeSet::new();
        walk(root, root, &mut out);
        out
    }

    #[test]
    fn the_snapshot_sees_each_kind_of_difference() {
        // Three DIFFERENT bugs, so three separate assertions: missing a new file,
        // missing a same-size content change, and missing a leftover empty
        // directory are not one failure mode.
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        std::fs::write(root.join("a.txt"), b"hello").unwrap();
        let base = snapshot(root);

        // (1) an extra file
        std::fs::write(root.join("b.txt"), b"x").unwrap();
        assert_ne!(base, snapshot(root), "an added file must be visible");
        std::fs::remove_file(root.join("b.txt")).unwrap();
        assert_eq!(
            base,
            snapshot(root),
            "and removing it must restore equality"
        );

        // (2) SAME SIZE, different content — the case a size-only snapshot misses,
        // and the shape a truncated-then-padded copy would take.
        std::fs::write(root.join("a.txt"), b"world").unwrap();
        assert_ne!(
            base,
            snapshot(root),
            "a same-size content change must be visible, or a partially written \
             file would pass as unchanged"
        );
        std::fs::write(root.join("a.txt"), b"hello").unwrap();
        assert_eq!(base, snapshot(root));

        // (3) an empty leftover directory — debris with no files in it at all
        std::fs::create_dir(root.join("leftover")).unwrap();
        assert_ne!(
            base,
            snapshot(root),
            "an empty leftover directory must be visible; a file-only walk sees \
             nothing here"
        );
    }

    // ---- fixtures -----------------------------------------------------------

    fn manifest_yaml(name: &str) -> String {
        format!(
            "name: {name}\nversion: \"0.1.0\"\nroute_namespace: /api/v1/{name}\n\
             event_module: {name}\ndata_dir: ~/.agent24/os/{name}/\n\
             kernel_capabilities: [events]\nimpl_kind: out_of_process_provider\n\
             spawn:\n  command: bin/{name}\n"
        )
    }

    /// A source package. `dir_name` is deliberately separate from the manifest
    /// name so tests can prove which one decides the installed identity.
    fn src_pkg(root: &Path, dir_name: &str, manifest_name: &str) -> PathBuf {
        let d = root.join(dir_name);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(MANIFEST_FILE), manifest_yaml(manifest_name)).unwrap();
        d
    }

    // ---- the happy path, which is also the CONTROL for every test below ------

    #[test]
    fn a_successful_install_changes_the_tree() {
        // Without this, "the tree is unchanged" in the failure tests is satisfiable
        // by an instrument that sees nothing at all. This is the positive control
        // for the whole file.
        let t = tempfile::tempdir().unwrap();
        let pkgs = t.path().join("packages");
        std::fs::create_dir_all(&pkgs).unwrap();
        let src = src_pkg(t.path(), "src", "cos72");

        let before = snapshot(&pkgs);
        let dest = install(&src, &pkgs).unwrap();
        let after = snapshot(&pkgs);

        assert_ne!(before, after, "a successful install must be visible");
        assert_eq!(dest, pkgs.join("cos72"));
        assert!(dest.join(MANIFEST_FILE).is_file());
    }

    #[test]
    fn the_installed_name_comes_from_the_manifest_not_the_directory() {
        // The manifest is the module's sole identity. Letting the enclosing
        // directory name decide would give a package two identities that can
        // disagree — and the one on disk is the one an operator would trust.
        let t = tempfile::tempdir().unwrap();
        let pkgs = t.path().join("packages");
        let src = src_pkg(t.path(), "looks-like-sin90", "cos72");

        let dest = install(&src, &pkgs).unwrap();
        assert_eq!(dest.file_name().unwrap(), "cos72");
        assert!(!pkgs.join("looks-like-sin90").exists());
    }

    // ---- refusal leaves NOTHING behind ---------------------------------------

    #[test]
    fn an_invalid_manifest_leaves_the_packages_root_untouched() {
        // Asserting `is_err()` would say nothing about the disk. The claim is
        // about the filesystem, so the assertion is about the filesystem.
        let t = tempfile::tempdir().unwrap();
        let pkgs = t.path().join("packages");
        std::fs::create_dir_all(&pkgs).unwrap();
        // Pre-existing content, so "unchanged" is a stronger statement than "empty".
        let existing = src_pkg(&pkgs, "already", "already");

        let bad = t.path().join("bad");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(bad.join(MANIFEST_FILE), "name: [not, a, string]\n").unwrap();

        let before = snapshot(&pkgs);
        let err = install(&bad, &pkgs).unwrap_err();
        assert!(matches!(err, InstallError::Source(_)), "{err}");
        assert_eq!(
            before,
            snapshot(&pkgs),
            "a refused install must leave no trace"
        );
        assert!(
            existing.join(MANIFEST_FILE).is_file(),
            "and must not disturb what was there"
        );
    }

    #[test]
    fn a_failure_partway_through_the_copy_leaves_no_debris() {
        // Injection point notes, because the first two attempts were wrong:
        //
        //  - `chmod 0o555` does not work: root ignores permission bits, so under a
        //    root CI container the install would SUCCEED and the test would fail on
        //    the wrong assertion.
        //  - Pre-occupying the staging path does not work either: `install` clears
        //    a stale staging directory first (a crash must not wedge the next
        //    install), so the injection is removed before it can fire. That is the
        //    code behaving correctly; the test was wrong.
        //
        // What does work, and depends on neither privileges nor timing: a symlink
        // NESTED inside the package. `copy_tree` recurses, so the failure happens
        // after it has already created directories and copied at least one file.
        let t = tempfile::tempdir().unwrap();
        let pkgs = t.path().join("packages");
        std::fs::create_dir_all(&pkgs).unwrap();
        let existing = src_pkg(&pkgs, "already", "already");

        let src = src_pkg(t.path(), "src", "cos72");
        std::fs::create_dir_all(src.join("assets")).unwrap();
        std::fs::write(src.join("assets/ok.bin"), vec![7u8; 64]).unwrap();
        let outside = t.path().join("outside.txt");
        std::fs::write(&outside, b"not part of this package").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, src.join("assets/link.bin")).unwrap();

        let before = snapshot(&pkgs);
        let err = install(&src, &pkgs).unwrap_err();
        assert!(matches!(err, InstallError::Source(_)), "{err}");

        let after = snapshot(&pkgs);
        // The specific debris, named — so a failure says which invariant broke.
        assert!(
            !after.iter().any(|e| e.rel.starts_with(".staging")),
            "staging must be cleaned up: {after:#?}"
        );
        assert!(!pkgs.join("cos72").exists(), "no half-installed package");
        // And the general statement, which also covers debris nobody thought of.
        assert_eq!(
            before, after,
            "the packages root must be byte-for-byte unchanged"
        );
        assert!(existing.join(MANIFEST_FILE).is_file());
    }

    #[test]
    fn two_calls_never_produce_the_same_staging_path() {
        // Two concurrent installs of the SAME package must not share a staging
        // directory. Measured externally, sharing one still ends cleanly — but for
        // the wrong reason: the loser's tree is DELETED by the winner and the loser
        // then fails entirely. That guarantee is borrowed, and it disappears the
        // day this function can resume or retry.
        //
        // This asserts the path directly. An earlier version of this test asserted
        // "repeated installs leave no debris" instead, and a mutation that removed
        // the counter killed nothing — sequential calls clean up after themselves
        // either way, so the test held with and without the property.
        let root = Path::new("/tmp/whatever");
        let a = staging_path(root, "cos72");
        let b = staging_path(root, "cos72");
        assert_ne!(a, b, "the same package must not reuse a staging path");
        // And it must still be inside the packages root, or atomicity is gone.
        assert_eq!(a.parent(), Some(root));
        assert_eq!(b.parent(), Some(root));
    }

    #[test]
    fn a_concurrent_install_is_not_disturbed_by_another_one() {
        // This replaces a test for a sweep that was removed. The sweep could not
        // tell this process's DEAD debris from this process's LIVE sibling — both
        // carry the same pid, only the seq differs — so it deleted a running
        // install's half-written tree. The victim then finished and renamed a
        // package missing a hundred files, and `install` returned Ok: a half
        // package, installed atomically, with no error signal anywhere.
        //
        // The property now is simply that two installs running at once do not
        // interfere. The source is large enough that the two overlap in practice
        // rather than finishing one after the other.
        let t = tempfile::tempdir().unwrap();
        let pkgs = t.path().join("packages");
        std::fs::create_dir_all(&pkgs).unwrap();

        let big = src_pkg(t.path(), "big", "beta");
        std::fs::create_dir_all(big.join("many")).unwrap();
        const FILES: usize = 4000;
        for i in 0..FILES {
            std::fs::write(big.join("many").join(format!("f{i}")), b"x").unwrap();
        }
        let small = src_pkg(t.path(), "small", "alpha");

        let (p1, p2) = (pkgs.clone(), pkgs.clone());
        let h = std::thread::spawn(move || install(&big, &p1));

        // Overlap DETERMINISTICALLY, not by hoping. A sleep, or just starting both,
        // lets the small install finish before the big one has even created its
        // staging directory — and then the test proves nothing. (Measured: with a
        // plain race, restoring the buggy sweep did not fail this test at all.)
        // Wait until the other install is WELL INTO its copy, not merely started.
        //
        // `(1..FILES)` looked like "mid-copy" but releases on the very first file,
        // so the sweep would delete only the handful copied so far — measured, the
        // victim finished 3996 of 4000 and the test's margin was FOUR FILES. A
        // victim that raced ahead and renamed first would have made it 4000 == 4000
        // and gone quietly green. `(FILES/4 .. FILES*3/4)` puts the deletion in the
        // middle: measured ~2560 of 4000, a margin of ~1435.
        //
        // Both directions have to be checked when tuning this: widening the wait
        // until "the big install never finishes" would also turn the red side red.
        //
        // Wait for the other install to be MID-COPY, not merely started. "Its
        // staging directory exists" is not enough: it may already have finished
        // copying, and then nothing can be lost. (Measured: with only that weaker
        // condition, restoring the buggy sweep failed this test 2 runs in 3 — a
        // detector that works two thirds of the time is not a regression test.)
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            assert!(
                std::time::Instant::now() < deadline,
                "the other install never reached a partially-copied state"
            );
            let partial = std::fs::read_dir(&p2)
                .into_iter()
                .flatten()
                .flatten()
                .find(|e| {
                    e.file_name()
                        .to_string_lossy()
                        .starts_with(".staging-beta-")
                })
                .map(|e| {
                    std::fs::read_dir(e.path().join("many"))
                        .map(std::iter::Iterator::count)
                        .unwrap_or(0)
                })
                .is_some_and(|n| (FILES / 4..FILES * 3 / 4).contains(&n));
            if partial {
                break;
            }
            std::thread::yield_now();
        }

        let r2 = install(&small, &p2);
        let r1 = h.join().unwrap();
        assert!(r1.is_ok() && r2.is_ok(), "{r1:?} {r2:?}");

        // The whole point: COUNT the files. "It returned Ok" was true in the broken
        // version too — that is exactly what made the bug invisible.
        let installed = std::fs::read_dir(pkgs.join("beta").join("many"))
            .unwrap()
            .count();
        assert_eq!(
            installed, FILES,
            "a concurrent install must not lose files from another one"
        );
        assert!(pkgs.join("alpha").join(MANIFEST_FILE).is_file());
        // Control: no debris either, so "complete" is not being satisfied by a
        // package that was never staged concurrently at all.
        assert!(
            !snapshot(&pkgs)
                .iter()
                .any(|e| e.rel.starts_with(".staging")),
            "no staging debris"
        );
    }

    #[test]
    fn installing_over_an_existing_package_is_refused_without_touching_it() {
        // The dangerous shape is not the refusal, it is a refusal that has already
        // clobbered the installed copy.
        let t = tempfile::tempdir().unwrap();
        let pkgs = t.path().join("packages");
        std::fs::create_dir_all(&pkgs).unwrap();
        let src = src_pkg(t.path(), "src", "cos72");
        install(&src, &pkgs).unwrap();
        // Mark the installed copy so a silent overwrite is detectable.
        std::fs::write(pkgs.join("cos72").join("MARKER"), b"original").unwrap();

        let before = snapshot(&pkgs);
        let err = install(&src, &pkgs).unwrap_err();
        assert!(matches!(err, InstallError::AlreadyInstalled(_)), "{err}");
        assert_eq!(
            before,
            snapshot(&pkgs),
            "the installed copy must be untouched"
        );
    }

    #[test]
    fn a_symlink_inside_the_package_is_refused() {
        // Same reason `os_discovery` refuses a symlinked manifest: a link lets part
        // of the package live outside the tree that was validated.
        let t = tempfile::tempdir().unwrap();
        let pkgs = t.path().join("packages");
        std::fs::create_dir_all(&pkgs).unwrap();
        let src = src_pkg(t.path(), "src", "cos72");
        let outside = t.path().join("secret.txt");
        std::fs::write(&outside, b"not yours").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, src.join("link.txt")).unwrap();

        let before = snapshot(&pkgs);
        let err = install(&src, &pkgs).unwrap_err();
        assert!(matches!(err, InstallError::Source(_)), "{err}");
        assert_eq!(before, snapshot(&pkgs), "and it must leave nothing behind");
    }

    #[test]
    fn uninstall_removes_it_and_reports_whether_there_was_anything() {
        let t = tempfile::tempdir().unwrap();
        let pkgs = t.path().join("packages");
        std::fs::create_dir_all(&pkgs).unwrap();
        let src = src_pkg(t.path(), "src", "cos72");
        install(&src, &pkgs).unwrap();

        assert!(
            uninstall("cos72", &pkgs).unwrap(),
            "reports that it removed one"
        );
        assert!(!pkgs.join("cos72").exists());
        assert!(
            !uninstall("cos72", &pkgs).unwrap(),
            "removing something absent is not an error, but must be distinguishable"
        );
    }

    /// The name is user-controlled and `remove_dir_all` is irreversible, so this
    /// is written with both controls: the two escapes must FAIL and leave the
    /// victim alone, and the ordinary removal must still work — a `uninstall`
    /// that refused everything would pass the first half on its own.
    #[test]
    fn uninstall_cannot_be_talked_out_of_the_packages_root() {
        let t = tempfile::tempdir().unwrap();
        let pkgs = t.path().join("packages");
        std::fs::create_dir_all(&pkgs).unwrap();
        let victim = t.path().join("VICTIM");
        std::fs::create_dir_all(victim.join("precious")).unwrap();
        let abs = victim.to_string_lossy().into_owned();

        for probe in ["../VICTIM", abs.as_str(), "", ".", "..", "/"] {
            let err = uninstall(probe, &pkgs)
                .expect_err(&format!("{probe:?} must be refused, not resolved"));
            assert!(
                matches!(err, InstallError::InvalidName(_)),
                "{probe:?} → {err:?}"
            );
        }
        assert!(
            victim.join("precious").exists(),
            "an escape deleted a directory outside the packages root"
        );

        // Controls: refusing everything would also satisfy the assertions above.
        let src = src_pkg(t.path(), "src", "cos72");
        install(&src, &pkgs).unwrap();
        assert!(
            uninstall("cos72", &pkgs).unwrap(),
            "a real name still removes"
        );
        assert!(
            !uninstall("cos72", &pkgs).unwrap(),
            "an absent-but-valid name is still Ok(false), not an error"
        );
    }

    /// A dangling symlink where a package directory should be is the case
    /// `Path::exists` gets wrong: it answers `false` for "the target is missing"
    /// just as it does for "there is nothing here". The scanner DOES see the
    /// entry and refuses it, so reporting "was not installed" would leave the
    /// operator with a package they can neither use nor remove.
    #[test]
    fn a_package_entry_that_cannot_be_inspected_is_not_reported_as_absent() {
        let t = tempfile::tempdir().unwrap();
        let pkgs = t.path().join("packages");
        std::fs::create_dir_all(&pkgs).unwrap();
        std::os::unix::fs::symlink(t.path().join("nowhere"), pkgs.join("cos72")).unwrap();

        let removed = uninstall("cos72", &pkgs).expect("a broken link is removable, not invisible");
        assert!(
            removed,
            "it was there — a dangling link is not 'not installed'"
        );
        // Not "the path is gone": the removal RENAMES first, so the original path
        // is empty either way and asserting only that would pass even if the entry
        // were merely moved and left behind. What has to hold is that the packages
        // root is EMPTY — a mutation that dropped the symlink branch of the
        // cleanup passed the weaker assertion, which is how this one got written.
        let left: Vec<_> = std::fs::read_dir(&pkgs)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            left.is_empty(),
            "uninstall reported success and left {left:?} behind"
        );
    }

    /// The failure path has to undo the directory it created. Asserted with the
    /// control that makes it meaningful: when the root ALREADY existed, a failed
    /// install must not delete it.
    #[test]
    fn a_failed_install_does_not_leave_a_packages_root_it_created() {
        let t = tempfile::tempdir().unwrap();
        // A valid manifest with a symlink inside — accepted at parse time, refused
        // during the copy, so failure happens after the root is created.
        let src = src_pkg(t.path(), "src", "cos72");
        std::os::unix::fs::symlink(t.path().join("elsewhere"), src.join("leak")).unwrap();

        let fresh = t.path().join("never-existed");
        assert!(install(&src, &fresh).is_err());
        assert!(
            !fresh.exists(),
            "the packages root was created by a failed install and left behind"
        );

        let preexisting = t.path().join("already-there");
        std::fs::create_dir_all(&preexisting).unwrap();
        assert!(install(&src, &preexisting).is_err());
        assert!(
            preexisting.exists(),
            "cleanup deleted a packages root it did not create"
        );
    }

    #[test]
    fn staging_lives_inside_the_packages_root_so_the_rename_is_atomic() {
        // The property is not "it works", it is WHERE staging is. `/tmp` is a
        // different volume on stock macOS, and a cross-device rename degrades to
        // copy-then-delete — not atomic, and the source of exactly the partial
        // state this module prevents. A test cannot observe atomicity directly, so
        // it observes the thing atomicity depends on.
        let t = tempfile::tempdir().unwrap();
        let pkgs = t.path().join("packages");
        std::fs::create_dir_all(&pkgs).unwrap();
        assert_eq!(
            same_device(&pkgs, &pkgs),
            Some(true),
            "the device check must actually work on this platform"
        );
        // The staging path is derived from the packages root, so it is on the same
        // device by construction. Pin that construction.
        let src = src_pkg(t.path(), "src", "cos72");
        install(&src, &pkgs).unwrap();
        assert_eq!(
            same_device(&pkgs.join("cos72"), &pkgs),
            Some(true),
            "the installed package must have landed on the packages root's device"
        );
    }
}
