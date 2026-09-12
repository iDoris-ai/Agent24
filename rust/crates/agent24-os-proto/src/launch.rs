//! ME-3b-3 — turning a manifest's `spawn` command into a running child.
//!
//! Three things happen here and they are separable on purpose:
//!
//! 1. [`resolve`] — decide WHICH program, and refuse anything outside the
//!    package. This is where the load-bearing check lives; the manifest's own
//!    `SpawnCommand::validate` is purely lexical and says so.
//! 2. [`mint_token`] — a fresh secret for this one handshake.
//! 3. [`spawn`] — start it, in its own process group, with a cleared
//!    environment, the proxy's listening socket as fd 3, and its output drained
//!    into the log. What comes back is a [`ModuleProcess`], the only holder of
//!    the child.
//!
//! Supervision (backoff, circuit breaker, startup timeout) is not here: what
//! "ready" means depends on the handshake (ME3-SUP's second and third slices).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use agent24_domain::SpawnCommand;

use crate::drain::Generation;
use crate::supervise::ModuleProcess;

/// The environment a module is started with — the launch half of the wire
/// contract (SPEC-ME3 §1).
///
/// `A24_LISTEN_FD`: the fd of the listening socket the module serves its HTTP
/// on — always [`LISTEN_FD`]. The module does not choose an address.
pub const ENV_LISTEN_FD: &str = "A24_LISTEN_FD";
/// `A24_CALLBACK_SOCK`: the path of the kernel's callback socket, where the
/// module connects and sends `initialize` first.
pub const ENV_CALLBACK_SOCK: &str = "A24_CALLBACK_SOCK";
/// `A24_HANDSHAKE_TOKEN`: this process's one-shot handshake secret.
pub const ENV_HANDSHAKE_TOKEN: &str = "A24_HANDSHAKE_TOKEN";
/// `A24_DATA_DIR`: the module's own data directory.
pub const ENV_DATA_DIR: &str = "A24_DATA_DIR";

/// The fd the listening socket arrives on — 3, the first after stdio, as
/// systemd's socket activation does, so a module written for that convention
/// needs no change.
pub const LISTEN_FD: i32 = 3;

/// The variables a module inherits from the daemon. Everything else is cleared
/// (SPEC-ME3 §5: spawn clears inherited fds and environment) — the daemon's
/// environment can carry credentials (a provider's API key, a cloud token) that
/// are the kernel's, not a module's. `LC_*` is inherited as a prefix. ⚖️ The
/// list is a choice: enough for an interpreter to find itself and a locale, and
/// nothing that names a credential.
pub const INHERITED_ENV: &[&str] = &["PATH", "HOME", "LANG", "TZ", "TMPDIR", "USER"];

fn inherited(name: &std::ffi::OsStr) -> bool {
    name.to_str()
        .is_some_and(|n| INHERITED_ENV.contains(&n) || n.starts_with("LC_"))
}

/// Why a module could not be started.
#[derive(Debug)]
pub enum LaunchError {
    /// The command could not be resolved to a program.
    Unresolved(String),
    /// It resolved to something outside the package directory.
    EscapesPackage { resolved: PathBuf, package: PathBuf },
    /// The OS refused to start it.
    Spawn(std::io::Error),
    /// A fresh token could not be produced. **Not** recoverable by reusing an
    /// old one — see [`mint_token`].
    NoEntropy(std::io::Error),
    /// Something in the package tree is not owned by this user or is writable
    /// by others — someone else could replace what is about to run.
    UnsafeOwnership(PathBuf),
    /// Something in the package tree could not be inspected (unreadable, or it
    /// vanished during the walk). Not an ownership verdict: the tree could not
    /// be checked at all.
    Inspect {
        path: PathBuf,
        error: std::io::Error,
    },
    /// The package tree has more than [`MAX_PACKAGE_ENTRIES`] entries — too
    /// many to check at every start.
    TooLarge { package: PathBuf },
}

impl std::fmt::Display for LaunchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unresolved(s) => write!(f, "{s}"),
            Self::EscapesPackage { resolved, package } => write!(
                f,
                "the spawn command resolves to {}, which is outside the package at {}",
                resolved.display(),
                package.display()
            ),
            Self::Spawn(e) => write!(f, "could not start the module process: {e}"),
            Self::NoEntropy(e) => write!(f, "could not mint a handshake token: {e}"),
            Self::Inspect { path, error } => {
                write!(f, "could not inspect {}: {error}", path.display())
            }
            Self::TooLarge { package } => write!(
                f,
                "the package at {} has more than {MAX_PACKAGE_ENTRIES} entries; too many \
                 to check at every start",
                package.display()
            ),
            Self::UnsafeOwnership(p) => write!(
                f,
                "{} is not owned by this user, or is writable by group or others; \
                 refusing to run a module from it",
                p.display()
            ),
        }
    }
}

impl std::error::Error for LaunchError {}

/// Decide which program a `spawn` command names.
///
/// # The check the manifest's own validation cannot do
///
/// `SpawnCommand::validate` is **lexical**: it refuses `/bin/sh` and `..`, and
/// its docs say plainly that it establishes only "the manifest contains no
/// spelled-out path escape". Two things defeat reading it as more than that: a
/// bare name resolves through `PATH` (deliberate — it is what lets `node` work),
/// and **a symlink inside the package is invisible to a lexical check**.
///
/// So the load-bearing check is here, where the path is real: a relative command
/// is canonicalised and must still lie under the canonicalised package
/// directory. `bin/node` pointing at `/bin/sh` is refused at this point, and
/// only at this point.
///
/// A bare name (no separator) is looked up on `PATH` and is NOT subject to that
/// rule — by design. It is how a Node or Python module names its interpreter,
/// and pretending otherwise would push every such module into a wrapper script,
/// which is the indirection this whole field exists to avoid.
///
/// # Errors
///
/// # Who may have written it
///
/// The whole package tree — every file and directory under the package
/// directory, the directory itself included — must be owned by this user and
/// not writable by group or others, **whatever the command is**. A bare
/// interpreter (`node server.js`) runs code from the package just as surely as
/// `bin/mod` does, and which argument is the code cannot be told from argv; the
/// first version checked only the path to a relative program and let `node
/// server.js` through with a world-writable package (review of SUP-1, round 1).
/// Symlinks are checked for owner only (their mode means nothing). The check
/// narrows the window, it cannot close it — the check and the `exec` are two
/// steps (FU-41) — and it costs a walk of the tree at every spawn.
///
/// # Errors
///
/// [`LaunchError::Unresolved`], [`LaunchError::EscapesPackage`] or
/// [`LaunchError::UnsafeOwnership`].
pub fn resolve(spawn: &SpawnCommand, package_dir: &Path) -> Result<PathBuf, LaunchError> {
    let command = Path::new(&spawn.command);
    let package = package_dir.canonicalize().map_err(|e| {
        LaunchError::Unresolved(format!(
            "the package directory {} could not be resolved: {e}",
            package_dir.display()
        ))
    })?;
    check_tree(&package)?;
    // A bare name has no separator. `Path::components` would normalise away a
    // leading `./`, so the test is on the raw string.
    if !spawn.command.contains(std::path::MAIN_SEPARATOR) {
        return which(&spawn.command).ok_or_else(|| {
            LaunchError::Unresolved(format!(
                "spawn.command {:?} was not found on PATH",
                spawn.command
            ))
        });
    }

    let resolved = package.join(command).canonicalize().map_err(|e| {
        LaunchError::Unresolved(format!(
            "spawn.command {:?} could not be resolved inside the package: {e}",
            spawn.command
        ))
    })?;
    if !resolved.starts_with(&package) {
        return Err(LaunchError::EscapesPackage { resolved, package });
    }
    Ok(resolved)
}

