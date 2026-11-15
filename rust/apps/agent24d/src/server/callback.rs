use std::sync::Arc as StdArc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use super::os_signature_policy;

/// FU-92 follow-up (security hardening): verify — or, if absent, create — the
/// top-level fallback directory [`callback_root`] names under the shared,
/// world-writable `/tmp`. Its name is a hash of `root`, which is predictable
/// (`root` is just `$HOME/.agent24` or similar), so any other local user could
/// pre-create it — or plant a symlink at that name pointing anywhere — before
/// this daemon ever starts, hijacking the callback socket that would otherwise
/// go under it (eavesdropping on / impersonating the module handshake).
///
/// This check has to happen HERE, before [`agent24_os_proto::endpoint::CallbackDir::create`]
/// touches `<fallback>/run`: that call's own directory setup uses
/// `std::fs::DirBuilder::create` with `recursive(true)`, which walks straight
/// through an existing symlink at an intermediate component (the same as
/// `mkdir -p` — it treats "resolves to a directory" as "already there") rather
/// than refusing it.
///
/// Fails closed: an existing entry that is not exactly a real directory, owned
/// by this process's own euid, mode exactly `0700` (no group/other bits, no
/// special bits) is refused with an actionable message — never silently
/// reused, never chmod'd/fixed in place (unlike `private_dir` in
/// `agent24-os-proto`, which tightens a loose directory of ours; here, on a
/// shared `/tmp`, "ours" itself cannot be trusted without the check), and
/// never retried at some OTHER path.
///
/// # Errors
///
/// A message naming the problem and telling the operator to remove the
/// offending path and restart.
pub(super) fn secure_fallback_dir(path: &std::path::Path) -> Result<(), String> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => std::fs::DirBuilder::new()
            .mode(0o700)
            .create(path)
            .map_err(|e| format!("could not create {}: {e}", path.display())),
        Err(e) => Err(format!("could not check {}: {e}", path.display())),
        Ok(meta) if meta.file_type().is_symlink() => Err(format!(
            "{} is a symlink — another local user may have planted it to intercept this \
             daemon's callback socket; remove it and restart",
            path.display()
        )),
        Ok(meta) if !meta.is_dir() => Err(format!(
            "{} exists and is not a directory; remove it and restart",
            path.display()
        )),
        Ok(meta) => {
            let me = rustix::process::geteuid().as_raw();
            if meta.uid() != me {
                return Err(format!(
                    "{} is owned by uid {}, not this process's {me} — another local user may \
                     have created it to intercept this daemon's callback socket; remove it and \
                     restart",
                    path.display(),
                    meta.uid()
                ));
            }
            // All twelve mode bits, like `agent24-os-proto`'s `check_private`: no
            // group/other bits AND no sticky/setuid/setgid bit either.
            if meta.mode() & 0o7777 != 0o700 {
                return Err(format!(
                    "{} has mode {:04o}, not 0700 — another local user could read or replace \
                     what goes under it; remove it and restart",
                    path.display(),
                    meta.mode() & 0o7777
                ));
            }
            Ok(())
        }
    }
}

/// FU-92: `<root>/run/<pid>/<n>.sock` must fit macOS's 103-byte `sun_path`
/// limit ([`agent24_os_proto::endpoint::MAX_SOCKET_PATH`]). A long `$HOME` (or,
/// for an ephemeral daemon, a long `$TMPDIR`) otherwise fails every
/// out-of-process module's start — with a message that only said "too long",
/// not by how much or what to do about it.
///
/// Rather than lose the whole out-of-process subsystem over a HOME the user
/// did not pick for this reason, fall back to a short path under `/tmp`
/// (a literal `/tmp`, not `$TMPDIR`, which on macOS is itself often long)
/// when `root` would not fit — named by a hash of `root` so distinct state
/// directories, and repeated ephemeral runs, do not collide. Only the
/// transient callback-socket directory moves: `os.json`, `daemon.json`,
/// packages and memory all stay under the real `root`. The fallback
/// directory itself is checked by [`secure_fallback_dir`] before use — never
/// blindly reused — since its name is predictable on a shared `/tmp`.
///
/// Codex ME4-CODEX-DEBT-10 (#528 High): the hash used to name that directory
/// must be [`fallback_dir_hash`], NOT `std::collections::hash_map::DefaultHasher`
/// — its own doc explicitly does not promise the same algorithm across Rust
/// versions (nor even across runs of the same binary — it is randomly seeded
/// unless a `Hasher` is built directly, which this WAS doing, keeping the
/// algorithm fixed but not its stability guarantee), so a toolchain upgrade
/// could silently rename every existing user's fallback directory. That
/// rename is itself harmless (see [`fallback_dir_hash`]'s own doc for why),
/// but there is no reason to accept it when a fixed algorithm is one call
/// away.
///
/// # Errors
///
/// If `root` is too long AND its fallback directory is not safe to use (see
/// [`secure_fallback_dir`]). Does not retry at any other path.
pub(super) fn callback_root(root: &std::path::Path) -> Result<std::path::PathBuf, String> {
    // CallbackDir canonicalizes before binding. Measure that same path: on
    // macOS /var and /tmp expand to /private/var and /private/tmp.
    let resolved = root.canonicalize().unwrap_or_else(|_| root.to_owned());
    let projected = resolved
        .join("run")
        .join(std::process::id().to_string())
        .join(format!("{}.sock", u64::MAX));
    if projected.as_os_str().len() <= agent24_os_proto::endpoint::MAX_SOCKET_PATH {
        return Ok(resolved);
    }
    let fallback =
        std::path::PathBuf::from("/tmp").join(format!("a24-run-{:016x}", fallback_dir_hash(root)));
    secure_fallback_dir(&fallback).map_err(|why| {
        format!(
            "{} is too long for callback sockets (over the {}-byte macOS limit), and its \
             short-path fallback {} is not safe to use: {why}",
            root.display(),
            agent24_os_proto::endpoint::MAX_SOCKET_PATH,
            fallback.display(),
        )
    })?;
    tracing::warn!(
        "{} is too long for callback sockets (over the {}-byte macOS limit); using {} instead \
         for this run. To use {} directly, point Agent24 at a shorter data directory (e.g. a \
         shorter $HOME).",
        root.display(),
        agent24_os_proto::endpoint::MAX_SOCKET_PATH,
        fallback.display(),
        root.display(),
    );
    Ok(fallback)
}

