//! A shutdown you can see and tune (SHUT-1b): the budgets, the deadlines
//! derived from them, the record a shutdown leaves, and what the next start
//! can tell from it. Semantics: `docs/design/SHUT-shutdown-observability.md`
//! (v5); the terms here are its terms.
//!
//! Two files in `<state dir>/run/`:
//! - `daemon.alive` — this daemon's `instance_id`, written right after the
//!   singleton lock and removed only once the shutdown's summary is on disk;
//! - `last-shutdown.json` — the latest summary that made it to disk.
//!
//! So a start that finds `daemon.alive` knows the previous daemon did not
//! leave a matching summary: the watchdog ended it, or it crashed, or it was
//! SIGKILLed — whichever, its clean end cannot be confirmed.

use agent24_os_proto::stop_record::{GroupEnd, ProcessAtStop, StopRecord, SupervisorEnd};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The HTTP drain's window — fixed; the module budgets do not move it.
pub const HTTP_GRACE: Duration = Duration::from_millis(1500);
/// After a module's stop grace: time to confirm its group is gone.
pub const CONFIRM: Duration = Duration::from_millis(200);
/// After the modules' deadline: time to put the summary on disk.
pub const PERSIST: Duration = Duration::from_millis(200);
/// After the later of the HTTP and persistence deadlines: the runtime's own
/// teardown, before the watchdog ends the process.
pub const WATCHDOG_MARGIN: Duration = Duration::from_millis(300);

pub const DRAIN_ENV: &str = "A24_MODULE_DRAIN_MS";
pub const GRACE_ENV: &str = "A24_MODULE_STOP_GRACE_MS";
const DRAIN_DEFAULT_MS: u64 = 800;
const DRAIN_MAX_MS: u64 = 10_000;
const GRACE_DEFAULT_MS: u64 = 500;
const GRACE_MIN_MS: u64 = 100;
const GRACE_MAX_MS: u64 = 5_000;

/// How long a shutdown gives each out-of-process module: to finish what it
/// has (`drain`), then to exit on SIGTERM before SIGKILL (`stop_grace`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Params {
    pub drain: Duration,
    pub stop_grace: Duration,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            drain: Duration::from_millis(DRAIN_DEFAULT_MS),
            stop_grace: Duration::from_millis(GRACE_DEFAULT_MS),
        }
    }
}

impl Params {
    /// Read from the environment. A value that is not a whole number of
    /// milliseconds in range is **not** fatal: a daemon that runs 24/7 must
    /// not go down over a typo in a tuning knob — the CLI would swallow the
    /// reason and launchd would crash-loop it. It is warned about, the default
    /// is used, and the warning is kept for the live report (SHUT-1c).
    pub fn from_env(get: impl Fn(&str) -> Option<String>) -> (Self, Vec<String>) {
        let mut warnings = Vec::new();
        let mut read = |name: &str, min: u64, max: u64, default: u64| {
            let Some(raw) = get(name) else {
                return Duration::from_millis(default);
            };
            match raw.trim().parse::<u64>() {
                Ok(ms) if (min..=max).contains(&ms) => Duration::from_millis(ms),
                _ => {
                    warnings.push(format!(
                        "{name}={raw:?} is not a whole number of milliseconds in {min}..={max}; \
                         using the default, {default}ms"
                    ));
                    Duration::from_millis(default)
                }
            }
        };
        let drain = read(DRAIN_ENV, 0, DRAIN_MAX_MS, DRAIN_DEFAULT_MS);
        let stop_grace = read(GRACE_ENV, GRACE_MIN_MS, GRACE_MAX_MS, GRACE_DEFAULT_MS);
        (Self { drain, stop_grace }, warnings)
    }

    /// [`Params::from_env`] over this process's environment — each variable
    /// read by its constant name, where the CLI's launchd test can see it
    /// (it demands every variable the daemon reads be forwarded).
    pub fn from_process_env() -> (Self, Vec<String>) {
        let drain = std::env::var(DRAIN_ENV).ok();
        let grace = std::env::var(GRACE_ENV).ok();
        Self::from_env(move |name| match name {
            DRAIN_ENV => drain.clone(),
            GRACE_ENV => grace.clone(),
            _ => None,
        })
    }

    /// Every deadline of a shutdown that began at `began`, derived from it —
    /// never from one another.
    #[must_use]
    pub fn deadlines(&self, began: tokio::time::Instant) -> Deadlines {
        let http = began + HTTP_GRACE;
        let modules = began + self.drain + self.stop_grace + CONFIRM;
        let persist = modules + PERSIST;
        let watchdog = http.max(persist) + WATCHDOG_MARGIN;
        Deadlines {
            began,
            http,
            modules,
            persist,
            watchdog,
        }
    }

    /// From SIGTERM to the process gone, at the latest: 2s at the defaults
    /// (TASKS B2), more only when the budgets were raised.
    #[must_use]
    pub fn exit_bound(&self) -> Duration {
        let d = self.deadlines(tokio::time::Instant::now());
        d.watchdog - d.began
    }
}

/// The deadlines of one shutdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Deadlines {
    pub began: tokio::time::Instant,
    /// The HTTP drain ends.
    pub http: tokio::time::Instant,
    /// Modules still stopping are left to the drop of their supervisors.
    pub modules: tokio::time::Instant,
    /// The summary is on disk, or not.
    pub persist: tokio::time::Instant,
    /// The process ends, whatever is stuck.
    pub watchdog: tokio::time::Instant,
}

