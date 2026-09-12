//! Domain-OS packages on disk: finding them, and putting them there.
//!
//! Extracted from the daemon binary because **the CLI could not reach it**. The
//! seam was supposed to be "the library API can complete an install on its own,
//! and the CLI only maps arguments onto it" — but `agent24d` is a binary crate, so
//! `agent24 os install` could not call the install mechanism at all. Not "the CLI
//! would have to recompute a path", which is the failure that criterion was
//! written to catch: a stronger version of the same fault, found by trying to
//! satisfy it.
//!
//! Nothing here needs a running daemon. Installing writes files; the daemon reads
//! them at startup. Routing an install through the daemon would have made the tool
//! that installs a module depend on the process that only notices it next time it
//! boots.

pub mod discovery;
pub mod install;

/// The name of the environment variable that overrides the packages root.
///
/// Exported because the two functions below take the override as an ARGUMENT
/// rather than reading it: a caller needs to know which variable to read.
pub const PACKAGES_ROOT_ENV: &str = "A24_OS_PACKAGES";

/// Where installed domain-OS PACKAGES live — deliberately NOT the same root as
/// their data.
///
/// Data lives in `~/.agent24/os/<name>/`, which the module owns and writes to.
/// A package holds the manifest, and the manifest is what DECIDES the module's
/// name, namespace and data directory. Putting the two in one tree would let a
/// module rewrite its own identity at runtime by writing one file into the
/// directory it was handed — so the manifest must live somewhere the module is
/// not given a handle to.
///
/// Reads [`PACKAGES_ROOT_ENV`] for the caller. That override is not a
/// convenience: it is what lets a test install a package into a temp dir and
/// prove the catalogue is read at startup rather than compiled in, WITHOUT
/// rebuilding the binary.
pub fn packages_root(state_dir: &std::path::Path, ephemeral: bool) -> std::path::PathBuf {
    match resolve_packages_root(env_override().as_deref(), Some(state_dir), ephemeral) {
        Ok(root) => root,
        // Unreachable: the only error `resolve` returns is "no state dir and no
        // override", and a state dir was given. Written as a fallback rather than
        // an `expect` so a future error variant cannot turn this into a panic
        // inside a daemon.
        Err(NoPackagesRoot) => state_dir.join("packages"),
    }
}

/// Read [`PACKAGES_ROOT_ENV`]. The only place in this crate that touches the
/// environment, so every rule about WHICH directory wins is decided by a pure
/// function that a test can drive.
pub fn env_override() -> Option<std::ffi::OsString> {
    std::env::var_os(PACKAGES_ROOT_ENV)
}

/// Decide the packages root from inputs, reading nothing.
///
/// The ORDER is the point, and it is the reason this takes `over` as an argument
/// instead of reading the environment itself. The override is consulted FIRST, so
/// a container or CI runner that sets it but has no `HOME` gets the directory it
/// asked for instead of an error about a home directory it deliberately does not
/// have — which is exactly the environment the override is documented for.
///
/// An earlier version read the variable in here. The ordering was then correct
/// but UNGUARDED: moving the override check after the state dir — reinstating the
/// bug verbatim — turned no test red, because a test cannot set a process-global
/// variable safely under a parallel runner and so no test exercised the branch at
/// all. A rule worth writing down is a rule worth being able to break in a test.
pub fn resolve_packages_root(
    over: Option<&std::ffi::OsStr>,
    state_dir: Option<&std::path::Path>,
    ephemeral: bool,
) -> Result<std::path::PathBuf, NoPackagesRoot> {
    if let Some(over) = over {
        return Ok(std::path::PathBuf::from(over));
    }
    if ephemeral {
        // An ephemeral daemon must not read the real user's packages: it is used
        // by tests and by `agent24 chat` with no daemon running, and silently
        // mounting whatever the user happens to have installed would make those
        // runs depend on machine state they never asked about.
        //
        // The name is UNPREDICTABLE, not merely unique. A pid alone is guessable
        // and reused, and on a shared `/tmp` (Linux; macOS gives each user a
        // private `$TMPDIR`) another local user can create the directory before
        // this process looks — at which point the ephemeral daemon scans packages
        // somebody else chose. Today that costs a polluted refusal list; from
        // ME-3b, when a package can name a process to spawn, it is an execution
        // boundary. Note what this does NOT do: it does not create the directory,
        // so it cannot check ownership or mode. A path function is the wrong place
        // for that; see FU-41.
        return Ok(std::env::temp_dir().join(format!("agent24-ephemeral-pkgs-{}", ephemeral_tag())));
    }
    match state_dir {
        Some(dir) => Ok(dir.join("packages")),
        None => Err(NoPackagesRoot),
    }
}