/// Most entries a package tree may have and still be started: the tree is
/// walked at every start, and an unbounded walk is a start that can take as
/// long as the package's author likes. ⚖️
pub const MAX_PACKAGE_ENTRIES: usize = 200_000;

/// Every entry under `root`, `root` included, is owned by this user and not
/// writable by group or others (symlinks: owner only). Does not follow
/// symlinks, so a link pointing out of the tree is not walked into and a link
/// loop is not a loop here.
fn check_tree(root: &Path) -> Result<(), LaunchError> {
    use std::os::unix::fs::MetadataExt;
    let me = rustix::process::geteuid().as_raw();
    let inspect = |path: &Path| {
        let path = path.to_owned();
        move |error| LaunchError::Inspect { path, error }
    };
    // Counted when an entry is FOUND, not when it is visited: counting on the
    // way out let one flat directory of millions of entries be read into
    // `pending` whole before the limit was ever consulted (review of SUP-1,
    // round 3). So `pending` never holds more than the limit either.
    let mut pending = vec![root.to_owned()];
    let mut found = 1usize;
    while let Some(path) = pending.pop() {
        let meta = std::fs::symlink_metadata(&path).map_err(inspect(&path))?;
        let writable_by_others = !meta.file_type().is_symlink() && meta.mode() & 0o022 != 0;
        if meta.uid() != me || writable_by_others {
            return Err(LaunchError::UnsafeOwnership(path));
        }
        if meta.is_dir() {
            for entry in std::fs::read_dir(&path).map_err(inspect(&path))? {
                found += 1;
                if found > MAX_PACKAGE_ENTRIES {
                    return Err(LaunchError::TooLarge {
                        package: root.to_owned(),
                    });
                }
                pending.push(entry.map_err(inspect(&path))?.path());
            }
        }
    }
    Ok(())
}

/// Find `name` on `PATH`, the way `execvp` would — except that relative
/// entries are skipped. A relative entry would be checked here against the
/// daemon's working directory and then executed from the package directory
/// (the child's), two different files (review of SUP-1, round 1).
fn which(name: &str) -> Option<PathBuf> {
    which_in(name, &std::env::var_os("PATH")?)
}

fn which_in(name: &str, path: &std::ffi::OsStr) -> Option<PathBuf> {
    std::env::split_paths(path)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable_file(candidate))
}