// ---- evidence across starts ----------------------------------------------

const ALIVE: &str = "daemon.alive";
const SUMMARY: &str = "last-shutdown.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Alive {
    instance_id: String,
    pid: u32,
    started_at_ms: u128,
}

/// What a start can tell about the daemon before it (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Previous {
    /// Neither file: no daemon has run from this state directory, or its
    /// history was removed.
    NoHistory,
    /// It left its summary; `stop_result` is that summary's own verdict.
    Clean { stop_result: String },
    /// A summary that cannot be read, and no marker: history lost, no sign of
    /// a crash.
    Unreadable,
    /// Its summary is on disk; only removing the marker failed.
    CleanupFailed,
    /// No summary matching it can be established.
    Unconfirmed,
}

fn wall_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// These files are small; anything bigger is not one of ours.
const MAX_EVIDENCE_BYTES: u64 = 1 << 20;

/// Read one of the two files: `None` if it is not there, `Some(Err)` if it
/// is there but not a readable record of ours — not a regular file (a
/// symlink, a FIFO), too big, or not the right JSON. Never follows a link,
/// never reads more than [`MAX_EVIDENCE_BYTES`] (review of SHUT-1b, round 1).
fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<Result<T, ()>> {
    use std::io::Read;
    // Opened without following a link and without blocking, then checked on
    // the descriptor itself: no window between a check by path and the open
    // for someone to put a link or a FIFO in its place (review of SHUT-1b,
    // round 2).
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let flags = rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK;
        // Reinterpreted, never defaulted: a fallback of 0 would open through
        // a link and block on a FIFO — the very things these flags forbid.
        opts.custom_flags(flags.bits().cast_signed());
    }
    let file = match opts.open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(_) => return Some(Err(())),
    };
    match file.metadata() {
        Ok(meta) if meta.is_file() && meta.len() <= MAX_EVIDENCE_BYTES => {}
        _ => return Some(Err(())),
    }
    let mut bytes = Vec::new();
    let read = file.take(MAX_EVIDENCE_BYTES + 1).read_to_end(&mut bytes);
    if read.is_err() || bytes.len() as u64 > MAX_EVIDENCE_BYTES {
        return Some(Err(()));
    }
    Some(serde_json::from_slice(&bytes).map_err(|_| ()))
}

/// The previous daemon, from the two files — the marker first: while it is
/// there, a summary that cannot be read does not hide that the daemon that
/// wrote the marker left no matching one.
#[cfg(test)]
pub fn previous(run_dir: &Path) -> Previous {
    evidence(run_dir).0
}

/// [`previous`], with the summary it was judged from — one read of each
/// file, so the verdict and the summary it reports on are the same snapshot
/// (review of SHUT-1c, round 1).
#[must_use]
pub fn evidence(run_dir: &Path) -> (Previous, Option<Summary>) {
    let alive = read_json::<Alive>(&run_dir.join(ALIVE));
    if matches!(alive, Some(Err(()))) {
        // A marker that cannot be read: no summary could be matched to it.
        return (Previous::Unconfirmed, None);
    }
    let summary = read_json::<Summary>(&run_dir.join(SUMMARY));
    let previous = match (&alive, &summary) {
        (None, None) => Previous::NoHistory,
        (None, Some(Ok(s))) => Previous::Clean {
            stop_result: s.stop_result.clone(),
        },
        (None, Some(Err(()))) => Previous::Unreadable,
        (Some(Ok(a)), Some(Ok(s))) if a.instance_id == s.instance_id => Previous::CleanupFailed,
        (Some(_), _) => Previous::Unconfirmed,
    };
    (previous, summary.and_then(Result::ok))
}

impl Previous {
    /// Its name in `GET /api/v1/shutdown`.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::NoHistory => "no_history",
            Self::Clean { .. } => "clean",
            Self::Unreadable => "unreadable",
            Self::CleanupFailed => "cleanup_failed",
            Self::Unconfirmed => "unconfirmed",
        }
    }

    /// A line for the start-up log, or `None` when there is nothing to say.
    #[must_use]
    pub fn warning(&self, run_dir: &Path) -> Option<String> {
        let dir = run_dir.display();
        match self {
            Self::NoHistory => None,
            Self::Clean { stop_result } if stop_result == "clean" => None,
            Self::Clean { stop_result } => Some(format!(
                "the previous daemon (state in {dir}) shut down {stop_result}: see {SUMMARY} there"
            )),
            Self::Unreadable => Some(format!(
                "the previous shutdown's summary in {dir} cannot be read; its history is lost"
            )),
            Self::CleanupFailed => Some(format!(
                "the previous daemon (state in {dir}) shut down and left its summary, but could not \
                 remove its {ALIVE} marker"
            )),
            Self::Unconfirmed => Some(format!(
                "the previous daemon (state in {dir}) did not confirm a clean shutdown — the watchdog \
                 ended it, it crashed, or it was killed; if the watchdog did, raise {DRAIN_ENV} / \
                 {GRACE_ENV} for modules that need longer"
            )),
        }
    }
}

fn sync_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::File::open(dir)?.sync_all()
}