/// Make sure the packages root is a directory **only this user can write**, and
/// say so if it is not.
///
/// # Why this stops being optional at ME-3b-3
///
/// Until the manifest could name a program, a package directory somebody else
/// controlled cost a polluted refusal list. From 3b-3 the manifest carries a
/// `spawn` command, so **whoever can write a package can choose what the daemon
/// executes**. Resolving the path unpredictably (see `ephemeral_tag`) lowers the
/// chance of a guess landing; it does not change what happens if one does.
/// FU-41 said exactly that: *do not treat "the path is unguessable" as fixed.*
///
/// # What it checks, and what it cannot
///
/// - Creates the directory with mode `0700` when it does not exist.
/// - When it does exist: refuses unless it is a real directory (not a symlink),
///   is owned by this process's uid, and is not group- or world-writable.
///
/// It cannot close the TOCTOU window between this check and a later open — a
/// directory can be swapped after it is inspected. Closing that needs the
/// spawning code to hold a descriptor, which is 3b-3's job, not a path
/// function's. Saying so is the point: **a check that names a hazard it does not
/// actually remove is worse than no check, because the name says it was
/// handled.**
///
/// # Errors
///
/// A message an operator can act on: which path, and which of the conditions.
pub fn ensure_packages_root(root: &std::path::Path) -> Result<(), UnsafePackagesRoot> {
    use std::os::unix::fs::DirBuilderExt;

    match std::fs::symlink_metadata(root) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(root)
            .map_err(|e| UnsafePackagesRoot {
                path: root.to_path_buf(),
                why: format!("could not create it: {e}"),
            }),
        _ => check_packages_root(root),
    }
}

/// The checks of [`ensure_packages_root`] on a root that must already exist —
/// **never creating anything**. A missing root is an error here.
///
/// `os install` creates the root and its parents itself, one level at a time,
/// recording each so a failure can take them back; then it checks with this.
/// Checking with `ensure_packages_root` instead let a root that a concurrent
/// install's cleanup had just removed be re-created, recursively, by the
/// check — directories nobody recorded, so nobody could take them back (review
/// of ME3-SUP slice 1, round 3⁗).
///
/// # Errors
///
/// A message an operator can act on: which path, and which of the conditions.
pub fn check_packages_root(root: &std::path::Path) -> Result<(), UnsafePackagesRoot> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let meta = std::fs::symlink_metadata(root).map_err(|e| UnsafePackagesRoot {
        path: root.to_path_buf(),
        why: format!("could not inspect it: {e}"),
    })?;
    // `symlink_metadata`, so a symlink is seen AS a symlink. Following it would
    // mean checking the permissions of the target while the daemon later writes
    // through the link — the two would not be the same object's rights.
    if meta.file_type().is_symlink() {
        return Err(UnsafePackagesRoot {
            path: root.to_path_buf(),
            why: "it is a symlink; the packages root must be a real directory".to_owned(),
        });
    }
    if !meta.is_dir() {
        return Err(UnsafePackagesRoot {
            path: root.to_path_buf(),
            why: "it is not a directory".to_owned(),
        });
    }
    let uid = effective_uid();
    if meta.uid() != uid {
        return Err(UnsafePackagesRoot {
            path: root.to_path_buf(),
            why: format!("owned by uid {} rather than {uid}", meta.uid()),
        });
    }
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o022 != 0 {
        return Err(UnsafePackagesRoot {
            path: root.to_path_buf(),
            why: format!(
                "mode {mode:04o} is writable by group or others; a package there \
                 decides what the daemon executes"
            ),
        });
    }
    Ok(())
}