/// A stable (across Rust versions, and across runs of the same binary — see
/// [`callback_root`]'s own doc for why `DefaultHasher` is not either) hash of
/// `root`, folded to 64 bits: the first 8 bytes of its sha256 digest, read as
/// a big-endian `u64`. Only used to NAME the `/tmp` fallback directory
/// [`callback_root`] picks when `root` itself is too long for a socket path
/// — this only has to be stable and collision-resistant enough that two
/// different `root`s (almost) never land on the same fallback name; it is
/// not a security boundary itself ([`secure_fallback_dir`] is what actually
/// keeps another local user out of a name they might guess or collide with).
///
/// A directory named by the OLD `DefaultHasher`-based scheme is simply
/// abandoned by a build using this function instead (its contents are the
/// transient callback-socket directory only — `run/<pid>/<n>.sock` files a
/// live daemon recreates from scratch on every start, per [`callback_root`]'s
/// own doc — so nothing is lost, and [`agent24_os_proto::endpoint::remove_stale`]
/// cleans up anything left behind under the old name once no process holds
/// it open).
#[must_use]
pub(super) fn fallback_dir_hash(root: &std::path::Path) -> u64 {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(root.as_os_str().as_encoded_bytes());
    let mut first8 = [0u8; 8];
    first8.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(first8)
}

/// What out-of-process modules are started with: the callback directory under
/// `root` (stale ones cleared first, FU-56; relocated by [`callback_root`] if
/// `root` itself is too long), this binary as the trampoline, and the
/// daemon's stop grace.
pub(super) fn process_host(
    root: &std::path::Path,
    stop_grace: Duration,
    shutdown: CancellationToken,
) -> Result<crate::domain::ProcessHost, String> {
    let root = &callback_root(root)?;
    for gone in agent24_os_proto::endpoint::remove_stale(root) {
        tracing::info!(
            "removed the callback directory of a daemon that is gone: {}",
            gone.display()
        );
    }
    let callback_dir = agent24_os_proto::endpoint::CallbackDir::create(root)
        .map_err(|e| format!("callback directory: {e}"))?;
    // Checked now, for the longest name a socket there can get, rather than
    // failing every module's start — and then its restarts — one by one: a long
    // `TMPDIR` (an ephemeral daemon's root) can put every socket over the limit.
    // Should not trigger given `callback_root` above, but kept as a safety net
    // — e.g. if `/tmp` itself were ever remapped to something long.
    let longest = callback_dir.path().join(format!("{}.sock", u64::MAX));
    let longest_len = longest.as_os_str().len();
    if longest_len > agent24_os_proto::endpoint::MAX_SOCKET_PATH {
        return Err(format!(
            "callback sockets under {} would be {longest_len} bytes, over the {}-byte macOS \
             limit — point Agent24 at a shorter data directory (e.g. a shorter $HOME) and \
             restart",
            callback_dir.path().display(),
            agent24_os_proto::endpoint::MAX_SOCKET_PATH
        ));
    }
    // This binary, which hands a module over at the top of `main`
    // (`run_as_trampoline_if_asked`), so it needs no arguments of its own. By
    // path: a module restarted after the binary was replaced or moved runs the
    // new one, or fails to start — upgrading the daemon means restarting it.
    let program = std::env::current_exe().map_err(|e| format!("this binary's path: {e}"))?;
    Ok(crate::domain::ProcessHost {
        signature: os_signature_policy(),
        callback_dir: StdArc::new(callback_dir),
        trampoline: agent24_os_proto::launch::Trampoline {
            program,
            args: Vec::new(),
        },
        timings: agent24_os_proto::supervisor::Timings {
            stop_grace,
            ..agent24_os_proto::supervisor::Timings::default()
        },
        supervisors: StdArc::new(crate::domain::Supervisors::default()),
        shutdown,
    })
}