/// Write `bytes` to `dir/name` durably: temp file, fsync, rename, fsync the
/// directory. `Err((published, e))`: `published` says whether the rename
/// had already landed — the file is visible, only its durability is unsure.
fn write_durable(dir: &Path, name: &str, bytes: &[u8]) -> Result<(), (bool, std::io::Error)> {
    use std::io::Write;
    // A fresh random name, created exclusively: a stale temp file — or a
    // link planted in its place — is never reused, so its mode is always
    // ours (review of SHUT-1b, round 1).
    let tmp = dir.join(format!(".{name}.tmp.{}", random_hex(8)));
    let staged = (|| {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, dir.join(name))
    })();
    if let Err(e) = staged {
        let _ = std::fs::remove_file(&tmp);
        return Err((false, e));
    }
    sync_dir(dir).map_err(|e| (true, e))
}

fn random_hex(bytes: usize) -> String {
    use rand::RngCore;
    let mut b = vec![0u8; bytes];
    rand::rng().fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Make `dir` (0700) if it is not there — and, when it was just made, fsync
/// its parent, so the directory's own entry survives a power loss along with
/// the marker inside it (review of SHUT-1b, round 3).
fn private_dir(dir: &Path) -> std::io::Result<()> {
    let existed = dir.is_dir();
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(dir)?;
    if !existed && let Some(parent) = dir.parent() {
        sync_dir(parent)?;
    }
    Ok(())
}

impl StoppingMarker {
    /// Where the evidence lives, for log lines that must say which state
    /// directory they are about.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

/// This daemon's `daemon.alive`, while it is starting: dropped before the
/// shutdown takes it over — a start-up that failed — it removes the marker,
/// since a start that failed is not a shutdown that did not finish.
#[derive(Debug)]
pub struct MarkerGuard {
    marker: StoppingMarker,
    /// Still start-up's: dropping it removes the marker.
    armed: bool,
}

/// The marker once the shutdown owns it: nothing removes it but a summary
/// that made it to disk (see [`persist`]). Handing it over is synchronous,
/// before the shutdown's task is spawned, so no drop of an unpolled task can
/// remove it as if start-up had failed.
#[derive(Debug, Clone)]
pub struct StoppingMarker {
    dir: PathBuf,
    instance_id: String,
}

impl StoppingMarker {
    #[must_use]
    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    /// Remove the marker if it is still this instance's.
    /// Remove the marker if it is still this instance's. A marker that is
    /// there but cannot be read is an error — not "someone else's" — so a
    /// shutdown that could not remove it says so (review of SHUT-1b,
    /// round 2).
    fn remove(&self) -> std::io::Result<()> {
        let path = self.dir.join(ALIVE);
        match read_json::<Alive>(&path) {
            Some(Ok(a)) if a.instance_id == self.instance_id => std::fs::remove_file(&path),
            Some(Err(())) => Err(std::io::Error::other(format!(
                "{} is there but cannot be read; left in place",
                path.display()
            ))),
            _ => Ok(()),
        }
    }
}

impl MarkerGuard {
    /// Write `daemon.alive` with a new instance id. Always a guard — this
    /// run's id and where its summary goes — and a warning when the marker
    /// could not be written (this run then leaves no crash evidence, and says
    /// so) or its durability is unsure. A marker that was never written is
    /// never removed by anyone: removal is by id (review of SHUT-1b, round 1).
    #[must_use]
    pub fn create(run_dir: &Path) -> (Self, Option<String>) {
        let instance_id = random_hex(16);
        let alive = Alive {
            instance_id: instance_id.clone(),
            pid: std::process::id(),
            started_at_ms: wall_ms(),
        };
        let guard = || Self {
            marker: StoppingMarker {
                dir: run_dir.to_owned(),
                instance_id: instance_id.clone(),
            },
            armed: true,
        };
        if let Err(e) = private_dir(run_dir) {
            return (
                guard(),
                Some(format!(
                    "cannot create {}: {e}; this run leaves no crash evidence",
                    run_dir.display()
                )),
            );
        }
        let bytes = serde_json::to_vec(&alive).unwrap_or_default();
        match write_durable(run_dir, ALIVE, &bytes) {
            Ok(()) => (guard(), None),
            Err((true, e)) => (
                guard(),
                Some(format!(
                    "{ALIVE} was written but its durability is unsure ({e})"
                )),
            ),
            Err((false, e)) => (
                guard(),
                Some(format!(
                    "cannot write {ALIVE} in {}: {e}; this run leaves no crash evidence",
                    run_dir.display()
                )),
            ),
        }
    }

    /// Hand the marker to the shutdown: from here only a summary on disk
    /// removes it.
    #[must_use]
    pub fn into_stopping(mut self) -> StoppingMarker {
        self.armed = false;
        self.marker.clone()
    }
}

impl Drop for MarkerGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.marker.remove();
        }
    }
}

// ---- the summary ---------------------------------------------------------

/// Why a module was being stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    Shutdown,
    Disable,
}

/// One module's stop, as written to `last-shutdown.json`. Field names and
/// values only grow; `None` is "not known".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleStop {
    pub name: String,
    pub reason: String,
    pub process: Option<String>,
    pub drain_ended_by: Option<String>,
    pub drain_budget_ms: Option<u128>,
    pub drain_ms: Option<u128>,
    pub leader: Option<String>,
    pub stop_ms: Option<u128>,
    pub abandoned: Option<usize>,
    pub never_sent: Option<usize>,
    pub group: Option<String>,
    pub supervisor: Option<String>,
}