/// This process's effective uid.
///
/// # A previous version of this function opened a hole in the check it serves
///
/// It created a file in the temp directory and asked the filesystem who owned
/// it. That is `File::create` — it **follows symlinks** and has no `O_EXCL` —
/// and the path was derived from pid and thread id, both low-entropy and
/// guessable. Measured in review: with the probe path pre-created as a symlink
/// to a root-owned file, the function returned **0** while the real euid was
/// 502.
///
/// The consequence ran straight through the check this module exists for: an
/// attacker on a shared `/tmp` pre-creates that path pointing at a file THEY
/// own → this returns THEIR uid → `ensure_packages_root` compares it against the
/// packages root's owner → **their own packages root passes the ownership
/// check** → they `chmod 0700` and the mode check passes too → their package's
/// `spawn` command gets executed. **That is the same shared `/tmp`, the same
/// pre-creation, and the same "guess the path" as the threat FU-41 names — the
/// helper opened a second hole on the very channel it was closing.**
///
/// The general shape, which is why this comment is long: **a total, infallible
/// constant (`geteuid` takes no arguments, has no failure mode, and reads no
/// external input) was replaced by a filesystem operation with
/// attacker-reachable input.** That is not a trade of "one more IO and one more
/// error path" — it is trading a fact for a question somebody else can answer.
///
/// # Why `rustix` and not the `unsafe` this crate's lints forbid
///
/// Review's suggestion was to make an exception and call `geteuid` directly,
/// arguing that "avoid unsafe" had pointed at the less safe implementation.
/// That argument is right about the ORDERING of risks, and `rustix` settles it
/// without paying either cost: it is already in this workspace's dependency
/// tree, `geteuid()` there is a safe wrapper over the same syscall, and no
/// `#[allow]` has to be planted for others to copy.
fn effective_uid() -> u32 {
    rustix::process::geteuid().as_raw()
}

/// The packages root is not somewhere the daemon may safely read packages from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsafePackagesRoot {
    /// The directory inspected.
    pub path: std::path::PathBuf,
    /// Which condition failed, in words an operator can act on.
    pub why: String,
}

impl std::fmt::Display for UnsafePackagesRoot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "refusing to read packages from {}: {}",
            self.path.display(),
            self.why
        )
    }
}

impl std::error::Error for UnsafePackagesRoot {}

/// There is no override and no state directory, so there is nowhere for packages
/// to be. Carries no message of its own: the caller knows which of its own inputs
/// was missing and can say so in its own words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoPackagesRoot;

impl std::fmt::Display for NoPackagesRoot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "no packages directory: neither {PACKAGES_ROOT_ENV} nor a state directory is set"
        )
    }
}

impl std::error::Error for NoPackagesRoot {}