fn is_executable_file(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// How many bytes of entropy a handshake token carries.
pub const TOKEN_BYTES: usize = 32;

/// Mint a token for ONE handshake.
///
/// # Why this returns an error instead of a fallback
///
/// The token is what tells the kernel that the process talking to it is the one
/// it started. What makes the naive `!=` comparison in the handshake acceptable
/// is not that the token is "one-shot" as a description — it is that **each
/// secret can be measured exactly once** (a byte-at-a-time timing attack needs
/// on the order of 256×len measurements). That property has two preconditions,
/// and this function owns the first: a fresh token per spawn.
///
/// The second — **not reusing a token when a failed handshake is retried** —
/// belongs to the supervisor, and it is the one that fails silently. See FU-44.
///
/// A weaker fallback (time, pid, an address) would keep the daemon running while
/// removing the property the comparison depends on, and nothing downstream could
/// tell. Refusing to start is the honest outcome.
///
/// # Errors
///
/// [`LaunchError::NoEntropy`] when the system source cannot be read.
pub fn mint_token() -> Result<String, LaunchError> {
    use std::io::Read;

    let mut bytes = [0u8; TOKEN_BYTES];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(LaunchError::NoEntropy)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// The program a module is started THROUGH: it marks every fd from 4 up
/// close-on-exec and then execs the module (see
/// [`run_as_trampoline_if_asked`]). The host binary — the daemon — must call
/// that function first thing in `main`. It is run as `program args…
/// --a24-exec-module <module program> <module args…>` with
/// `A24_TRAMPOLINE=1`.
///
/// # Why a trampoline
///
/// A module must receive fds 0–3 and nothing else. Marking the daemon's own fds
/// close-on-exec before the fork is not enough: another thread can open an fd
/// without the flag between that and the fork (on macOS the standard library
/// itself creates a spawn's pipes and only then flags them — measured, 1 in 60
/// runs of this crate's tests, a module saw another spawn's pipe). Closing the
/// window needs the child's side, between fork and exec, which is `unsafe` code
/// in the daemon; a trampoline gets the child's side without it. It starts as a
/// fresh process with one thread, so nothing can open an fd behind its back
/// while it flags them, and `exec` does the closing (user decision, 2026-09-12,
/// over an audited `unsafe` block and over accepting the window).
#[derive(Debug, Clone)]
pub struct Trampoline {
    pub program: PathBuf,
    pub args: Vec<std::ffi::OsString>,
}

/// Set (to `1`) on a process started as a module's trampoline. Half of what
/// activates it; the other half is [`TRAMPOLINE_ARG`] in its argv. Either alone
/// is an ordinary start — a stray variable in a service's environment must not
/// turn the daemon into something that execs whatever follows (review of
/// SUP-1, round 3).
pub const ENV_TRAMPOLINE: &str = "A24_TRAMPOLINE";

/// Marks, in the trampoline's argv, where the module's program and its
/// arguments begin: `<trampoline args…> --a24-exec-module <program> <args…>`.
/// The module's arguments travel as argv elements of their own, as they would
/// to the module directly — not packed into one variable, where Linux's limit
/// on a single string (128 KiB) would refuse a command line that is legal
/// when passed as separate arguments (review of SUP-1, round 3).
pub const TRAMPOLINE_ARG: &str = "--a24-exec-module";

/// Where this process's open fds can be listed.
#[cfg(target_os = "linux")]
const FD_DIR: &str = "/proc/self/fd";
#[cfg(not(target_os = "linux"))]
const FD_DIR: &str = "/dev/fd";

/// Where `close_fds`'s last-resort scan stops (exclusive): an fd at or above
/// it is flagged only if the library took a complete path.
const FALLBACK_SCAN_END: i32 = 65_536;

/// The first open fd at or above `bound`, from a complete listing of
/// [`FD_DIR`] — any error while listing, including part-way, is an error.
fn open_fd_at_or_above(bound: i32) -> std::io::Result<Option<i32>> {
    for entry in std::fs::read_dir(FD_DIR)? {
        let name = entry?.file_name();
        if let Some(fd) = name.to_str().and_then(|n| n.parse::<i32>().ok())
            && fd >= bound
        {
            return Ok(Some(fd));
        }
    }
    Ok(None)
}

/// If this process was started as a module's trampoline, become the module:
/// mark every fd from 4 up close-on-exec, check that from a complete listing of
/// its own fds, drop the trampoline marker, and exec the module's program with
/// its arguments. Otherwise return at once.
///
/// **Call it first thing in the host's `main`**, before any thread exists —
/// that is what makes flagging the fds race-free. If it cannot guarantee the
/// flagging, or the exec fails, the process exits with 127 and says why on
/// stderr, which the kernel logs as the module's.
pub fn run_as_trampoline_if_asked() {
    use std::os::unix::process::CommandExt;
    // Looked up by iterating: the CLI's scan of daemon sources looks for
    // `env::var("…")` to learn what a service manager must pass through, and
    // this one is set by the daemon for its own child, never by a user.
    if !std::env::vars_os().any(|(k, v)| k == ENV_TRAMPOLINE && v == "1") {
        return;
    }
    let mut argv = std::env::args_os().skip_while(|a| a != TRAMPOLINE_ARG);
    if argv.next().is_none() {
        return; // the variable alone: an ordinary start
    }
    let Some(program) = argv.next() else {
        eprintln!("agent24: the module trampoline was started without a program");
        std::process::exit(127);
    };
    close_fds::set_fds_cloexec_threadsafe(LISTEN_FD + 1, &[]);
    // `close_fds` covers the whole range when `close_range` works (Linux) or
    // when it can walk FD_DIR; otherwise — and silently, even part-way through
    // a walk that failed — it falls back to a scan that stops at fd 65 535
    // (review of SUP-1, rounds 3 and 3′). So which path it took is not
    // trusted: this process lists its own fds, completely, and refuses if the
    // listing fails or shows an fd the fallback could have missed. With no fd
    // at or above that bound, every path flagged everything.
    match open_fd_at_or_above(FALLBACK_SCAN_END) {
        Ok(None) => {}
        Ok(Some(fd)) => {
            eprintln!(
                "agent24: fd {fd} is open, above what can be verified as close-on-exec; \
                 refusing to start a module that might inherit it"
            );
            std::process::exit(127);
        }
        Err(e) => {
            eprintln!(
                "agent24: cannot list this process's fds ({FD_DIR}: {e}); refusing to \
                 start a module without being able to keep the daemon's fds from it"
            );
            std::process::exit(127);
        }
    }
    let error = std::process::Command::new(&program)
        .args(argv)
        .env_remove(ENV_TRAMPOLINE)
        // macOS CoreFoundation writes this into the environment of a process
        // that initializes it — the trampoline sometimes does — and it would
        // pass to the module as the one variable the contract does not name.
        // Seen as a test failure only in some runs, which is the reason to take
        // it out here rather than list it as allowed.
        .env_remove("__CF_USER_TEXT_ENCODING")
        .exec();
    eprintln!(
        "agent24: could not start the module program {}: {error}",
        std::path::Path::new(&program).display()
    );
    std::process::exit(127);
}

/// Everything [`spawn`] needs besides the generation.
#[derive(Debug)]
pub struct LaunchSpec<'a> {
    /// The module's name, for its log lines.
    pub name: &'a str,
    pub command: &'a SpawnCommand,
    pub package_dir: &'a Path,
    pub data_dir: &'a Path,
    /// Where the kernel listens for this process's callback connection.
    pub callback_sock: &'a Path,
    /// What the module is started through (see [`Trampoline`]).
    pub trampoline: &'a Trampoline,
    /// The socket the module serves its HTTP on. Passed as fd [`LISTEN_FD`]
    /// and closed in this process once the child has it — so when the module
    /// dies, a connection to its port is refused at once instead of queueing in
    /// a backlog nobody will ever accept from.
    pub listener: std::net::TcpListener,
}

/// Start a module for `generation`.
///
/// # What the child gets
///
/// - **Its own process group**, so a stop can signal the whole tree: a module
///   written in a scripting language routinely starts helpers, and killing only
///   the pid we hold leaves them running.
/// - **A cleared environment**: [`INHERITED_ENV`] plus the four `A24_*`
///   variables. The token travels in the environment, not on the command line,
///   because arguments are world-readable through `ps`.
/// - **fds 0–3 only**: stdin is `/dev/null`, stdout and stderr are pipes the
///   kernel drains, fd 3 is the listening socket, and the [`Trampoline`] flags
///   everything else close-on-exec before the module's program starts.
///
/// Output is drained whether or not anyone reads the log: a pipe nobody reads
/// fills at about 64 KiB, and a module blocked writing a log line looks exactly
/// like one that hung — it would be killed for a startup timeout it did not
/// cause.
///
/// The package-tree check runs on a blocking thread: it is a walk of the whole
/// tree, and must not stall the runtime it is called from.
///
/// # Errors
///
/// [`LaunchError`] — resolution, ownership, entropy, or the OS refusing.
pub async fn spawn(
    spec: LaunchSpec<'_>,
    generation: Arc<Generation>,
) -> Result<ModuleProcess, LaunchError> {
    use command_fds::{CommandFdExt, FdMapping};

    let program = {
        let (command, package) = (spec.command.clone(), spec.package_dir.to_owned());
        tokio::task::spawn_blocking(move || resolve(&command, &package))
            .await
            .map_err(|e| LaunchError::Spawn(std::io::Error::other(e)))??
    };
    let token = mint_token()?;

    let mut cmd = tokio::process::Command::new(&spec.trampoline.program);
    cmd.args(&spec.trampoline.args)
        .arg(TRAMPOLINE_ARG)
        .arg(&program)
        .args(&spec.command.args)
        .current_dir(spec.package_dir)
        .env_clear()
        .envs(std::env::vars_os().filter(|(k, _)| inherited(k)))
        .env(ENV_LISTEN_FD, LISTEN_FD.to_string())
        .env(ENV_CALLBACK_SOCK, spec.callback_sock)
        .env(ENV_HANDSHAKE_TOKEN, &token)
        .env(ENV_DATA_DIR, spec.data_dir)
        .env(ENV_TRAMPOLINE, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // 0 means "a new group whose id is this child's pid". The trampoline
        // execs in place, so the module keeps that pid and that group.
        .process_group(0);
    cmd.fd_mappings(vec![FdMapping {
        parent_fd: spec.listener.into(),
        child_fd: LISTEN_FD,
    }])
    .map_err(|e| LaunchError::Spawn(std::io::Error::other(e)))?;

    let mut child = cmd.spawn().map_err(LaunchError::Spawn)?;
    // The command owns our copy of the listener; dropping it closes that copy.
    drop(cmd);
    let mut drains = Vec::new();
    if let Some(out) = child.stdout.take() {
        drains.push(tokio::spawn(drain_output(
            out,
            spec.name.to_owned(),
            "stdout",
        )));
    }
    if let Some(err) = child.stderr.take() {
        drains.push(tokio::spawn(drain_output(
            err,
            spec.name.to_owned(),
            "stderr",
        )));
    }
    ModuleProcess::new(child, generation, token, drains).map_err(LaunchError::Spawn)
}

/// The test binary is its own trampoline: started with `--exact` on
/// `launch::tests::trampoline_host`, it runs only that test, which becomes the
/// module (and is a no-op in an ordinary test run). The `--` makes libtest
/// take the marker, the program and its arguments as test-name filters, which
/// match nothing else under `--exact`.
///
/// A module argument equal to another test's full name would select that test
/// too; `start_with` in the tests refuses arguments containing `::tests::`, so
/// no test here can pass one by accident.
///
/// **Not single-threaded**, unlike the daemon's `main`: libtest runs the test
/// on a worker thread while its main thread waits. The waiting thread opens no
/// fds, so the flagging is not raced in practice; the production path is
/// pinned separately by `agent24d`'s `tests/trampoline.rs`, which runs the
/// real binary with a flagless inherited fd.
#[cfg(test)]
#[allow(clippy::expect_used)]
pub(crate) fn test_trampoline() -> Trampoline {
    Trampoline {
        program: std::env::current_exe().expect("the test binary's path"),
        args: [
            "--exact",
            "launch::tests::trampoline_host",
            "--test-threads=1",
            "--nocapture",
            "-q",
            "--",
        ]
        .into_iter()
        .map(Into::into)
        .collect(),
    }
}

/// Longest line of module output logged; the rest of the line is dropped and
/// the line marked as cut. ⚖️
pub const MAX_LOG_LINE: usize = 4096;

/// Most lines of one stream logged per second; beyond it lines are counted and
/// dropped, and the count is logged when the second ends. ⚖️ A module must not
/// be able to fill the daemon's log, or its disk.
pub const LOG_LINES_PER_SECOND: u32 = 200;

/// Read a module's output to its end, logging it line by line within
/// [`MAX_LOG_LINE`] and [`LOG_LINES_PER_SECOND`].
async fn drain_output<R>(stream: R, module: String, which: &'static str)
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncBufReadExt;
    let mut reader = tokio::io::BufReader::new(stream);
    let mut lines = Lines::default();
    let mut limit = RateLimit::new(std::time::Instant::now());
    loop {
        let chunk = match reader.fill_buf().await {
            Ok([]) | Err(_) => break,
            Ok(chunk) => chunk,
        };
        let taken = chunk.len();
        for line in lines.push(chunk) {
            log_line(&module, which, &line, &mut limit);
        }
        reader.consume(taken);
    }
    if let Some(line) = lines.finish() {
        log_line(&module, which, &line, &mut limit);
    }
    if let Some(dropped) = limit.flush() {
        tracing::warn!(target: "agent24::module", module, stream = which, dropped, "module output dropped (rate limit)");
    }
}

fn log_line(module: &str, which: &'static str, line: &Line, limit: &mut RateLimit) {
    let now = std::time::Instant::now();
    if let Some(dropped) = limit.roll(now) {
        tracing::warn!(target: "agent24::module", module, stream = which, dropped, "module output dropped (rate limit)");
    }
    if limit.admit() {
        tracing::info!(
            target: "agent24::module",
            module,
            stream = which,
            cut = line.cut,
            "{}",
            String::from_utf8_lossy(&line.text)
        );
    }
}

/// One line of output, at most [`MAX_LOG_LINE`] bytes of it.
#[derive(Debug, PartialEq, Eq)]
struct Line {
    text: Vec<u8>,
    /// Bytes past [`MAX_LOG_LINE`] were dropped.
    cut: bool,
}

/// Splits a byte stream into [`Line`]s, holding at most [`MAX_LOG_LINE`] bytes
/// of an unfinished line however long it runs. Pure, so it is tested without a
/// process.
#[derive(Debug, Default)]
struct Lines {
    current: Vec<u8>,
    cut: bool,
}

impl Lines {
    fn push(&mut self, mut bytes: &[u8]) -> Vec<Line> {
        let mut done = Vec::new();
        while let Some(i) = bytes.iter().position(|&b| b == b'\n') {
            self.take(&bytes[..i]);
            done.push(Line {
                text: std::mem::take(&mut self.current),
                cut: std::mem::take(&mut self.cut),
            });
            bytes = &bytes[i + 1..];
        }
        self.take(bytes);
        done
    }

    fn take(&mut self, bytes: &[u8]) {
        let room = MAX_LOG_LINE - self.current.len();
        if bytes.len() > room {
            self.cut = true;
        }
        self.current
            .extend_from_slice(&bytes[..bytes.len().min(room)]);
    }

    /// The last line, if the stream ended without a newline.
    fn finish(self) -> Option<Line> {
        (!self.current.is_empty() || self.cut).then_some(Line {
            text: self.current,
            cut: self.cut,
        })
    }
}

/// At most [`LOG_LINES_PER_SECOND`] per one-second window. Holds no clock.
#[derive(Debug)]
struct RateLimit {
    window: std::time::Instant,
    admitted: u32,
    dropped: u64,
}

impl RateLimit {
    fn new(now: std::time::Instant) -> Self {
        Self {
            window: now,
            admitted: 0,
            dropped: 0,
        }
    }

    /// Start a new window if the current one is over; returns the lines the
    /// old one dropped, if any.
    fn roll(&mut self, now: std::time::Instant) -> Option<u64> {
        if now.duration_since(self.window) < std::time::Duration::from_secs(1) {
            return None;
        }
        self.window = now;
        self.admitted = 0;
        self.flush()
    }

    fn admit(&mut self) -> bool {
        if self.admitted < LOG_LINES_PER_SECOND {
            self.admitted += 1;
            true
        } else {
            self.dropped += 1;
            false
        }
    }

    fn flush(&mut self) -> Option<u64> {
        (self.dropped > 0).then(|| std::mem::take(&mut self.dropped))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn pkg() -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("bin")).unwrap();
        t
    }

    fn exe(path: &Path, body: &str) {
        std::fs::write(path, body).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn cmd(command: &str, args: &[&str]) -> SpawnCommand {
        SpawnCommand {
            command: command.to_owned(),
            args: args.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    /// **The check the manifest's lexical validation cannot do**, and the reason
    /// this function exists.
    ///
    /// `bin/node` passes `SpawnCommand::validate` — no absolute path, no `..` —
    /// while pointing at `/bin/sh`. A package that could do that would be
    /// reviewed by reading it and still run something else.
    #[test]
    fn a_symlink_inside_the_package_that_points_out_is_refused() {
        let t = pkg();
        let link = t.path().join("bin/node");
        std::os::unix::fs::symlink("/bin/sh", &link).unwrap();

        let spawn = cmd("bin/node", &[]);
        // It passes the manifest's own check…
        spawn.validate().expect("lexically it is clean");
        // …and is refused here, where the path is real.
        let err = resolve(&spawn, t.path()).expect_err("must be refused");
        assert!(matches!(err, LaunchError::EscapesPackage { .. }), "{err:?}");

        // Control: a real program at the same path resolves. Without this the
        // refusal above could be "symlinks never resolve" rather than "this one
        // leaves the package".
        std::fs::remove_file(&link).unwrap();
        exe(&link, "#!/bin/sh\nexit 0\n");
        let ok = resolve(&spawn, t.path()).expect("a real file inside the package");
        assert!(ok.starts_with(t.path().canonicalize().unwrap()));
    }

    /// A symlink that stays inside the package is fine — the rule is about
    /// leaving, not about links. Without this case the implementation could be
    /// "refuse all symlinks" and pass everything above.
    #[test]
    fn a_symlink_that_stays_inside_the_package_is_allowed() {
        let t = pkg();
        exe(&t.path().join("bin/real"), "#!/bin/sh\nexit 0\n");
        std::os::unix::fs::symlink("real", t.path().join("bin/alias")).unwrap();
        let ok = resolve(&cmd("bin/alias", &[]), t.path()).expect("stays inside");
        assert!(ok.ends_with("bin/real"), "{}", ok.display());
    }

    /// A bare name goes to `PATH` and is deliberately NOT held to the
    /// package-containment rule — that is how a Node or Python module names its
    /// interpreter. Holding it to that rule would push every such module into a
    /// wrapper script, which is the indirection the `spawn` field exists to
    /// avoid.
    #[test]
    fn a_bare_name_resolves_through_path_and_may_live_outside_the_package() {
        let t = pkg();
        let resolved = resolve(&cmd("sh", &[]), t.path()).expect("sh is on PATH");
        assert!(resolved.is_absolute(), "{}", resolved.display());
        assert!(
            !resolved.starts_with(t.path()),
            "the interpreter is outside the package, and that is the point"
        );
    }

    #[test]
    fn a_bare_name_that_is_not_on_path_is_a_clear_refusal() {
        let t = pkg();
        let err = resolve(&cmd("definitely-not-a-real-program-xyz", &[]), t.path())
            .expect_err("must not resolve");
        assert!(matches!(err, LaunchError::Unresolved(_)), "{err:?}");
    }

    /// The property the handshake's naive comparison rests on: **a fresh secret
    /// per spawn**. Not "usually different" — this is checked as a set.
    #[test]
    fn every_token_is_new() {
        let n = 64;
        let tokens: std::collections::BTreeSet<String> =
            (0..n).map(|_| mint_token().unwrap()).collect();
        assert_eq!(tokens.len(), n, "a token repeated within {n} draws");
        // And each is the full width — a short token is a weak one, and length
        // is the part a bad entropy path would silently change.
        for t in &tokens {
            assert_eq!(t.len(), TOKEN_BYTES * 2, "{t}");
            assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
        }
    }

    fn listener() -> std::net::TcpListener {
        std::net::TcpListener::bind("127.0.0.1:0").unwrap()
    }

    /// When this test binary is started as a module's trampoline (see
    /// `test_trampoline`), this is the test it runs, and it becomes the module.
    /// In an ordinary run it returns at once.
    #[test]
    fn trampoline_host() {
        run_as_trampoline_if_asked();
    }

    /// Start `command` from package `t` for a fresh generation, with a fresh
    /// listener.
    async fn start(t: &Path, command: &SpawnCommand) -> ModuleProcess {
        start_with(t, command, listener()).await
    }

    async fn start_with(
        t: &Path,
        command: &SpawnCommand,
        listener: std::net::TcpListener,
    ) -> ModuleProcess {
        // Through the test trampoline, module arguments are libtest filters: one
        // equal to a test's full name would run that test in the child too.
        assert!(
            !command.args.iter().any(|a| a.contains("::tests::")),
            "a module argument that names a test: {:?}",
            command.args
        );
        let trampoline = crate::launch::test_trampoline();
        spawn(
            LaunchSpec {
                name: "t",
                command,
                package_dir: t,
                data_dir: &t.join("data"),
                callback_sock: &t.join("cb.sock"),
                trampoline: &trampoline,
                listener,
            },
            Generation::starting(),
        )
        .await
        .expect("spawn")
    }

    /// Wait for the leader to exit, within a bound — a test that waits without
    /// one reports a hang as silence.
    async fn exited(p: &mut ModuleProcess) -> crate::supervise::Exit {
        tokio::time::timeout(std::time::Duration::from_secs(10), p.exited())
            .await
            .expect("the module did not exit within 10s")
            .unwrap()
    }

    fn read(t: &Path, name: &str) -> String {
        std::fs::read_to_string(t.join(name)).unwrap_or_default()
    }

    /// The child must be in its OWN process group, so a stop can signal the
    /// whole tree. A module that starts helpers and is killed by pid alone
    /// leaves them running — holding ports, holding the package directory, and
    /// invisible to a `disable` that reported success.
    #[tokio::test]
    async fn the_child_gets_its_own_process_group() {
        let t = pkg();
        // Output goes to a file: the child's stdout is drained into the log.
        exe(
            &t.path().join("bin/mod"),
            "#!/bin/sh\nps -o pgid= -p $$ | tr -d ' ' > out\n",
        );
        let mut p = start(t.path(), &cmd("bin/mod", &[])).await;
        exited(&mut p).await;
        let child_pgid: i32 = read(t.path(), "out").trim().parse().unwrap();

        assert_eq!(
            child_pgid,
            p.pid(),
            "the child's process group should be its own pid"
        );
        // Control: it is NOT this process's group — otherwise "its own group"
        // would be satisfied by inheriting ours whenever we happen to lead one.
        assert_ne!(child_pgid, i32::try_from(std::process::id()).unwrap());
        let _ = p.stop(std::time::Duration::from_millis(100)).await;
    }

    /// The token reaches the child, and through the environment rather than the
    /// command line — arguments are world-readable through `ps`.
    #[tokio::test]
    async fn the_token_reaches_the_child_out_of_sight_of_ps() {
        let t = pkg();
        // The child reports BOTH: the env var on the first line, its own argv on
        // the second. Asking the child is the whole point — an assertion on the
        // `args` vector built here would only be checking a value this test just
        // wrote, which is true whatever `spawn` does with it. (Measured: a
        // mutation that ALSO put the token in argv left that version green.)
        exe(
            &t.path().join("bin/mod"),
            "#!/bin/sh\n{ echo \"$A24_HANDSHAKE_TOKEN\"; echo \"$@\"; } > out\n",
        );
        let mut p = start(t.path(), &cmd("bin/mod", &["--flag"])).await;
        let token = p.token().to_owned();
        exited(&mut p).await;
        let out = read(t.path(), "out");
        let mut lines = out.lines();

        assert_eq!(lines.next().unwrap_or_default().trim(), token, "env var");
        let argv = lines.next().unwrap_or_default();
        assert!(
            !argv.contains(&token),
            "the token appeared in the child's argv, where `ps` can read it: {argv:?}"
        );
        // Control: the child really can see its arguments, so the assertion above
        // is not satisfied by argv being empty for some unrelated reason.
        assert!(
            argv.contains("--flag"),
            "the child saw no arguments: {argv:?}"
        );
        let _ = p.stop(std::time::Duration::from_millis(100)).await;
    }

    /// Run `code` in Python (no site, isolated: it adds nothing of its own to
    /// the environment and opens no files) as the module.
    fn python(code: &str) -> SpawnCommand {
        cmd("python3", &["-I", "-S", "-c", code])
    }

    /// SPEC-ME3 §5: spawn clears the inherited environment. The daemon's can
    /// carry the kernel's credentials; a module gets [`INHERITED_ENV`], `LC_*`
    /// and the four `A24_*` variables, and nothing else.
    ///
    /// The probe is `sh -c env`, not Python: macOS's `/usr/bin/python3` is an
    /// Xcode shim that ADDS `SDKROOT`, `CPATH` and others to its own
    /// environment even when started with none (measured with `env -i`), which
    /// would read as a leak that is not one. A shell adds only `_`, `PWD` and
    /// `SHLVL`, listed below.
    #[tokio::test]
    async fn the_environment_is_cleared_down_to_the_allowlist() {
        // The positive sample: cargo sets this for every test process. If it is
        // missing the test proves nothing, so that is a failure, not a skip.
        //
        // Looked up by iterating rather than with `env::var_os("…")`: the CLI's
        // `passthrough_list_matches_what_the_daemon_actually_reads` scans daemon
        // sources for that spelling to find what the daemon reads at RUN time,
        // and a test's precondition is not that.
        assert!(
            std::env::vars_os().any(|(k, _)| k == "CARGO_MANIFEST_DIR"),
            "precondition: the parent has a variable the child must not inherit"
        );
        let t = pkg();
        let mut p = start(t.path(), &cmd("sh", &["-c", "env > out"])).await;
        exited(&mut p).await;
        let out = read(t.path(), "out");
        let env: std::collections::BTreeMap<&str, &str> =
            out.lines().filter_map(|l| l.split_once('=')).collect();

        assert!(
            !env.contains_key("CARGO_MANIFEST_DIR"),
            "the child inherited the parent's environment: {env:?}"
        );
        let stray: Vec<&&str> = env
            .keys()
            .filter(|k| {
                !(INHERITED_ENV.contains(k) || k.starts_with("LC_") || k.starts_with("A24_"))
                    // Set by the shell itself, not inherited.
                    && !["_", "PWD", "SHLVL", "OLDPWD"].contains(k)
            })
            .collect();
        assert!(stray.is_empty(), "not on the allowlist: {stray:?}");
        // Control: what must be there is there, with the values given — and
        // no `A24_*` beyond the four.
        assert!(env.contains_key("PATH"), "{env:?}");
        let a24: Vec<&&str> = env.keys().filter(|k| k.starts_with("A24_")).collect();
        assert_eq!(
            a24,
            [
                &ENV_CALLBACK_SOCK,
                &ENV_DATA_DIR,
                &ENV_HANDSHAKE_TOKEN,
                &ENV_LISTEN_FD
            ],
            "exactly the four A24_* variables"
        );
        assert_eq!(env.get(ENV_LISTEN_FD), Some(&"3"));
        assert_eq!(env.get(ENV_HANDSHAKE_TOKEN).copied(), Some(p.token()));
        let data = t.path().join("data");
        let sock = t.path().join("cb.sock");
        assert_eq!(env.get(ENV_DATA_DIR).copied(), data.to_str());
        assert_eq!(env.get(ENV_CALLBACK_SOCK).copied(), sock.to_str());
        let _ = p.stop(std::time::Duration::from_millis(100)).await;
    }

    /// SPEC-ME3 §1: the kernel opens the listening socket and hands it over as
    /// fd 3; the module does not choose an address. The module here accepts one
    /// connection on fd 3 and answers on it.
    #[tokio::test]
    async fn the_listening_socket_arrives_as_fd_3() {
        use tokio::io::AsyncReadExt;
        let t = pkg();
        let l = listener();
        let addr = l.local_addr().unwrap();
        let mut p = start_with(
            t.path(),
            &python(
                "import os, socket\n\
                 s = socket.socket(fileno=int(os.environ['A24_LISTEN_FD']))\n\
                 c, _ = s.accept()\n\
                 c.sendall(b'hello from fd 3')\n\
                 c.close()",
            ),
            l,
        )
        .await;
        let mut conn = tokio::net::TcpStream::connect(addr).await.unwrap();
        let mut got = Vec::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            conn.read_to_end(&mut got),
        )
        .await
        .expect("no answer within 10s")
        .unwrap();
        assert_eq!(got, b"hello from fd 3");
        exited(&mut p).await;
        let _ = p.stop(std::time::Duration::from_millis(100)).await;
    }

    /// The listener is closed in the daemon once the child has it: when the
    /// module is gone, its port refuses at once — a copy kept here would queue
    /// connections in a backlog nobody accepts from, and a request would hang
    /// instead of failing.
    ///
    /// Up to three attempts, each with a fresh port: other tests in this
    /// process bind `127.0.0.1:0` concurrently, and one of them can be handed
    /// the port the moment the module frees it — measured once in 120 full
    /// runs, as a connection "accepted by a dead module's port". A real leak
    /// (the daemon keeping its copy) accepts on every attempt, so it stays red;
    /// a coincidence three times running is about one in a million.
    #[tokio::test]
    async fn once_the_module_is_gone_its_port_refuses() {
        let mut accepted = Vec::new();
        for _ in 0..3 {
            let t = pkg();
            let l = listener();
            let addr = l.local_addr().unwrap();
            let mut p = start_with(t.path(), &cmd("sh", &["-c", "exit 0"]), l).await;
            exited(&mut p).await;
            let _ = p.stop(std::time::Duration::from_millis(100)).await;
            let connect = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                tokio::net::TcpStream::connect(addr),
            )
            .await
            .expect("the connect hung: something still holds the listener");
            if connect.is_err() {
                return; // refused, as it should be
            }
            accepted.push(addr);
        }
        panic!("a dead module's port accepted a connection on every attempt: {accepted:?}");
    }

    /// Only fds 0–3 reach the module: stdio and the listener. Anything else
    /// would be a daemon resource — a database, a socket to a provider — in a
    /// third party's hands.
    ///
    /// The probe is an fd the daemon holds **without** close-on-exec, at a high
    /// number — the shape of an fd a daemon inherits from a shell or a service
    /// manager. The first version probed with a plain `File`, which Rust opens
    /// close-on-exec, so it passed with no protection at all (review of SUP-1,
    /// round 1).
    #[tokio::test]
    async fn an_inherited_fd_without_close_on_exec_does_not_reach_the_child() {
        let t = pkg();
        let file = std::fs::File::open(t.path()).unwrap();
        let high = rustix::io::fcntl_dupfd_cloexec(&file, 700).unwrap();
        rustix::io::fcntl_setfd(&high, rustix::io::FdFlags::empty()).unwrap();
        let number = std::os::fd::AsRawFd::as_raw_fd(&high);
        assert!(
            (700..1024).contains(&number),
            "precondition: a high fd the probe below scans, got {number}"
        );
        let mut p = start(
            t.path(),
            &python(
                "import os\n\
                 def is_open(fd):\n\
                 \x20   try:\n\
                 \x20       os.fstat(fd)\n\
                 \x20       return True\n\
                 \x20   except OSError:\n\
                 \x20       return False\n\
                 fds = [fd for fd in range(0, 1024) if is_open(fd)]\n\
                 open('out','w').write(' '.join(map(str, fds)))",
            ),
        )
        .await;
        exited(&mut p).await;
        // 0–3 open is also the control: the probe sees open fds.
        assert_eq!(read(t.path(), "out"), "0 1 2 3");
        let _ = p.stop(std::time::Duration::from_millis(100)).await;
        drop(high);
    }

    /// A module that writes more than a pipe holds is not blocked by it: its
    /// output is drained whether or not anyone reads the log. Undrained, it
    /// blocks at about 64 KiB and looks exactly like a module that hung.
    #[tokio::test]
    async fn a_module_that_floods_stderr_is_not_blocked_by_it() {
        let t = pkg();
        let mut p = start(
            t.path(),
            &cmd(
                "sh",
                &["-c", "head -c 2097152 /dev/zero >&2; echo done > out"],
            ),
        )
        .await;
        exited(&mut p).await;
        assert_eq!(read(t.path(), "out").trim(), "done");
        let _ = p.stop(std::time::Duration::from_millis(100)).await;
    }

    /// Someone else able to write the package could swap what runs — its
    /// directory, any directory or file in it. Each is refused, **for a bare
    /// interpreter too**: `sh main.sh` runs package code as surely as `bin/mod`
    /// does (the first version let it through; review of SUP-1, round 1). The
    /// same tree with owner-only write access is the control.
    #[test]
    fn a_package_others_can_write_is_refused_whatever_the_command() {
        let t = pkg();
        exe(&t.path().join("bin/mod"), "#!/bin/sh\nexit 0\n");
        std::fs::create_dir(t.path().join("lib")).unwrap();
        std::fs::write(t.path().join("lib/code.sh"), "exit 0\n").unwrap();
        let mode = |p: &Path, m: u32| {
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(m)).unwrap();
        };
        for spawn in [cmd("bin/mod", &[]), cmd("sh", &["lib/code.sh"])] {
            resolve(&spawn, t.path()).expect("control: owner-only write access is fine");
            for (what, path, bad, good) in [
                ("package dir", t.path().to_owned(), 0o777, 0o700),
                ("bin/", t.path().join("bin"), 0o775, 0o755),
                ("program", t.path().join("bin/mod"), 0o757, 0o755),
                (
                    "a file the interpreter runs",
                    t.path().join("lib/code.sh"),
                    0o666,
                    0o644,
                ),
            ] {
                mode(&path, bad);
                let err = resolve(&spawn, t.path()).expect_err(what);
                assert!(
                    matches!(&err, LaunchError::UnsafeOwnership(p) if p == &path.canonicalize().unwrap()),
                    "{} / {what}: {err:?}",
                    spawn.command
                );
                mode(&path, good);
            }
        }
    }

    /// A relative `PATH` entry is skipped: it would be checked against the
    /// daemon's directory and executed from the package's — two different
    /// files. The absolute entry after it is the control.
    #[test]
    fn a_relative_path_entry_is_not_searched() {
        let t = pkg();
        let found = which_in("sh", std::ffi::OsStr::new("bin:/bin:/usr/bin")).expect("sh");
        assert!(found.is_absolute(), "{}", found.display());
        // A relative entry that DOES reach an executable `sh` from here —
        // `../..` up to `/`, then down into the package — is still not
        // searched. (Without the climb it would resolve to nothing and the
        // assertion would hold with no filter at all.)
        exe(&t.path().join("bin/sh"), "#!/bin/sh\nexit 0\n");
        let cwd = std::env::current_dir().unwrap();
        let mut rel = PathBuf::new();
        for _ in cwd.components().skip(1) {
            rel.push("..");
        }
        rel.push(t.path().join("bin").strip_prefix("/").unwrap());
        assert!(
            rel.is_relative() && rel.join("sh").exists(),
            "precondition: {}",
            rel.display()
        );
        assert_eq!(which_in("sh", rel.as_os_str()), None);
    }

    #[test]
    fn a_long_line_is_cut_and_the_next_one_is_whole() {
        let mut lines = Lines::default();
        let long = vec![b'x'; MAX_LOG_LINE * 3];
        let mut out = lines.push(&long);
        out.extend(lines.push(b"\nshort\npart"));
        out.extend(lines.push(b"ial\n"));
        assert_eq!(out.len(), 3, "{out:?}");
        assert_eq!(out[0].text.len(), MAX_LOG_LINE);
        assert!(out[0].cut);
        assert_eq!((out[1].text.as_slice(), out[1].cut), (&b"short"[..], false));
        // A line split across reads is joined.
        assert_eq!(out[2].text, b"partial");
        // The last line without a newline is not lost.
        let mut tail = Lines::default();
        assert!(tail.push(b"no newline").is_empty());
        assert_eq!(tail.finish().map(|l| l.text), Some(b"no newline".to_vec()));
    }

    #[test]
    fn output_beyond_the_rate_is_counted_and_dropped() {
        let t0 = std::time::Instant::now();
        let mut limit = RateLimit::new(t0);
        let admitted = (0..LOG_LINES_PER_SECOND + 50)
            .filter(|_| limit.admit())
            .count();
        assert_eq!(admitted, LOG_LINES_PER_SECOND as usize);
        // Within the same second: nothing to report yet.
        assert_eq!(limit.roll(t0 + std::time::Duration::from_millis(500)), None);
        // The next window reports the drop once, and admits again.
        assert_eq!(limit.roll(t0 + std::time::Duration::from_secs(1)), Some(50));
        assert!(limit.admit());
        assert_eq!(limit.flush(), None);
    }

    /// The trampoline flags fds close-on-exec in the CHILD, so a module gets
    /// 0–3 even while other threads here keep opening fds without the flag —
    /// the window that marking the daemon's own fds before the fork left open
    /// (review of SUP-1, round 2). Twelve modules start at once while a thread
    /// churns flagless fds; every one reports exactly 0–3.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn modules_started_while_fds_churn_still_get_only_stdio_and_the_listener() {
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let churn = std::thread::spawn({
            let stop = stop.clone();
            move || {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    if let Ok(f) = std::fs::File::open("/dev/null") {
                        let _ = rustix::io::fcntl_setfd(&f, rustix::io::FdFlags::empty());
                        std::thread::yield_now();
                    }
                }
            }
        });
        let report = "import os\n\
             def is_open(fd):\n\
             \x20   try:\n\
             \x20       os.fstat(fd)\n\
             \x20       return True\n\
             \x20   except OSError:\n\
             \x20       return False\n\
             fds = [fd for fd in range(0, 1024) if is_open(fd)]\n\
             open('out','w').write(' '.join(map(str, fds)))";
        let dirs: Vec<tempfile::TempDir> = (0..12).map(|_| pkg()).collect();
        let command = python(report);
        let mut starting = tokio::task::JoinSet::new();
        for d in &dirs {
            let (path, command) = (d.path().to_owned(), command.clone());
            starting.spawn(async move { start(&path, &command).await });
        }
        let mut procs = Vec::new();
        while let Some(p) = starting.join_next().await {
            procs.push(p.unwrap());
        }
        for p in &mut procs {
            exited(p).await;
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        churn.join().unwrap();
        for d in &dirs {
            assert_eq!(read(d.path(), "out"), "0 1 2 3", "{}", d.path().display());
        }
        for p in procs {
            let _ = p.stop(std::time::Duration::from_millis(100)).await;
        }
    }

    /// A long argument list reaches the module whole: the module's arguments
    /// travel as argv elements of their own through the trampoline. Packed into
    /// one environment variable they hit Linux's limit on a single string
    /// (128 KiB) — 2000 arguments of 100 bytes are legal as argv and were not
    /// as one variable (review of SUP-1, round 3).
    #[tokio::test]
    async fn a_long_argument_list_reaches_the_module_whole() {
        let t = pkg();
        let arg = "a".repeat(100);
        let mut args = vec!["-c", "echo $# > out", "sh"];
        args.extend(std::iter::repeat_n(arg.as_str(), 2000));
        let mut p = start(t.path(), &cmd("sh", &args)).await;
        let exit = exited(&mut p).await;
        assert_eq!(exit.code, Some(0), "{exit}");
        assert_eq!(read(t.path(), "out").trim(), "2000");
        let _ = p.stop(std::time::Duration::from_millis(100)).await;
    }

    /// The trampoline's own check: a complete listing of this process's fds
    /// finds an fd that is open at or above a bound, and nothing above one
    /// that no fd reaches.
    #[test]
    fn the_fd_listing_finds_a_high_fd_and_nothing_above_it() {
        let file = std::fs::File::open("/dev/null").unwrap();
        let high = rustix::io::fcntl_dupfd_cloexec(&file, 900).unwrap();
        let number = std::os::fd::AsRawFd::as_raw_fd(&high);
        let found = open_fd_at_or_above(number).unwrap();
        assert!(
            found.is_some_and(|fd| fd >= number),
            "{found:?} for {number}"
        );
        assert_eq!(open_fd_at_or_above(i32::MAX).unwrap(), None);
    }

    /// What the module's exit looked like comes back: `exited` reports it, and
    /// so does `stop`.
    #[tokio::test]
    async fn the_exit_code_is_reported() {
        let t = pkg();
        let mut p = start(t.path(), &cmd("sh", &["-c", "exit 3"])).await;
        let exit = exited(&mut p).await;
        assert_eq!((exit.code, exit.signal), (Some(3), None));
        let report = p.stop(std::time::Duration::from_millis(100)).await.unwrap();
        assert_eq!(report.exit.code, Some(3));
    }

    /// A tree that cannot be read is reported as exactly that — not as an
    /// ownership verdict, which would send the operator after the wrong cause
    /// (review of SUP-1, round 2).
    #[test]
    fn an_unreadable_directory_is_an_inspection_error_not_an_ownership_one() {
        let t = pkg();
        exe(&t.path().join("bin/mod"), "#!/bin/sh\nexit 0\n");
        let locked = t.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let err = resolve(&cmd("bin/mod", &[]), t.path()).expect_err("unreadable");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            matches!(&err, LaunchError::Inspect { path, .. } if path.ends_with("locked")),
            "{err:?}"
        );
        // Control: readable again, and it passes.
        resolve(&cmd("bin/mod", &[]), t.path()).expect("readable");
    }
}