/// What a shutdown leaves for the next start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    pub version: u32,
    pub instance_id: String,
    /// Wall-clock milliseconds since the Unix epoch.
    pub began_at_ms: u128,
    pub took_ms: u128,
    /// `clean` | `degraded` | `timed_out` (see [`stop_result`]).
    pub stop_result: String,
    pub params: SummaryParams,
    pub records: Vec<ModuleStop>,
    /// Records left out so the file stays within what a start reads back
    /// ([`MAX_EVIDENCE_BYTES`]); 0 in any daemon with a sane number of
    /// modules.
    #[serde(default)]
    pub omitted_records: usize,
}

/// The budgets in effect, and every deadline they gave, in milliseconds
/// after the shutdown began.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SummaryParams {
    pub drain_ms: u128,
    pub stop_grace_ms: u128,
    pub http_deadline_ms: u128,
    pub module_deadline_ms: u128,
    pub persist_deadline_ms: u128,
    pub watchdog_ms: u128,
}

impl SummaryParams {
    #[must_use]
    pub fn of(params: &Params) -> Self {
        let d = params.deadlines(tokio::time::Instant::now());
        let after = |t: tokio::time::Instant| (t - d.began).as_millis();
        Self {
            drain_ms: params.drain.as_millis(),
            stop_grace_ms: params.stop_grace.as_millis(),
            http_deadline_ms: after(d.http),
            module_deadline_ms: after(d.modules),
            persist_deadline_ms: after(d.persist),
            watchdog_ms: after(d.watchdog),
        }
    }
}

fn name_of<T: std::fmt::Debug>(v: Option<T>) -> Option<String> {
    v.map(|v| {
        let debug = format!("{v:?}");
        // `KilledAfterGrace` → `killed_after_grace`
        let mut out = String::new();
        for (i, c) in debug.chars().enumerate() {
            if c.is_uppercase() {
                if i > 0 {
                    out.push('_');
                }
                out.extend(c.to_lowercase());
            } else {
                out.push(c);
            }
        }
        out
    })
}

impl ModuleStop {
    #[must_use]
    pub fn new(name: &str, reason: Reason, r: &StopRecord) -> Self {
        Self {
            name: name.to_owned(),
            reason: match reason {
                Reason::Shutdown => "shutdown",
                Reason::Disable => "disable",
            }
            .to_owned(),
            process: name_of(r.process),
            drain_ended_by: name_of(r.drain.map(|d| d.ended_by)),
            drain_budget_ms: r.drain.map(|d| d.budget.as_millis()),
            drain_ms: r.drain.map(|d| d.elapsed.as_millis()),
            leader: name_of(r.leader),
            stop_ms: r.stop_elapsed.map(|d| d.as_millis()),
            abandoned: r.abandoned,
            never_sent: r.never_sent,
            group: name_of(r.group),
            supervisor: name_of(r.supervisor),
        }
    }
}

/// The shutdown's verdict over its records, taken at the modules' deadline —
/// a total function, the first that holds: `timed_out` (a running module's
/// group, or a process-less one's loop, still had no end), `degraded` (a
/// group not confirmed gone, or a loop that did not stop by itself), `clean`.
#[must_use]
pub fn stop_result(records: &[StopRecord]) -> &'static str {
    let open = |r: &StopRecord| match r.process {
        Some(ProcessAtStop::None) => r.supervisor.is_none(),
        _ => r.group.is_none(),
    };
    if records.iter().any(open) {
        return "timed_out";
    }
    let bad = |r: &StopRecord| {
        matches!(
            r.group,
            Some(GroupEnd::Failed | GroupEnd::KillAttempted | GroupEnd::KillUnavailable)
        ) || matches!(
            r.supervisor,
            Some(SupervisorEnd::CutOff | SupervisorEnd::Killed | SupervisorEnd::Panicked)
        )
    };
    if records.iter().any(bad) {
        "degraded"
    } else {
        "clean"
    }
}

impl Summary {
    #[must_use]
    pub fn new(
        instance_id: &str,
        params: &Params,
        began_ago: Duration,
        records: &[(String, Reason, StopRecord)],
    ) -> Self {
        let snapshots: Vec<StopRecord> = records.iter().map(|(_, _, r)| r.clone()).collect();
        Self {
            version: 1,
            instance_id: instance_id.to_owned(),
            began_at_ms: wall_ms().saturating_sub(began_ago.as_millis()),
            took_ms: began_ago.as_millis(),
            stop_result: stop_result(&snapshots).to_owned(),
            params: SummaryParams::of(params),
            omitted_records: 0,
            records: records
                .iter()
                .map(|(name, reason, r)| ModuleStop::new(name, *reason, r))
                .collect(),
        }
    }

    /// One line for the log.
    #[must_use]
    pub fn describe(&self) -> String {
        let modules: Vec<String> = self
            .records
            .iter()
            .map(|m| {
                let end = match (m.process.as_deref(), &m.group, &m.supervisor) {
                    (Some("none"), _, Some(sup)) => format!("no process, {sup}"),
                    (Some("none"), _, None) => "no process, unfinished".to_owned(),
                    (_, Some(group), _) => group.clone(),
                    (_, None, _) => "unfinished".to_owned(),
                };
                let mut parts = vec![end];
                if let (Some(by), Some(ms)) = (&m.drain_ended_by, m.drain_ms) {
                    parts.push(format!("drain {by} {ms}ms"));
                }
                let cut = m
                    .abandoned
                    .unwrap_or(0)
                    .saturating_add(m.never_sent.unwrap_or(0));
                if cut > 0 {
                    parts.push(format!("cut {cut}"));
                }
                if let Some(leader) = &m.leader {
                    parts.push(leader.replace('_', " "));
                }
                if let Some(ms) = m.stop_ms {
                    parts.push(format!("stop {ms}ms"));
                }
                format!("{} ({})", m.name, parts.join(", "))
            })
            .collect();
        format!(
            "shutdown took {}ms, {}: {}",
            self.took_ms,
            self.stop_result,
            if modules.is_empty() {
                "no out-of-process modules".to_owned()
            } else {
                modules.join("; ")
            }
        )
    }
}