/// A per-process tag that another process cannot guess. Computed once: two calls
/// in one process must agree, or the daemon would scan one directory and a later
/// caller another.
fn ephemeral_tag() -> &'static str {
    static TAG: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    TAG.get_or_init(|| {
        // Entropy without a dependency: the nanosecond the process first asked
        // (unknown to an observer that only sees the pid) mixed with the pid and
        // the address of a stack local, which ASLR moves per process.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| u64::from(d.subsec_nanos()) ^ d.as_secs())
            .unwrap_or(0);
        let local = 0u8;
        let addr = std::ptr::addr_of!(local) as usize as u64;
        format!(
            "{}-{:016x}",
            std::process::id(),
            nanos.wrapping_mul(0x9e37_79b9_7f4a_7c15).rotate_left(31)
                ^ addr.wrapping_mul(0xbf58_476d_1ce4_e5b9)
        )
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};

    /// The precedence, as a table. Every row is reachable because nothing here
    /// reads the environment — which is the whole reason `over` is a parameter.
    /// Moving the override check below the state dir (the bug this ordering fixes)
    /// turns the first row red; when the variable was read inside the function,
    /// that same change turned nothing red anywhere in the workspace.
    #[test]
    fn the_override_wins_and_it_wins_first() {
        let state = Path::new("/s");
        let over = OsStr::new("/o");
        /// One row: the override, the state dir, whether the caller is ephemeral,
        /// and the root that must come out.
        type Case<'a> = (
            Option<&'a OsStr>,
            Option<&'a Path>,
            bool,
            Result<&'a str, NoPackagesRoot>,
        );
        let cases: [Case<'_>; 5] = [
            // The row that matters: override set, NO state dir — a container or CI
            // runner with no HOME. This must not be an error.
            (Some(over), None, false, Ok("/o")),
            // Override beats a state dir that is also present…
            (Some(over), Some(state), false, Ok("/o")),
            // …and beats the ephemeral branch too, so an ephemeral daemon under an
            // explicit override still reads what the operator pointed it at.
            (Some(over), Some(state), true, Ok("/o")),
            // Controls. Without the override the state dir answers…
            (None, Some(state), false, Ok("/s/packages")),
            // …and with neither there is nowhere, rather than a silent default.
            (None, None, false, Err(NoPackagesRoot)),
        ];
        for (over, state_dir, ephemeral, want) in cases {
            let got = resolve_packages_root(over, state_dir, ephemeral);
            match want {
                Ok(path) => assert_eq!(
                    got,
                    Ok(PathBuf::from(path)),
                    "over={over:?} state={state_dir:?} ephemeral={ephemeral}"
                ),
                Err(e) => assert_eq!(
                    got,
                    Err(e),
                    "over={over:?} state={state_dir:?} ephemeral={ephemeral}"
                ),
            }
        }
    }

    #[test]
    fn the_packages_root_is_not_the_data_root() {
        // The separation this module exists for. Data lives in `os/<name>/`, which
        // the module owns and writes to; the manifest — the file that DECIDES the
        // module's name, namespace and data directory — must not live somewhere the
        // module was handed a handle to.
        let state = Path::new("/tmp/a24state");
        let pkgs = resolve_packages_root(None, Some(state), false).unwrap();
        assert_eq!(pkgs, state.join("packages"));
        assert_ne!(pkgs, state.join("os"), "packages must not be the data root");
    }

    #[test]
    fn an_ephemeral_daemon_does_not_read_the_users_packages() {
        // `agent24 chat` with no daemon spins up an ephemeral one. Silently
        // mounting whatever the user happens to have installed would make those
        // runs depend on machine state nobody asked about.
        let state = Path::new("/tmp/a24state");
        let eph = resolve_packages_root(None, Some(state), true).unwrap();
        assert_ne!(
            eph,
            resolve_packages_root(None, Some(state), false).unwrap()
        );
        assert!(!eph.starts_with(state), "{}", eph.display());
    }

    #[test]
    fn the_ephemeral_root_is_in_the_system_temp_dir_and_not_guessable() {
        // Two claims, because a wrong implementation satisfies either one alone: a
        // fixed `/tmp/agent24-ephemeral-pkgs` is in the temp dir and outside the
        // state dir, and a random path under `./` is unguessable.
        let eph = resolve_packages_root(None, Some(Path::new("/tmp/a24state")), true).unwrap();
        assert!(
            eph.starts_with(std::env::temp_dir()),
            "not under the system temp dir: {}",
            eph.display()
        );
        let name = eph.file_name().unwrap().to_string_lossy().into_owned();
        let pid_only = format!("agent24-ephemeral-pkgs-{}", std::process::id());
        assert_ne!(
            name, pid_only,
            "the pid alone is guessable by any local process, and it is reused"
        );
        assert!(
            name.starts_with(&pid_only),
            "the pid is still wanted for a human reading `ls`: {name}"
        );
        // Same process, same directory — a daemon that scanned one path and later
        // resolved another would silently stop seeing what it mounted.
        assert_eq!(
            eph,
            resolve_packages_root(None, Some(Path::new("/other")), true).unwrap()
        );
    }

    /// `packages_root` is the thin wrapper that reads the environment. Its own
    /// behaviour worth asserting is the one thing it does beyond delegating: with
    /// a state dir supplied it can never fail, so it never panics.
    #[test]
    fn the_env_reading_wrapper_always_answers_when_given_a_state_dir() {
        let got = packages_root(Path::new("/s"), false);
        match env_override() {
            Some(over) => assert_eq!(got, PathBuf::from(over)),
            None => assert_eq!(got, PathBuf::from("/s/packages")),
        }
    }
}