/// Put the summary on disk durably and only then remove the marker — one
/// job, each step only after the one before succeeded: a marker is never
/// removed without its summary on disk. Blocking; the caller bounds it.
///
/// # Errors
///
/// The first step that failed; the marker is then still there.
pub fn persist(summary: &Summary, marker: &StoppingMarker) -> std::io::Result<()> {
    let bytes = bounded(summary)?;
    write_durable(&marker.dir, SUMMARY, &bytes).map_err(|(_, e)| e)?;
    marker.remove()?;
    sync_dir(&marker.dir)
}

/// The summary as written: within [`MAX_EVIDENCE_BYTES`], so the next start
/// can read it back — records are dropped from the end, and counted, if the
/// whole would not fit (review of SHUT-1b, round 2).
fn bounded(summary: &Summary) -> std::io::Result<Vec<u8>> {
    let mut s = summary.clone();
    loop {
        let bytes = serde_json::to_vec_pretty(&s).map_err(std::io::Error::other)?;
        if bytes.len() as u64 <= MAX_EVIDENCE_BYTES || s.records.is_empty() {
            return Ok(bytes);
        }
        let drop = (s.records.len() / 8).max(1);
        s.records.truncate(s.records.len() - drop);
        s.omitted_records += drop;
    }
}

/// The largest integer the report's schema admits (2^53 - 1): a JSON number
/// beyond it loses precision in a JavaScript client.
const JSON_SAFE_MAX: u64 = (1 << 53) - 1;

/// A figure for the report, clamped to [`JSON_SAFE_MAX`] — the persisted ones
/// come from a file on disk, which may say anything that parses (review of
/// SHUT-1c, round 2).
fn ms(v: u128) -> u64 {
    u64::try_from(v).map_or(JSON_SAFE_MAX, |v| v.min(JSON_SAFE_MAX))
}

/// What `GET /api/v1/shutdown` answers (SHUT-1c): the budgets in effect,
/// what was warned about, and the daemon before — each figure the same one
/// the start-up log gave.
#[must_use]
pub fn report(
    params: &Params,
    warnings: &[String],
    evidence: Option<(&Path, &Previous, Option<Summary>)>,
) -> agent24_protocol::ShutdownReport {
    let ephemeral = evidence.is_none();
    let (evidence_dir, previous, previous_detail, last) = match evidence {
        None => (None, "no_history", None, None),
        Some((dir, previous, last)) => (
            Some(dir.display().to_string()),
            previous.code(),
            previous.warning(dir),
            last,
        ),
    };
    agent24_protocol::ShutdownReport {
        ephemeral,
        evidence_dir,
        drain_ms: ms(params.drain.as_millis()),
        stop_grace_ms: ms(params.stop_grace.as_millis()),
        exit_bound_ms: ms(params.exit_bound().as_millis()),
        config_warnings: warnings.to_vec(),
        previous: previous.to_owned(),
        previous_detail,
        last_shutdown: last.map(|s| agent24_protocol::LastShutdown {
            stop_result: s.stop_result.clone(),
            began_at_ms: ms(s.began_at_ms),
            took_ms: ms(s.took_ms),
            killed_after_grace: s
                .records
                .iter()
                .filter(|m| m.leader.as_deref() == Some("killed_after_grace"))
                .map(|m| m.name.clone())
                .collect(),
            cut_requests: s
                .records
                .iter()
                .filter_map(|m| {
                    // Saturating: these come from a file on disk (review of
                    // SHUT-1c, round 1).
                    let cut = m
                        .abandoned
                        .unwrap_or(0)
                        .saturating_add(m.never_sent.unwrap_or(0));
                    (m.drain_ended_by.as_deref() == Some("deadline") && cut > 0)
                        .then(|| format!("{} ({cut})", m.name))
                })
                .collect(),
            omitted_records: ms(s.omitted_records as u128),
        }),
    }
}