#[cfg(test)]
mod root_safety_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn a_missing_root_is_created_private() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("packages");
        ensure_packages_root(&root).expect("creating it");
        // The check alone never creates: a missing root is an error, and stays
        // missing.
        let missing = root.with_file_name("never-made");
        check_packages_root(&missing).expect_err("a missing root");
        assert!(!missing.exists(), "the check created the root");
        let mode = std::fs::metadata(&root).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "created with mode {mode:04o}");
    }

    /// The condition FU-41 is about: a directory anyone can write is a directory
    /// anyone can put a `spawn` command in.
    #[test]
    fn a_world_writable_root_is_refused() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("packages");
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o777)).unwrap();

        let err = ensure_packages_root(&root).expect_err("0777 must be refused");
        assert!(err.why.contains("writable"), "{err}");

        // Control: the same directory, tightened, is accepted — so the refusal is
        // about the mode and not about the path.
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        ensure_packages_root(&root).expect("0700 is fine");
    }

    /// Group-writable is refused too. Written as its own case because `0o022`
    /// covers two bits and a check written for one of them passes half of this.
    #[test]
    fn a_group_writable_root_is_refused() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("packages");
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o770)).unwrap();
        assert!(ensure_packages_root(&root).is_err(), "0770 must be refused");
        // …and a mode that is merely READABLE by others is fine: the hazard is
        // writing, not reading. Without this the rule could tighten into
        // something that refuses ordinary setups and gets switched off.
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        ensure_packages_root(&root).expect("0755 is not a write hazard");
    }

    /// A symlink is refused rather than followed: following it would check the
    /// permissions of the target while the daemon later writes through the link,
    /// so the rights inspected would not be the rights used.
    #[test]
    fn a_symlinked_root_is_refused_not_followed() {
        let t = tempfile::tempdir().unwrap();
        let real = t.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o700)).unwrap();
        let link = t.path().join("packages");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let err = ensure_packages_root(&link).expect_err("a symlinked root must be refused");
        assert!(err.why.contains("symlink"), "{err}");
        // Control: the target itself passes, so the refusal is about the link.
        ensure_packages_root(&real).expect("the real directory is fine");
    }

    #[test]
    fn a_file_where_the_root_should_be_is_refused() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("packages");
        std::fs::write(&root, b"not a directory").unwrap();
        assert!(ensure_packages_root(&root).is_err());
    }

    /// The instrument for the ownership check.
    ///
    /// It also carries the regression for a real defect: the first version of
    /// `effective_uid` created a file at a guessable path with `File::create`
    /// (follows symlinks, no `O_EXCL`) and read its owner. Pre-creating that path
    /// as a symlink to a root-owned file made it return 0. **The helper opened a
    /// second hole on the same channel the module exists to close.**
    ///
    /// The regression is written as a PROPERTY rather than by re-staging that
    /// attack: whatever `effective_uid` returns must not be influenceable by
    /// anything on disk. A syscall satisfies that by construction; the check here
    /// is that the answer is stable and matches a file this process just made.
    #[test]
    fn the_uid_probe_answers_and_cannot_be_told_what_to_say() {
        let uid = effective_uid();
        assert_eq!(
            uid,
            effective_uid(),
            "the answer must not vary between calls"
        );

        // Planting things in the temp directory must not change it. This is the
        // shape of the old defect: the old implementation READ the filesystem to
        // answer, so the filesystem could answer for it.
        let decoy = std::env::temp_dir().join(format!("agent24-uid-probe-{}", std::process::id()));
        let _ = std::fs::remove_file(&decoy);
        let _ = std::os::unix::fs::symlink("/dev/null", &decoy);
        assert_eq!(uid, effective_uid(), "a planted symlink changed the answer");
        let _ = std::fs::remove_file(&decoy);
        // A directory this process just made must be owned by it — that is the
        // only claim the probe makes, and it is checkable.
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("mine");
        ensure_packages_root(&root).unwrap();
        use std::os::unix::fs::MetadataExt;
        assert_eq!(std::fs::metadata(&root).unwrap().uid(), uid);
    }
}