/// Where the two files live, for a non-ephemeral daemon.
#[must_use]
pub fn run_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("run")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use agent24_os_proto::stop_record::{DrainEnd, DrainEndedBy, Leader};

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: std::collections::HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |k| map.get(k).cloned()
    }

    /// Unset → the defaults; in range → used; anything else → the default
    /// and a warning naming the value, the range and the default. Never an
    /// error.
    #[test]
    fn budgets_come_from_the_environment_and_a_bad_one_falls_back_loudly() {
        let (p, w) = Params::from_env(env(&[]));
        assert_eq!(p, Params::default());
        assert!(w.is_empty());

        let (p, w) = Params::from_env(env(&[(DRAIN_ENV, "2500"), (GRACE_ENV, "1200")]));
        assert_eq!(p.drain, Duration::from_millis(2500));
        assert_eq!(p.stop_grace, Duration::from_millis(1200));
        assert!(w.is_empty());

        for bad in ["-1", "1.5", "", "10001", "fast"] {
            let (p, w) = Params::from_env(env(&[(DRAIN_ENV, bad)]));
            assert_eq!(p.drain, Duration::from_millis(DRAIN_DEFAULT_MS), "{bad:?}");
            assert_eq!(w.len(), 1, "{bad:?}");
            assert!(
                w[0].contains(DRAIN_ENV) && w[0].contains("0..=10000"),
                "{}",
                w[0]
            );
        }
        let (p, w) = Params::from_env(env(&[(GRACE_ENV, "50")]));
        assert_eq!(p.stop_grace, Duration::from_millis(GRACE_DEFAULT_MS));
        assert!(w[0].contains("100..=5000"), "{}", w[0]);
    }

    /// At the defaults a shutdown ends within 2s, as TASKS B2 asks; raised
    /// budgets raise the bound honestly; the smallest budgets never end it
    /// before the HTTP drain's 1.5s.
    #[tokio::test(start_paused = true)]
    async fn the_deadlines_follow_the_budgets_and_2s_holds_at_the_defaults() {
        let t0 = tokio::time::Instant::now();
        let d = Params::default().deadlines(t0);
        assert_eq!(d.http - t0, Duration::from_millis(1500));
        assert_eq!(d.modules - t0, Duration::from_millis(1500));
        assert_eq!(d.persist - t0, Duration::from_millis(1700));
        assert_eq!(d.watchdog - t0, Duration::from_millis(2000));
        assert_eq!(Params::default().exit_bound(), Duration::from_millis(2000));

        let raised = Params {
            drain: Duration::from_secs(5),
            stop_grace: Duration::from_secs(2),
        };
        assert_eq!(raised.exit_bound(), Duration::from_millis(7700));

        let least = Params {
            drain: Duration::ZERO,
            stop_grace: Duration::from_millis(100),
        };
        let d = least.deadlines(t0);
        assert_eq!(
            d.watchdog - t0,
            Duration::from_millis(1800),
            "not before HTTP's 1.5s"
        );
    }

    fn rec(process: ProcessAtStop) -> StopRecord {
        let mut r = StopRecord::default();
        r.process = Some(process);
        r
    }

    /// The verdict is total and takes the worst: an unfinished stop over a
    /// bad one over a clean one; a stop with no process is finished once its
    /// loop is.
    #[test]
    fn the_verdict_is_total_and_takes_the_worst() {
        let mut gone = rec(ProcessAtStop::Running);
        gone.group = Some(GroupEnd::Gone);
        gone.supervisor = Some(SupervisorEnd::Stopped);
        let mut none = rec(ProcessAtStop::None);
        none.supervisor = Some(SupervisorEnd::Stopped);
        assert_eq!(stop_result(&[]), "clean");
        assert_eq!(stop_result(&[gone.clone(), none.clone()]), "clean");

        let mut killed = gone.clone();
        killed.group = Some(GroupEnd::KillAttempted);
        assert_eq!(stop_result(&[gone.clone(), killed]), "degraded");
        let mut cut = gone.clone();
        cut.supervisor = Some(SupervisorEnd::CutOff);
        assert_eq!(stop_result(&[cut.clone()]), "degraded");

        let open = rec(ProcessAtStop::Running);
        assert_eq!(stop_result(&[cut, open]), "timed_out");
        let open_none = rec(ProcessAtStop::None);
        assert_eq!(stop_result(&[gone, open_none]), "timed_out");
        // A grace that ran out is not a bad shutdown — it is warned about.
        let mut slow = rec(ProcessAtStop::Running);
        slow.group = Some(GroupEnd::Gone);
        slow.leader = Some(Leader::KilledAfterGrace);
        slow.drain = Some(DrainEnd {
            ended_by: DrainEndedBy::Deadline,
            budget: Duration::from_millis(800),
            elapsed: Duration::from_millis(800),
        });
        assert_eq!(stop_result(&[slow]), "clean");
    }

    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn summary(id: &str) -> Summary {
        Summary::new(id, &Params::default(), Duration::from_millis(40), &[])
    }

    /// Every row of the design's table, the marker first.
    #[test]
    fn the_previous_daemon_is_told_from_the_two_files() {
        let d = dir();
        let run = d.path();
        assert_eq!(previous(run), Previous::NoHistory);

        std::fs::write(
            run.join(SUMMARY),
            serde_json::to_vec(&summary("a")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            previous(run),
            Previous::Clean {
                stop_result: "clean".into()
            }
        );

        std::fs::write(run.join(SUMMARY), b"{half").unwrap();
        assert_eq!(previous(run), Previous::Unreadable);

        let alive = |id: &str| {
            serde_json::to_vec(&Alive {
                instance_id: id.into(),
                pid: 1,
                started_at_ms: 0,
            })
            .unwrap()
        };
        std::fs::write(run.join(ALIVE), alive("a")).unwrap();
        assert_eq!(
            previous(run),
            Previous::Unconfirmed,
            "marker, unreadable summary"
        );
        std::fs::write(
            run.join(SUMMARY),
            serde_json::to_vec(&summary("a")).unwrap(),
        )
        .unwrap();
        assert_eq!(previous(run), Previous::CleanupFailed);
        std::fs::write(
            run.join(SUMMARY),
            serde_json::to_vec(&summary("b")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            previous(run),
            Previous::Unconfirmed,
            "another instance's summary"
        );
        std::fs::remove_file(run.join(SUMMARY)).unwrap();
        assert_eq!(previous(run), Previous::Unconfirmed, "no summary");
        std::fs::write(run.join(ALIVE), b"garbage").unwrap();
        std::fs::write(
            run.join(SUMMARY),
            serde_json::to_vec(&summary("a")).unwrap(),
        )
        .unwrap();
        assert_eq!(previous(run), Previous::Unconfirmed, "an unreadable marker");
    }

    /// A start-up that fails removes its marker; one the shutdown took over
    /// keeps it until the summary is on disk, and then removes it — only its
    /// own.
    #[test]
    fn the_marker_is_removed_by_a_failed_start_or_a_summary_on_disk_only() {
        let d = dir();
        let run = d.path().join("run");
        let (guard, warning) = MarkerGuard::create(&run);
        assert!(warning.is_none(), "{warning:?}");
        assert!(run.join(ALIVE).exists());
        drop(guard);
        assert!(!run.join(ALIVE).exists(), "a failed start left its marker");

        let (guard, _) = MarkerGuard::create(&run);
        let stopping = guard.into_stopping();
        assert!(run.join(ALIVE).exists(), "handing over removed the marker");
        let s = summary(stopping.instance_id());
        persist(&s, &stopping).unwrap();
        assert!(!run.join(ALIVE).exists());
        assert_eq!(
            previous(&run),
            Previous::Clean {
                stop_result: "clean".into()
            }
        );

        // Another instance's marker is not removed by this one's summary.
        let (other, _) = MarkerGuard::create(&run);
        let other = other.into_stopping();
        persist(&summary("someone-else"), &stopping).unwrap();
        assert!(
            run.join(ALIVE).exists(),
            "removed another instance's marker"
        );
        drop(other);
    }

    /// A summary that cannot be written leaves the marker: the next start
    /// then cannot confirm this shutdown, which is the truth.
    #[cfg(unix)]
    #[test]
    fn a_summary_that_cannot_be_written_leaves_the_marker() {
        use std::os::unix::fs::PermissionsExt;
        let d = dir();
        let run = d.path().join("run");
        let (guard, _) = MarkerGuard::create(&run);
        let stopping = guard.into_stopping();
        std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o500)).unwrap();
        let written = persist(&summary(stopping.instance_id()), &stopping);
        std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(written.is_err());
        assert!(run.join(ALIVE).exists());
        assert_eq!(previous(&run), Previous::Unconfirmed);
    }

    /// A marker that could not be written still leaves this run a summary:
    /// only the crash evidence is lost, and that is warned about (review of
    /// SHUT-1b, round 1).
    #[cfg(unix)]
    #[test]
    fn a_marker_that_cannot_be_written_still_gets_a_summary() {
        use std::os::unix::fs::PermissionsExt;
        let d = dir();
        let run = d.path().join("run");
        std::fs::create_dir(&run).unwrap();
        std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o500)).unwrap();
        let (guard, warning) = MarkerGuard::create(&run);
        std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(warning.unwrap().contains("no crash evidence"));
        assert!(!run.join(ALIVE).exists());
        let stopping = guard.into_stopping();
        persist(&summary(stopping.instance_id()), &stopping).unwrap();
        assert!(run.join(SUMMARY).exists());
    }

    /// Only a small regular file is read: a link or a huge file is not ours,
    /// and says so rather than being followed or loaded.
    #[cfg(unix)]
    #[test]
    fn evidence_is_read_only_from_small_regular_files() {
        let d = dir();
        let run = d.path();
        std::os::unix::fs::symlink("/dev/zero", run.join(SUMMARY)).unwrap();
        assert_eq!(previous(run), Previous::Unreadable);
        std::fs::remove_file(run.join(SUMMARY)).unwrap();
        let f = std::fs::File::create(run.join(SUMMARY)).unwrap();
        f.set_len(MAX_EVIDENCE_BYTES + 1).unwrap();
        assert_eq!(previous(run), Previous::Unreadable);
        // A FIFO in its place would block a read for ever — and with it every
        // start. Not a regular file: not read at all.
        std::fs::remove_file(run.join(SUMMARY)).unwrap();
        assert!(
            std::process::Command::new("mkfifo")
                .arg(run.join(SUMMARY))
                .status()
                .unwrap()
                .success()
        );
        let (tx, rx) = std::sync::mpsc::channel();
        let dir = run.to_owned();
        std::thread::spawn(move || {
            let _ = tx.send(previous(&dir));
        });
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5))
                .expect("a FIFO blocked the start"),
            Previous::Unreadable
        );
    }

    /// The live report says what the start-up log said: the budgets, the
    /// warnings, the previous daemon, and which modules the last shutdown
    /// found too slow — the ones to tune for.
    #[test]
    fn the_report_names_the_modules_to_tune_for() {
        let mut slow = rec(ProcessAtStop::Running);
        slow.group = Some(GroupEnd::Gone);
        slow.leader = Some(Leader::KilledAfterGrace);
        let mut cut = rec(ProcessAtStop::Running);
        cut.group = Some(GroupEnd::Gone);
        cut.abandoned = Some(1);
        cut.never_sent = Some(2);
        cut.drain = Some(DrainEnd {
            ended_by: DrainEndedBy::Deadline,
            budget: Duration::from_millis(800),
            elapsed: Duration::from_millis(800),
        });
        let last = Summary::new(
            "x",
            &Params::default(),
            Duration::from_millis(900),
            &[
                ("slow".into(), Reason::Shutdown, slow),
                ("busy".into(), Reason::Shutdown, cut),
            ],
        );
        let d = dir();
        let r = report(
            &Params::default(),
            &["A24_MODULE_DRAIN_MS=\"x\" is not…".to_owned()],
            Some((d.path(), &Previous::Unconfirmed, Some(last))),
        );
        assert!(!r.ephemeral);
        assert_eq!(
            (r.drain_ms, r.stop_grace_ms, r.exit_bound_ms),
            (800, 500, 2000)
        );
        assert_eq!(r.config_warnings.len(), 1);
        assert_eq!(r.previous, "unconfirmed");
        assert!(r.previous_detail.unwrap().contains("did not confirm"));
        let last = r.last_shutdown.unwrap();
        assert_eq!(last.killed_after_grace, ["slow"]);
        assert_eq!(last.cut_requests, ["busy (3)"]);

        let e = report(&Params::default(), &[], None);
        assert!(e.ephemeral && e.evidence_dir.is_none() && e.last_shutdown.is_none());
        assert_eq!(e.previous, "no_history");
    }

    /// A summary on disk may carry figures past what the schema admits; the
    /// report clamps them rather than hand a JavaScript client a number it
    /// cannot hold (review of SHUT-1c, round 2).
    #[test]
    fn the_report_never_exceeds_the_json_safe_integer() {
        let mut last = Summary::new("x", &Params::default(), Duration::ZERO, &[]);
        last.began_at_ms = u128::from(u64::MAX) + 1;
        last.took_ms = u128::from(JSON_SAFE_MAX) + 1;
        last.omitted_records = usize::MAX;
        let d = dir();
        let r = report(
            &Params::default(),
            &[],
            Some((d.path(), &Previous::NoHistory, Some(last))),
        );
        let last = r.last_shutdown.unwrap();
        assert_eq!(
            (last.began_at_ms, last.took_ms, last.omitted_records),
            (JSON_SAFE_MAX, JSON_SAFE_MAX, JSON_SAFE_MAX)
        );
        assert_eq!(ms(u128::from(JSON_SAFE_MAX)), JSON_SAFE_MAX);
        assert_eq!(ms(7), 7);
    }

    /// A summary too big to be read back is cut to fit, and says how much it
    /// left out — a start never meets its own summary as unreadable.
    #[test]
    fn a_huge_summary_is_cut_to_what_a_start_reads_back() {
        let mut r = rec(ProcessAtStop::Running);
        r.group = Some(GroupEnd::Gone);
        let records: Vec<_> = (0..20_000)
            .map(|i| (format!("module-{i:05}"), Reason::Shutdown, r.clone()))
            .collect();
        let s = Summary::new(
            "big",
            &Params::default(),
            Duration::from_millis(5),
            &records,
        );
        let bytes = bounded(&s).unwrap();
        assert!(bytes.len() as u64 <= MAX_EVIDENCE_BYTES);
        let back: Summary = serde_json::from_slice(&bytes).unwrap();
        assert!(back.omitted_records > 0);
        assert_eq!(back.records.len() + back.omitted_records, 20_000);
    }

    /// The file's shape, pinned: what SHUT-1c and a person reading it rely on.
    #[test]
    fn the_summary_file_has_the_documented_shape() {
        let s = summary("id");
        let v = serde_json::to_value(&s).unwrap();
        let keys: std::collections::BTreeSet<_> = v.as_object().unwrap().keys().cloned().collect();
        assert_eq!(
            keys,
            [
                "version",
                "instance_id",
                "began_at_ms",
                "took_ms",
                "stop_result",
                "params",
                "records",
                "omitted_records"
            ]
            .into_iter()
            .map(String::from)
            .collect()
        );
        assert_eq!(
            v["params"],
            serde_json::json!({
                "drain_ms": 800, "stop_grace_ms": 500, "http_deadline_ms": 1500,
                "module_deadline_ms": 1500, "persist_deadline_ms": 1700, "watchdog_ms": 2000
            })
        );
    }

    /// The log line names each module's end, its drain, what was cut, how the
    /// leader went and how long the stop took.
    #[test]
    fn the_summary_line_says_what_happened_to_each_module() {
        let mut r = rec(ProcessAtStop::Running);
        r.group = Some(GroupEnd::Gone);
        r.leader = Some(Leader::KilledAfterGrace);
        r.stop_elapsed = Some(Duration::from_millis(512));
        r.abandoned = Some(1);
        r.never_sent = Some(1);
        r.drain = Some(DrainEnd {
            ended_by: DrainEndedBy::Deadline,
            budget: Duration::from_millis(800),
            elapsed: Duration::from_millis(800),
        });
        let s = Summary::new(
            "x",
            &Params::default(),
            Duration::from_millis(1120),
            &[("y".into(), Reason::Shutdown, r)],
        );
        assert_eq!(
            s.describe(),
            "shutdown took 1120ms, clean: y (gone, drain deadline 800ms, cut 2, killed after grace, stop 512ms)"
        );
        assert_eq!(s.records[0].leader.as_deref(), Some("killed_after_grace"));

        let mut none = rec(ProcessAtStop::None);
        none.supervisor = Some(SupervisorEnd::Stopped);
        let s = Summary::new(
            "x",
            &Params::default(),
            Duration::from_millis(3),
            &[("z".into(), Reason::Shutdown, none)],
        );
        assert_eq!(
            s.describe(),
            "shutdown took 3ms, clean: z (no process, stopped)"
        );
    }
}
