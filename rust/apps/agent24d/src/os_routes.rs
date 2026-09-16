//! `/api/v1/os` — inspect and toggle domain OSes (ME-2b).
//!
//! **The daemon owns the write; the CLI never touches `os.json`.** That is the
//! whole design, and it buys three things:
//!
//! 1. **One PLACE that writes**, so the schema lives once. It does NOT make
//!    writes serial by itself — axum handlers run concurrently and ephemeral
//!    daemons are exempt from the singleton lock — which is why
//!    `OsConfig::set_enabled` takes a cross-process file lock. An earlier version
//!    of this comment claimed "one writer, no lock", and that was simply wrong.
//! 2. **A typo fails NOW.** The daemon knows which modules it provides, so
//!    `agent24 os disable sin09` is refused at the moment it is typed — naming
//!    the modules that do exist — instead of writing a file that bricks the whole
//!    registry at the next start. That closes the route THIS command opens; it
//!    does not repair an entry written by hand or by an older build, which still
//!    needs the file edited (the list reports such an entry as the reason every
//!    module is degraded).
//! 3. **The CLI stays thin**, and does not need to duplicate the schema.
//!
//! It costs one thing, and the cost lands exactly where it hurts: if a domain OS
//! is what keeps the daemon from STARTING, the tool for switching it off needs
//! the daemon. `os.json` is plain JSON and the user can always edit it — the CLI
//! prints the exact edit when it cannot reach the daemon — but that is a
//! documented escape hatch, not a second writer. Making the CLI write the file
//! whenever the daemon is down would be the tidier-looking answer and the wrong
//! one: it would mean two writers, and the failure it prevents is rarer than the
//! races it would introduce.
//!
//! **Disabling a running out-of-process module stops it now; every other toggle
//! takes effect when the daemon restarts** (SUP-5). A disable stops the module
//! the SPEC §4 way — DRAINING, then REVOKING — in the background: the request
//! returns once the config is written AND the module refuses new requests,
//! and the list reports it `disabled` (with `stopping` while it drains; a
//! stop that fails is reported `degraded`, as what it is). A compiled-in module, and ANY enable, still
//! waits for a restart: routes are built once at startup, and nothing starts a
//! module at runtime. The list reports the config state AND the running state
//! separately, and sets `restart_required` when the config differs from what is
//! running — not merely when a module is enabled and not running, which would
//! conflate a pending toggle with a module that is simply unhealthy.

use agent24_domain::http::{RESTART_DAEMON_INSTRUCTION, error_response, error_response_with_hint};
use agent24_protocol::{DomainOsList, DomainOsUpdate, DomainOsView};
use axum::Json;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::domain::{MountOutcome, MountReport, ResourceStatus};
use crate::routes::read_body_or_response;
use crate::server::AppState;

/// Render one module for the wire, combining what the config says NOW with what
/// the daemon did at startup.
fn view(
    report: &MountReport,
    enabled_now: bool,
    registry_usable: bool,
    live: Option<&agent24_os_proto::supervisor::Status>,
    hot: Option<bool>,
) -> DomainOsView {
    use agent24_os_proto::supervisor::Status;
    // `hot`: `None` if no `os disable` asked to stop it since the daemon
    // started; else whether that has taken effect (see `applied`). A disable
    // whose stop failed is not applied: the module is reported as what it is
    // — degraded, with the failure — and a restart is still what settles it
    // (review of SUP-5, round 1), whatever the config says since (round 5).
    let stop_failed = hot.is_some()
        && matches!(
            live,
            Some(Status::StopFailed { .. } | Status::Panicked | Status::Killed)
        );
    let hot_disabled = hot == Some(true) && !stop_failed;
    let (state, detail) = match (&report.outcome, live) {
        // Stopped by `os disable` while the daemon runs: disabled, whatever the
        // mount said — still `stopping` while it drains (SUP-5).
        (MountOutcome::Mounted, Some(Status::Stopped)) if hot_disabled => ("disabled", None),
        (MountOutcome::Mounted, _) if hot_disabled => ("disabled", Some("stopping".to_owned())),
        // Asked to stop and still admitting: what it is, and what is coming —
        // whatever its supervisor has published yet (review of SUP-5, round 7).
        (MountOutcome::Mounted, _) if hot == Some(false) && !stop_failed => {
            ("mounted", Some("stop requested".to_owned()))
        }
        // A package started at mount: its supervisor says where it is NOW. A
        // mount verdict alone called a module that had since given up
        // "mounted" (review of SUP-4, round 1).
        (MountOutcome::Mounted, Some(status)) => match status {
            Status::Running => ("mounted", None),
            Status::Starting { attempt, after } => (
                "mounted",
                Some(match after {
                    // FU-57: what the run before this one failed of.
                    Some(failed) => format!("starting (run {attempt}) — after: {failed}"),
                    None => format!("starting (run {attempt})"),
                }),
            ),
            Status::Stopping => ("mounted", Some("stopping".to_owned())),
            Status::Backoff {
                failures,
                delay,
                last,
            } => (
                "degraded",
                Some(format!(
                    "restarting in {}ms after {failures} failed run(s) — last: {last}",
                    delay.as_millis()
                )),
            ),
            Status::GaveUp {
                failures,
                within,
                last,
            } => (
                "degraded",
                Some(format!(
                    "gave up after {failures} failed runs within {}s — last: {last}",
                    within.as_secs()
                )),
            ),
            // FU-61: caught right before what would have been the next
            // restart — not a run failure, so it does not go through
            // `RunFailure`'s `Display`. Recovery is a daemon restart:
            // `os disable`/`enable` cannot bring the supervisor back
            // (`enable` only changes config, it does not create a new
            // supervisor in a running daemon — see the design doc).
            Status::PackageChanged { reason } => (
                "degraded",
                Some(format!(
                    "package changed: {reason} — restart the daemon to pick up the current \
                     package (`agent24 daemon stop && agent24 daemon start`)"
                )),
            ),
            Status::StopFailed { error } => ("degraded", Some(error.clone())),
            Status::Stopped => ("degraded", Some("stopped".to_owned())),
            Status::Panicked => ("degraded", Some("its supervisor panicked".to_owned())),
            Status::Killed => (
                "degraded",
                Some(
                    "its supervisor was cancelled; SIGKILL attempted, exit unconfirmed".to_owned(),
                ),
            ),
        },
        (MountOutcome::Mounted, None) => ("mounted", None),
        (MountOutcome::Disabled, _) => ("disabled", None),
        (MountOutcome::Degraded(why), _) => ("degraded", Some(why.clone())),
        (MountOutcome::Refused(why), _) => ("refused", Some(why.clone())),
    };
    let (resources, missing_models) = match &report.resources {
        ResourceStatus::NotChecked => ("not_checked", Vec::new()),
        ResourceStatus::Satisfied => ("ok", Vec::new()),
        ResourceStatus::MissingModels(m) => ("missing", m.clone()),
        ResourceStatus::Unknown(_) => ("unknown", Vec::new()),
    };
    // "Has the config changed since we mounted?" — NOT "is it running?". Comparing
    // against RUNNING conflated a pending toggle with a module that is enabled and
    // merely unhealthy: the first is fixed by a restart, the second is not, and it
    // also missed a disable applied to an already-degraded module.
    //
    // A REFUSED module is excluded whatever the config says: its manifest is
    // inadmissible for this binary, so a restart cannot deliver it, and asking for
    // one would send the user to do something that changes nothing.
    // A hot disable has already applied "off" — or has asked for it, which
    // cannot be taken back: for the comparison below, what is running is what
    // a start with it disabled would have given. So an enable that lands
    // before the stop takes hold still needs a restart (review of SUP-5,
    // round 6).
    let running_enabled = if hot.is_some() {
        Some(false)
    } else {
        report.enabled_at_start
    };
    let restart_required = match running_enabled {
        _ if matches!(report.outcome, MountOutcome::Refused(_)) => false,
        // A disable left it with no supervisor that can bring it back: a
        // later enable changes the config, not that — nor does fixing the
        // registry (review of SUP-5, rounds 5 and 6).
        _ if stop_failed => true,
        // A registry that is STILL unusable cannot be applied by restarting — the
        // fix is the file. Saying otherwise sent the user to restart into exactly
        // the same degradation. (The syntactically-invalid case never reaches here;
        // it fails the load. This is the semantic one: a file that parses but
        // disables something the build does not provide.)
        _ if !registry_usable => false,
        // It WAS unusable at startup and is usable now, so the current config has
        // never been applied.
        None => true,
        Some(then) => then != enabled_now,
    };

    DomainOsView {
        name: report.name.clone(),
        namespace: report.namespace.clone(),
        version: report.version.clone(),
        enabled: enabled_now,
        state: state.to_owned(),
        detail,
        granted: report.granted.clone(),
        missing_models,
        resources: resources.to_owned(),
        restart_required,
    }
}

/// Re-run the SEMANTIC registry check against what this build provides.
///
/// `OsConfig::load` only proves the file PARSES. An entry that disables a module
/// nothing provides parses perfectly while leaving the module the user meant to
/// switch off running — the mounter rejects that at startup, and the view has to
/// reach the same verdict or it would keep telling the user a restart will apply a
/// file that is still broken.
///
/// Extracted rather than inlined so it can be tested: `render` reads a path from
/// the environment, which a unit test cannot steer.
fn semantic_registry_error<'a>(
    cfg: &crate::os_config::OsConfig,
    provided: impl Iterator<Item = &'a str>,
) -> Option<String> {
    let provided: std::collections::BTreeSet<&str> = provided.collect();
    let unknown = cfg.unknown_disabled(&provided);
    (!unknown.is_empty()).then(|| {
        format!(
            "os.json disables {unknown:?}, which this build does not provide — so \
             the module you meant to switch off is still running. Remove that entry \
             (your other settings are fine), or set \"default\": \"disabled\" to \
             use an allow-list. Restarting will not help until the file is fixed."
        )
    })
}

fn render(state: &AppState) -> Response {
    render_at(state, crate::os_config::config_path())
}

/// [`render`], reading the config from an injected path instead of the
/// process's real `$HOME` (T8/ME-3g — the same reason `patch_os_at` exists:
/// a unit test needs to assert on the file it just wrote, not on whatever
/// happens to be at `~/.agent24/os.json` on the machine running the test).
fn render_at(state: &AppState, path: Option<std::path::PathBuf>) -> Response {
    // Read the config fresh: it may have been changed since startup, by this very
    // process, and the point of the view is to show that divergence.
    let cfg = path
        .ok_or_else(|| "HOME not set".to_owned())
        .and_then(|p| crate::os_config::OsConfig::load(&p));
    let cfg = match cfg {
        Ok(c) => c,
        Err(why) => {
            return error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "registry_invalid",
                &format!("os.json could not be read: {why}"),
            );
        }
    };
    let registry_error =
        semantic_registry_error(&cfg, state.os_reports.iter().map(|r| r.name.as_str()));
    let modules = state
        .os_reports
        .iter()
        .map(|r| {
            let live = state
                .module_status
                .get(&r.name)
                .map(|rx| rx.borrow().clone());
            view(
                r,
                cfg.is_enabled(&r.name),
                registry_error.is_none(),
                live.as_ref(),
                hot_disabled(state, &r.name),
            )
        })
        .collect();
    Json(DomainOsList {
        modules,
        registry_error,
    })
    .into_response()
}

fn hot_disabled(state: &AppState, name: &str) -> Option<bool> {
    let slot = state
        .supervisors
        .as_ref()
        .and_then(|s| s.disabled_slot(name))?;
    Some(applied(Some(&slot)))
}

/// Whether a disable has taken effect: its module no longer admits requests.
/// Asked for but not yet so, it is still what it was — `mounted`, and
/// serving (review of SUP-5, round 4).
fn applied(disabled: Option<&crate::domain::Disabled>) -> bool {
    disabled
        .is_some_and(|d| d.current.get().state() != agent24_os_proto::drain::DrainState::Running)
}

/// How long a disabled module gets to finish the requests it has (DRAINING)
/// before it is stopped. ⚖️ The proxy's own total deadline for a request, so
/// every request admitted before the disable either finishes or times out by
/// itself first — a disable abandons nothing a request's own deadline would not.
const DISABLE_DRAIN: std::time::Duration = std::time::Duration::from_secs(30);

/// How long a disable waits for the module's generation to stop admitting
/// requests. Its supervisor does that as soon as it is scheduled; this only
/// bounds a runtime too busy to schedule it.
const ADMISSION_CLOSED_WITHIN: std::time::Duration = std::time::Duration::from_secs(2);

/// What a disable did to the running module (SUP-5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HotStop {
    /// Its supervisor is now draining and stopping it, and it refuses new
    /// requests.
    Stopping,
    /// It still admitted requests when the wait ran out — whether this
    /// disable or an earlier one asked for the stop.
    Pending,
    /// An earlier disable asked for the stop, and it refuses requests.
    Already,
    /// Its supervisor could not stop it cleanly (`StopFailed`, `Panicked`,
    /// `Killed`): not a disable applied (review of SUP-5, round 4).
    Failed,
    /// Nothing running to stop: a compiled-in module, a package not started
    /// at mount, or a daemon already shutting down.
    NotRunning,
}

/// Hand a running out-of-process module's stop to its supervisor (SUP-5):
/// it drains and stops in the background, and the shutdown — if it begins
/// meanwhile — waits for that stop, cutting it off at `cut_off`. Synchronous:
/// the stop is asked for before this returns. The module's proxy slot, and
/// whether THIS call asked (`true`) or an earlier disable did; `None` if
/// there is nothing to stop.
fn hand_off(
    supervisors: Option<&crate::domain::Supervisors>,
    cut_off: impl std::future::Future<Output = ()> + Send + 'static,
    name: &str,
) -> Option<(bool, crate::domain::Disabled)> {
    let supervisors = supervisors?;
    match supervisors.disable(name, DISABLE_DRAIN, cut_off) {
        Some(current) => Some((true, current)),
        None => supervisors.disabled_slot(name).map(|c| (false, c)),
    }
}

/// Wait up to `within` for a handed-off module to refuse new requests, so a
/// request sent after the disable answers is not admitted. What this
/// establishes is "refuses requests, stop under way": a failure its
/// supervisor has already reported by then is `Failed`, and one that comes
/// later is the list's to report — `degraded`, restart required — since
/// waiting for the stop to finish would hold the answer for the whole drain
/// (review of SUP-5, round 5). Admission is closed — also on a
/// repeated disable, whose predecessor may have timed out (review of SUP-5,
/// round 3).
async fn settle(
    handed: Option<(bool, crate::domain::Disabled)>,
    within: std::time::Duration,
) -> HotStop {
    match handed {
        None => HotStop::NotRunning,
        Some((asked, d)) => {
            let closed = admission_closed(&d.current, within).await;
            let status = d.status.borrow().clone();
            classify(closed, asked, &status)
        }
    }
}

/// The answer's last look: a stop that has failed since `settle` looked is
/// `Failed` after all, and one that timed out but refuses requests by now is
/// under way. This moment — just before the response is chosen — is the one
/// the answer describes (review of SUP-5, rounds 6 and 7).
fn last_look(hot: HotStop, slot: Option<&crate::domain::Disabled>) -> HotStop {
    // Test-only call counter (compiles to nothing in a release build): the
    // one thing round-2 code review found impossible to pin any other way
    // is "does `settled_and_reconciled` actually call this function" — its
    // CORRECTIVE effect (a status that changes between `settle` and this
    // call) has no scheduler yield point to exploit deterministically, but
    // whether it is called AT ALL is trivially, deterministically provable
    // this way.
    #[cfg(test)]
    tests::LAST_LOOK_CALLS.with(|n| n.set(n.get() + 1));
    use agent24_os_proto::supervisor::Status;
    let failed = slot.is_some_and(|d| {
        matches!(
            *d.status.borrow(),
            Status::StopFailed { .. } | Status::Panicked | Status::Killed
        )
    });
    if failed {
        HotStop::Failed
    } else if hot == HotStop::Pending && applied(slot) {
        HotStop::Stopping
    } else {
        hot
    }
}

/// `settle` → `last_look`, in one place — shared by `patch_os` and
/// `stop_now_os` (FU-61) so the reconciliation `last_look` does (a one-shot
/// `settle()` alone can miss a `Stopping` that becomes `StopFailed` moments
/// later, or a `Pending` that closes admission just after its own timeout)
/// cannot silently drift out of sync between the two routes, and a test of
/// this one function protects both call sites. Takes `handed` rather than
/// calling `hand_off` itself: `patch_os` already has it from `apply()`.
async fn settled_and_reconciled(
    handed: Option<(bool, crate::domain::Disabled)>,
    within: std::time::Duration,
) -> HotStop {
    let slot = handed.as_ref().map(|(_, d)| d.clone());
    last_look(settle(handed, within).await, slot.as_ref())
}

/// A handed-off stop, by whether its module refuses requests, whether THIS
/// disable asked for it, and its supervisor's status. A failed stop revokes
/// admission too, so the failure is looked at first.
fn classify(closed: bool, asked: bool, status: &agent24_os_proto::supervisor::Status) -> HotStop {
    use agent24_os_proto::supervisor::Status;
    if matches!(
        status,
        Status::StopFailed { .. } | Status::Panicked | Status::Killed
    ) {
        return HotStop::Failed;
    }
    match (closed, asked) {
        (false, _) => HotStop::Pending,
        (true, true) => HotStop::Stopping,
        (true, false) => HotStop::Already,
    }
}

#[cfg(test)]
async fn stop_now(
    supervisors: Option<&crate::domain::Supervisors>,
    cut_off: impl std::future::Future<Output = ()> + Send + 'static,
    name: &str,
    within: std::time::Duration,
) -> HotStop {
    settle(hand_off(supervisors, cut_off, name), within).await
}

/// Write `enabled` for `name` to os.json and, for a disable, hand the
/// running module's stop off. `Err` only if the change was not published;
/// published with an error — a rename that landed, then a failed directory
/// fsync — still hands the stop off, so the running state matches what the
/// file now says, and returns that error alongside (review of SUP-5,
/// round 3).
async fn apply(
    state: &AppState,
    path: std::path::PathBuf,
    name: &str,
    enabled: bool,
) -> Result<(Option<(bool, crate::domain::Disabled)>, Option<String>), String> {
    // Off the async workers: the file lock waits for any other writer, for
    // as long as that takes (review of SUP-5, round 2).
    let written = tokio::task::spawn_blocking({
        let (path, name) = (path.clone(), name.to_owned());
        move || crate::os_config::OsConfig::set_enabled(&path, &name, enabled)
    })
    .await;
    let failed = match written {
        Ok(Ok(_)) => None,
        Ok(Err(why)) => Some(why),
        Err(e) => Some(format!("the config write failed: {e}")),
    };
    if let Some(why) = &failed {
        let published = tokio::task::spawn_blocking({
            let name = name.to_owned();
            move || {
                crate::os_config::OsConfig::load(&path)
                    .is_ok_and(|c| c.is_enabled(&name) == enabled)
            }
        })
        .await
        .unwrap_or(false);
        if !published {
            return Err(why.clone());
        }
    }
    let handed = if enabled {
        None
    } else {
        hand_off(
            state.supervisors.as_deref(),
            state.shutdown.modules_cut_off(),
            name,
        )
    };
    Ok((handed, failed))
}

/// Wait, for up to `within`, until the generation `current` routes to no
/// longer admits requests. `false` if it still did then.
async fn admission_closed(
    current: &agent24_os_proto::drain::Current,
    within: std::time::Duration,
) -> bool {
    let by = tokio::time::Instant::now() + within;
    while current.get().state() == agent24_os_proto::drain::DrainState::Running {
        if tokio::time::Instant::now() >= by {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
    true
}

pub async fn list_os(State(state): State<AppState>) -> Response {
    render(&state)
}

/// The `hint` both `patch_os` and `stop_now_os` give for their `stop_failed`
/// (control-plane) response — one function, not two hand-copied `format!`
/// call sites that could drift apart (code review round 1 Low 2c: this is
/// what makes the two control-plane restart hints unit-testable without
/// standing up a full failing-supervisor HTTP fixture for a static string).
fn control_plane_stop_failed_hint() -> String {
    format!("`agent24 os list` shows why; {RESTART_DAEMON_INSTRUCTION}")
}

/// Same reasoning as [`control_plane_stop_failed_hint`], for the
/// `disable_pending` response both endpoints share verbatim.
const CONTROL_PLANE_DISABLE_PENDING_HINT: &str =
    "check `agent24 os list` shortly to see if it has stopped; this request can be retried";

/// `patch_os`'s `stop_failed` response, extracted so its exact `message`
/// shape is unit-testable without driving the whole handler through a
/// failing-supervisor fixture (code review round 2 Low 3c — the earlier
/// test only pinned the shared `hint` in isolation, not what either
/// endpoint's response actually serializes). `write_error_note` is the
/// `"; writing os.json also reported: {why}"` suffix, or empty — built by
/// the caller, which already has `write_error` in scope.
fn patch_os_stop_failed_response(name: &str, write_error_note: &str) -> Response {
    error_response_with_hint(
        StatusCode::INTERNAL_SERVER_ERROR,
        "stop_failed",
        &format!(
            "this request disabled {name:?} in os.json, but its supervisor could not stop it \
             cleanly; `agent24 os list` shows why, and a restart is needed{write_error_note}"
        ),
        &control_plane_stop_failed_hint(),
    )
}

/// `patch_os`'s `disable_pending` response — see
/// [`patch_os_stop_failed_response`].
fn patch_os_disable_pending_response(name: &str) -> Response {
    error_response_with_hint(
        StatusCode::SERVICE_UNAVAILABLE,
        "disable_pending",
        &format!(
            "this request disabled {name:?} in os.json and asked its supervisor to stop it, \
             but it still admitted requests after {ADMISSION_CLOSED_WITHIN:?}; `agent24 os list` \
             shows when it has stopped"
        ),
        CONTROL_PLANE_DISABLE_PENDING_HINT,
    )
}

/// `stop_now_os`'s `stop_failed` response — see
/// [`patch_os_stop_failed_response`]. A shorter `message` than `patch_os`'s:
/// this endpoint never wrote `os.json`, so there is no "this request
/// disabled ... in os.json" to say.
fn stop_now_os_stop_failed_response(name: &str) -> Response {
    error_response_with_hint(
        StatusCode::INTERNAL_SERVER_ERROR,
        "stop_failed",
        &format!("could not stop {name:?} cleanly; `agent24 os list` shows why"),
        &control_plane_stop_failed_hint(),
    )
}

/// `stop_now_os`'s `disable_pending` response — see
/// [`patch_os_stop_failed_response`].
fn stop_now_os_disable_pending_response(name: &str) -> Response {
    error_response_with_hint(
        StatusCode::SERVICE_UNAVAILABLE,
        "disable_pending",
        &format!(
            "asked {name:?}'s supervisor to stop it, but it still admitted requests after \
             {ADMISSION_CLOSED_WITHIN:?}; `agent24 os list` shows when it has stopped"
        ),
        CONTROL_PLANE_DISABLE_PENDING_HINT,
    )
}

/// T8/ME-3g: `enable` on a name whose only `os_reports` entry is already
/// `Refused` — see the design doc's "在哪加、加什么". `why` is used verbatim
/// as `message` (it is the same string `agent24 os list` already shows as
/// `detail`), so the two never say different things about the same module.
fn admission_refused_response(why: &str) -> Response {
    error_response_with_hint(
        StatusCode::CONFLICT,
        "admission_refused",
        why,
        &format!(
            "`agent24 os list` shows the reason; correct it, then {RESTART_DAEMON_INSTRUCTION}, \
             and retry enable once the module is re-admitted"
        ),
    )
}

/// T8/ME-3g §"重扫挡住时的 hint": deliberately NOT the same hint as
/// [`admission_refused_response`] — the `MountReport` behind this path still
/// just says `Disabled`, so the real reason only exists in this one response,
/// and the right sequence is fix-on-disk → retry `enable` (this time it
/// persists) → restart once, not "go look at `agent24 os list`".
fn rescan_blocked_response(message: &str) -> Response {
    error_response_with_hint(
        StatusCode::CONFLICT,
        "admission_refused",
        message,
        "the reason is above, not in `agent24 os list` (this module is still recorded as \
         disabled there); fix it on disk, then retry enable — once it passes, restart the \
         daemon so it re-admits the module",
    )
}

/// T8/ME-3g: everything this needs is on disk, so it runs entirely off the
/// async executor (`spawn_blocking` — this does real filesystem I/O:
/// `read_dir`, file reads, hashing, YAML parsing, mirroring how
/// `OsConfig::set_enabled` is already dispatched a few lines down in
/// `apply`). `Ok(())` means the package still looks admissible and the
/// `enable` write may proceed; `Err(message)` is the client-facing reason to
/// block it with, via [`rescan_blocked_response`].
///
/// `dir` is `name`'s directory as recorded in `AppState.package_dirs` at
/// startup — matching is by directory, not by name, because a manifest that
/// now fails to parse may not yield a trustworthy name at all (a `Refused`
/// entry only carries `dir`/`why`).
fn rescan_disabled_package_on_disk(
    packages_root: &std::path::Path,
    name: &str,
    dir: &std::path::Path,
) -> Result<(), String> {
    if let Err(e) = agent24_os_packages::check_packages_root(packages_root) {
        return Err(format!(
            "the directory installed packages live under ({}) does not check out: {e}",
            packages_root.display()
        ));
    }
    let scan = agent24_os_packages::discovery::scan(packages_root);
    classify_scan(&scan, packages_root, name, dir)
}

/// The pure part of [`rescan_disabled_package_on_disk`] — given a `Scan`
/// result (real or, in tests, hand-built), decides whether `name`/`dir`
/// still looks admissible. Split out so judgement criterion 15b can inject a
/// root-level `Refused` deterministically instead of depending on real
/// permission bits actually making `read_dir` fail (which root-run tests
/// would not observe — code review round 1 Low 3).
fn classify_scan(
    scan: &agent24_os_packages::discovery::Scan,
    packages_root: &std::path::Path,
    name: &str,
    dir: &std::path::Path,
) -> Result<(), String> {
    // `check_packages_root` only inspects metadata (symlink/type/ownership/
    // write bits) — it does not itself try `read_dir`, so a root that passes
    // it can still fail here. `scan()` reports that as a `Refused` whose
    // `dir` IS the root, not any package's directory; caught first, or it
    // gets misattributed to "this package's directory disappeared" below.
    if let Some(root_refusal) = scan
        .refused
        .iter()
        .find(|r| r.dir.as_path() == packages_root)
    {
        return Err(format!(
            "the directory installed packages live under ({}) could not be read: {}",
            packages_root.display(),
            root_refusal.why
        ));
    }
    if let Some(refused) = scan.refused.iter().find(|r| r.dir.as_path() == dir) {
        return Err(refused.why.clone());
    }
    let Some(found) = scan.found.iter().find(|d| d.dir.as_path() == dir) else {
        return Err(format!(
            "the package that was installed at {} is no longer there; confirm whether it \
             was uninstalled, or reinstalled somewhere else",
            dir.display()
        ));
    };
    let manifest = &found.manifest;
    // Directory match only proves "this is the package that occupied this
    // directory at startup" — not that its manifest still declares the name
    // being enabled. `name` is `os.json`'s configuration key: if the manifest
    // was renamed since boot, the NEXT boot's catalogue (built fresh from
    // whatever is on disk then) will never contain the old name at all, so an
    // `enable` for it must not be allowed to persist (design doc round 5
    // High 1 — an earlier version of this check dropped this comparison
    // entirely and reopened exactly the bug T8 exists to close).
    if manifest.name() != name {
        return Err(format!(
            "the manifest at {} now declares the name {:?}, not {name:?} — it may have been \
             renamed since this module was mounted",
            dir.display(),
            manifest.name()
        ));
    }
    // Mirrors `mount_package`'s own delivery-mode check (domain.rs) verbatim —
    // not a new judgment call, the same question `mount_package` would ask at
    // the next boot, just asked now instead of then.
    if manifest
        .spawn()
        .filter(|_| !manifest.is_mountable_in_process())
        .is_none()
    {
        return Err(
            "a package on disk must declare an out-of-process provider with a spawn command; \
             in-process modules are compiled in"
                .to_owned(),
        );
    }
    Ok(())
}

/// `Some(response)` blocks the `enable`; `None` lets the write proceed. Gets
/// [`rescan_disabled_package_on_disk`] off the async executor and turns its
/// verdict into a response.
async fn rescan_disabled_package(
    state: &AppState,
    name: &str,
    dir: &std::path::Path,
) -> Option<Response> {
    let packages_root = std::sync::Arc::clone(&state.packages_root);
    let name = name.to_owned();
    let dir = dir.to_owned();
    let outcome = tokio::task::spawn_blocking(move || {
        rescan_disabled_package_on_disk(&packages_root, &name, &dir)
    })
    .await;
    match outcome {
        Ok(Ok(())) => None,
        Ok(Err(message)) => Some(rescan_blocked_response(&message)),
        Err(e) => {
            // `JoinError`'s `Display` includes the panic payload when the
            // blocking task panicked — logged for whoever has to debug it,
            // never put in the response: a panic message can carry a path or
            // parsed file content across the API boundary otherwise.
            tracing::error!("on-demand admission re-check panicked or was cancelled: {e}");
            Some(error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "the on-demand admission re-check failed",
            ))
        }
    }
}

pub async fn patch_os(
    State(state): State<AppState>,
    Path(name): Path<String>,
    req: Request<Body>,
) -> Response {
    patch_os_at(
        State(state),
        Path(name),
        req,
        crate::os_config::config_path(),
    )
    .await
}

/// [`patch_os`], reading/writing the config at an injected path instead of the
/// process's real `$HOME` (T8/ME-3g) — the seam judgement criteria use to
/// assert on exactly what did or did not get written, without touching
/// `~/.agent24/os.json` on the machine running the test.
async fn patch_os_at(
    State(state): State<AppState>,
    Path(name): Path<String>,
    req: Request<Body>,
    path: Option<std::path::PathBuf>,
) -> Response {
    // Refuse a name this daemon does not provide, BEFORE writing anything. This is
    // the reason the daemon owns the file: ME-2a can only report a typo'd entry at
    // the next start, by which point the registry is already broken.
    if !state.os_reports.iter().any(|r| r.name == name) {
        let known: Vec<&str> = state.os_reports.iter().map(|r| r.name.as_str()).collect();
        return error_response(
            StatusCode::NOT_FOUND,
            "not_found",
            &format!("no domain OS named {name:?}; this daemon provides {known:?}"),
        );
    }
    let bytes = match read_body_or_response(req).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let update: DomainOsUpdate = match serde_json::from_slice(&bytes) {
        Ok(u) => u,
        Err(e) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                &format!("invalid body: {e}"),
            );
        }
    };
    // T8/ME-3g: admission gate, before touching `path`/`os_control` — no point
    // taking the write lock for a request this is about to refuse. Only ever
    // engages when this name has EXACTLY ONE `os_reports` entry: a duplicate
    // name (two catalogue entries claiming it) is left to today's existing
    // unconditional-allow behavior on purpose (design doc round 3 High 3 —
    // telling "the one that actually won this name" apart from "a loser
    // refused only because the name was already taken" needs more provenance
    // than this gate has, and is out of scope here).
    if update.enabled {
        let same_name: Vec<&MountReport> =
            state.os_reports.iter().filter(|r| r.name == name).collect();
        if let [only] = same_name.as_slice() {
            match &only.outcome {
                MountOutcome::Refused(why) => return admission_refused_response(why),
                MountOutcome::Disabled => {
                    if let Some(dir) = state.package_dirs.get(&name)
                        && let Some(blocked) = rescan_disabled_package(&state, &name, dir).await
                    {
                        return blocked;
                    }
                }
                MountOutcome::Mounted | MountOutcome::Degraded(_) => {}
            }
        }
    }
    let path = match path {
        Some(p) => p,
        None => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "HOME not set",
            );
        }
    };
    // A clone for the final `render_at` call below — `path` itself moves into
    // the spawned `apply` task next.
    let path_for_render = path.clone();
    // The write and the hand-off of a running module's stop are one step
    // against another toggle, done in a task of the daemon's own: a client
    // that goes away half-way cannot leave os.json disabling a module that
    // keeps serving. The lock is released before the wait for the module to
    // refuse requests, so a slow module does not hold up other toggles
    // (review of SUP-5, round 3).
    let control = state.os_control.clone().lock_owned().await;
    let step = tokio::spawn({
        let (state, name) = (state.clone(), name.clone());
        async move {
            let _control = control;
            apply(&state, path, &name, update.enabled).await
        }
    });
    let (handed, write_error) = match step.await {
        Ok(Ok(applied)) => applied,
        Ok(Err(why)) => {
            return error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal", &why);
        }
        Err(e) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                &format!("the config change failed: {e}"),
            );
        }
    };
    // A disable of a running out-of-process module applies now; anything else
    // at the next start.
    let hot = settled_and_reconciled(handed, ADMISSION_CLOSED_WITHIN).await;
    let effect = match hot {
        HotStop::Stopping => "stopping it now",
        HotStop::Pending => "its supervisor was asked to stop it; not yet refusing requests",
        HotStop::Already => "an earlier disable already stopped it or is stopping it",
        HotStop::Failed => "its supervisor could not stop it cleanly",
        HotStop::NotRunning => "takes effect on restart",
    };
    tracing::info!(
        "domain OS {name:?} set enabled={} in os.json ({effect})",
        update.enabled
    );
    // First, so a write error does not hide the code a client acts on
    // (review of SUP-5, round 5).
    if hot == HotStop::Failed {
        let also = write_error
            .as_deref()
            .map(|why| format!("; writing os.json also reported: {why}"))
            .unwrap_or_default();
        return patch_os_stop_failed_response(&name, &also);
    }
    if let Some(why) = write_error {
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            &format!(
                "this request set enabled={} for {name:?} in os.json ({effect}), but writing it \
                 reported: {why}",
                update.enabled
            ),
        );
    }
    // Not a success: a request sent after this answer could still be
    // admitted (review of SUP-5, rounds 2 and 3).
    if hot == HotStop::Pending {
        return patch_os_disable_pending_response(&name);
    }
    // Return the whole list so a client sees the new `restart_required` state
    // without a second round trip.
    render_at(&state, Some(path_for_render))
}

/// Hands a running module's stop off RIGHT NOW, without writing `os.json`
/// (FU-61 — `uninstall`'s hot-disable step must not reuse `patch_os`:
/// persisting `enabled:false` for a name about to vanish from discovery
/// entirely trips `unknown_disabled`'s fail-closed check on the daemon's
/// next start and takes the WHOLE registry down, not just this package).
/// Same "refuse an unknown name" guard `patch_os` has (this daemon's own
/// mount report, a snapshot from startup, still lists a package whose files
/// were just deleted — that is expected and fine, uninstall is exactly the
/// moment this route exists for), same `hand_off`/`settle`/`last_look`
/// machinery (`last_look` is not optional — a one-shot `settle()` alone
/// would miss a `Stopping` that becomes `StopFailed` moments later, or a
/// `Pending` that closes admission just after its own timeout, and answer
/// 2xx for a stop that did not actually land), same error codes for
/// `Failed`/`Pending` — the only thing genuinely new here is "skip the
/// config write." The success response does NOT reuse `render(&state)`:
/// `render` can itself return `503 registry_invalid` if `os.json` happens
/// to be unreadable at that instant, which would make a fully successful
/// stop look like a failure for a reason that has nothing to do with it.
pub async fn stop_now_os(State(state): State<AppState>, Path(name): Path<String>) -> Response {
    if !state.os_reports.iter().any(|r| r.name == name) {
        let known: Vec<&str> = state.os_reports.iter().map(|r| r.name.as_str()).collect();
        return error_response(
            StatusCode::NOT_FOUND,
            "not_found",
            &format!("no domain OS named {name:?}; this daemon provides {known:?}"),
        );
    }
    let handed = hand_off(
        state.supervisors.as_deref(),
        state.shutdown.modules_cut_off(),
        &name,
    );
    let hot = settled_and_reconciled(handed, ADMISSION_CLOSED_WITHIN).await;
    match hot {
        HotStop::Failed => stop_now_os_stop_failed_response(&name),
        HotStop::Pending => stop_now_os_disable_pending_response(&name),
        // Covers `Stopping`/`Already`/`NotRunning` alike — the body does not
        // (and must not) claim "it was running and I stopped it"; that
        // distinction belongs to `agent24 os list`, not to this ack.
        HotStop::Stopping | HotStop::Already | HotStop::NotRunning => {
            Json(serde_json::json!({ "name": name, "stopped": true })).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::domain::{MountOutcome, MountReport, ResourceStatus};

    thread_local! {
        /// Incremented on every `last_look` call — see its doc comment.
        /// Thread-local, not a shared global: `#[tokio::test]` (current-
        /// thread flavor, the default) runs each test's async body on the
        /// OS thread the test harness gave that test, so a `thread_local`
        /// counter cannot be perturbed by other tests running concurrently
        /// on other threads — a shared `static` would have been a flaky
        /// test in its own right.
        pub(super) static LAST_LOOK_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    /// FU-61 (round-2 code review Medium): a mutation deleting
    /// `settled_and_reconciled`'s `last_look` call would leave every
    /// existing `stop_now_os`/`patch_os` test green, because none of them
    /// depend on `last_look`'s CORRECTIVE effect being exercised (that
    /// effect needs a status change to land in a scheduler gap that, on a
    /// single-threaded runtime, does not exist — see the comment on
    /// `last_look` itself). This test instead pins that the call happens AT
    /// ALL, which is the one thing a mutation removing it would actually
    /// change.
    #[tokio::test]
    async fn settled_and_reconciled_calls_last_look() {
        let before = LAST_LOOK_CALLS.with(std::cell::Cell::get);
        let _ = settled_and_reconciled(None, std::time::Duration::ZERO).await;
        assert_eq!(
            LAST_LOOK_CALLS.with(std::cell::Cell::get),
            before + 1,
            "settled_and_reconciled must call last_look exactly once"
        );
    }

    /// What a disable reports: `Stopping` once the module refuses new
    /// requests; `Pending` if it still admitted them when the wait ran out —
    /// here at once, on this single-threaded runtime, before its supervisor
    /// has run at all — and again `Pending` for a retry while that is still
    /// so, `Already` once it refuses them; nothing to stop without
    /// supervisors (review of SUP-5, rounds 2 and 3).
    #[tokio::test]
    async fn a_disable_says_whether_the_module_refuses_requests_yet() {
        let never = std::future::pending::<()>;
        let tmp = tempfile::Builder::new()
            .prefix("a24")
            .tempdir_in("/tmp")
            .unwrap();
        let host = crate::domain::tests::running_package(tmp.path()).await;
        let zero = std::time::Duration::ZERO;
        assert_eq!(
            stop_now(None, never(), "remote", zero).await,
            HotStop::NotRunning
        );
        assert_eq!(
            stop_now(Some(&host.supervisors), never(), "nope", zero).await,
            HotStop::NotRunning
        );
        assert_eq!(
            stop_now(Some(&host.supervisors), never(), "remote", zero).await,
            HotStop::Pending
        );
        assert_eq!(
            stop_now(Some(&host.supervisors), never(), "remote", zero).await,
            HotStop::Pending,
            "a retry answered while the module still admits requests"
        );
        assert!(
            !applied(host.supervisors.disabled_slot("remote").as_ref()),
            "listed as disabled while it still admits requests"
        );
        assert_eq!(
            stop_now(
                Some(&host.supervisors),
                never(),
                "remote",
                std::time::Duration::from_secs(5)
            )
            .await,
            HotStop::Already
        );
        assert!(applied(host.supervisors.disabled_slot("remote").as_ref()));
        for stop in host.supervisors.close().disabling {
            stop.task.await.unwrap();
        }

        let tmp = tempfile::Builder::new()
            .prefix("a24")
            .tempdir_in("/tmp")
            .unwrap();
        let host = crate::domain::tests::running_package(tmp.path()).await;
        assert_eq!(
            stop_now(
                Some(&host.supervisors),
                never(),
                "remote",
                std::time::Duration::from_secs(5)
            )
            .await,
            HotStop::Stopping
        );
        for stop in host.supervisors.close().disabling {
            stop.task.await.unwrap();
        }
    }

    /// A failed stop is not a disable applied, whatever admission says — it
    /// revokes admission too (review of SUP-5, round 4).
    #[test]
    fn a_failed_stop_is_classified_before_admission() {
        use agent24_os_proto::supervisor::Status;
        let failed = Status::StopFailed { error: "x".into() };
        for asked in [true, false] {
            assert_eq!(classify(true, asked, &failed), HotStop::Failed);
            assert_eq!(classify(true, asked, &Status::Panicked), HotStop::Failed);
            assert_eq!(classify(true, asked, &Status::Killed), HotStop::Failed);
            assert_eq!(classify(false, asked, &Status::Running), HotStop::Pending);
        }
        assert_eq!(classify(true, true, &Status::Stopping), HotStop::Stopping);
        assert_eq!(classify(true, false, &Status::Stopped), HotStop::Already);
    }

    /// A stop that failed after `settle` looked is still reported failed
    /// (review of SUP-5, round 6).
    #[test]
    fn the_last_look_catches_a_failure_since_settle() {
        use agent24_os_proto::supervisor::Status;
        let (tx, status) = tokio::sync::watch::channel(Status::Stopping);
        let slot = crate::domain::Disabled {
            current: agent24_os_proto::drain::Current::new(
                agent24_os_proto::drain::Generation::starting(),
            ),
            status,
        };
        assert_eq!(last_look(HotStop::Stopping, Some(&slot)), HotStop::Stopping);
        // Starting is not Running: a pending disable whose module no longer
        // admits requests by the last look is under way.
        assert_eq!(last_look(HotStop::Pending, Some(&slot)), HotStop::Stopping);
        tx.send_replace(Status::StopFailed { error: "x".into() });
        assert_eq!(last_look(HotStop::Stopping, Some(&slot)), HotStop::Failed);
        assert_eq!(last_look(HotStop::NotRunning, None), HotStop::NotRunning);
    }

    /// A disable answers only once the module's generation refuses new work:
    /// here its supervisor gets to it late, and the wait outlasts that. And
    /// the wait is bounded, for a supervisor that never does (SUP-5).
    #[tokio::test]
    async fn a_disable_waits_until_the_generation_refuses_work() {
        use agent24_os_proto::drain::{Current, DrainState, Generation};
        let serving = || {
            let g = Generation::serving_at("127.0.0.1:1".parse().unwrap());
            assert!(g.ready());
            g
        };
        let g = serving();
        let current = Current::new(g.clone());
        let late = {
            let g = g.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                assert!(g.begin_drain(
                    std::time::Instant::now(),
                    std::time::Duration::from_secs(10)
                ));
            })
        };
        assert!(admission_closed(&current, std::time::Duration::from_secs(5)).await);
        assert_eq!(g.state(), DrainState::Draining);
        late.await.unwrap();

        let stuck = Current::new(serving());
        assert!(!admission_closed(&stuck, std::time::Duration::from_millis(50)).await);
    }

    /// A module stopped by `os disable` is reported `disabled` — `stopping`
    /// while it drains — and needs no restart: the running state already
    /// matches the config (SUP-5). Control: without the hot disable, the same
    /// config change needs a restart.
    #[test]
    fn a_hot_disabled_module_is_disabled_and_needs_no_restart() {
        use agent24_os_proto::supervisor::Status;
        let r = report("pkg", MountOutcome::Mounted);
        let v = view(&r, false, true, Some(&Status::Stopping), Some(true));
        assert_eq!(
            (v.state.as_str(), v.detail.as_deref()),
            ("disabled", Some("stopping"))
        );
        assert!(!v.restart_required);
        let v = view(&r, false, true, Some(&Status::Stopped), Some(true));
        assert_eq!((v.state.as_str(), v.detail.as_deref()), ("disabled", None));
        assert!(!v.restart_required);
        let v = view(&r, false, true, Some(&Status::Running), None);
        assert!(v.restart_required, "a pending disable not applied yet");
        // A stop that failed is not a disable applied.
        let failed = Status::StopFailed {
            error: "still there".into(),
        };
        let v = view(&r, false, true, Some(&failed), Some(true));
        assert_eq!(
            (v.state.as_str(), v.detail.as_deref()),
            ("degraded", Some("still there"))
        );
        assert!(v.restart_required);
        // ... and still after the config is switched back on: nothing but a
        // restart brings it back (review of SUP-5, round 5).
        assert!(view(&r, true, true, Some(&failed), Some(true)).restart_required);
        // A disable asked for, not yet applied: what it still is, and what
        // is coming — no restart needed for the disable, but one for an
        // enable landing before the stop takes hold, which cannot be taken
        // back (review of SUP-5, round 6).
        let v = view(&r, false, true, Some(&Status::Running), Some(false));
        assert_eq!(
            (v.state.as_str(), v.detail.as_deref()),
            ("mounted", Some("stop requested"))
        );
        let v = view(
            &r,
            false,
            true,
            Some(&Status::Starting {
                attempt: 1,
                after: None,
            }),
            Some(false),
        );
        assert_eq!(v.detail.as_deref(), Some("stop requested"));
        assert!(!v.restart_required);
        assert!(view(&r, true, true, Some(&Status::Running), Some(false)).restart_required);
        assert!(view(&r, true, true, Some(&Status::Stopped), Some(true)).restart_required);
        // A failed stop needs a restart even with the registry unusable.
        assert!(view(&r, false, false, Some(&failed), Some(true)).restart_required);
    }

    /// FU-61: `PackageChanged`'s detail names the reason AND the one
    /// recovery that actually works — restarting the daemon. It must NOT
    /// suggest `os disable`/`enable`: `enable` only changes config, it does
    /// not create a new supervisor in a running daemon, so that advice
    /// would leave the module stopped forever (design doc, "操作者可见文本").
    #[test]
    fn a_package_changed_module_is_told_to_restart_the_daemon_not_disable_enable() {
        use agent24_os_proto::supervisor::Status;
        let r = report("pkg", MountOutcome::Mounted);
        let changed = Status::PackageChanged {
            reason: "the package directory no longer exists".to_owned(),
        };
        let v = view(&r, true, true, Some(&changed), None);
        assert_eq!(v.state, "degraded");
        let detail = v.detail.unwrap();
        assert!(
            detail.contains("the package directory no longer exists"),
            "{detail}"
        );
        assert!(
            detail.contains("agent24 daemon stop && agent24 daemon start"),
            "{detail}"
        );
        assert!(
            !detail.contains("disable") && !detail.contains("enable"),
            "advice that cannot restart a stopped supervisor: {detail}"
        );
    }

    /// A package mounted at start is reported by its supervisor's status NOW:
    /// one that has given up is degraded, with the reason — not "mounted"
    /// (review of SUP-4, round 1). Control: a running one is mounted.
    #[test]
    fn a_mounted_package_is_reported_by_its_live_status() {
        use agent24_os_proto::supervisor::Status;
        let r = report("pkg", MountOutcome::Mounted);
        use agent24_os_proto::failure::{FailureKind, RunFailure};
        let last = RunFailure::new(FailureKind::Exited, "the module exited with code 3");
        let gave_up = Status::GaveUp {
            failures: 5,
            within: std::time::Duration::from_secs(4),
            last: last.clone(),
        };
        let v = view(&r, true, true, Some(&gave_up), None);
        assert_eq!(v.state, "degraded");
        // FU-57: how the last run failed is part of what is said.
        assert_eq!(
            v.detail.as_deref(),
            Some(
                "gave up after 5 failed runs within 4s — last: exited (the module exited with code 3)"
            )
        );
        let backoff = Status::Backoff {
            failures: 2,
            delay: std::time::Duration::from_millis(800),
            last: RunFailure::new(FailureKind::Setup, "could not start the module: x"),
        };
        assert_eq!(
            view(&r, true, true, Some(&backoff), None).detail.as_deref(),
            Some(
                "restarting in 800ms after 2 failed run(s) — last: setup (could not start the module: x)"
            )
        );
        let starting = Status::Starting {
            attempt: 3,
            after: Some(last),
        };
        assert_eq!(
            view(&r, true, true, Some(&starting), None)
                .detail
                .as_deref(),
            Some("starting (run 3) — after: exited (the module exited with code 3)")
        );
        let v = view(&r, true, true, Some(&Status::Running), None);
        assert_eq!(v.state, "mounted");
        assert_eq!(v.detail, None);
    }

    /// A report shaped the way the mounter actually produces one.
    ///
    /// Two invariants the earlier fixture broke, and a broken fixture hides
    /// rendering regressions rather than catching them:
    /// - only a MOUNTED module holds grants or has had its resources checked;
    /// - `enabled_at_start` follows the outcome only for `Disabled` (which implies
    ///   the registry said false). A `Refused` module can be configured either way,
    ///   so callers that care pass it explicitly via [`report_configured`].
    fn report(name: &str, outcome: MountOutcome) -> MountReport {
        let enabled_at_start = Some(!matches!(outcome, MountOutcome::Disabled));
        report_configured(name, outcome, enabled_at_start)
    }

    fn report_configured(
        name: &str,
        outcome: MountOutcome,
        enabled_at_start: Option<bool>,
    ) -> MountReport {
        let live = matches!(outcome, MountOutcome::Mounted);
        MountReport {
            name: name.to_owned(),
            namespace: format!("/api/v1/{name}"),
            outcome,
            version: "0.2.1".to_owned(),
            enabled_at_start,
            granted: if live {
                vec!["events".to_owned()]
            } else {
                Vec::new()
            },
            resources: if live {
                ResourceStatus::Satisfied
            } else {
                ResourceStatus::NotChecked
            },
        }
    }

    #[test]
    fn a_pending_toggle_is_reported_as_needing_a_restart() {
        // Routes are built once at startup, so a toggle cannot take effect until
        // the next one. Reporting `enabled` while every request 503s would leave a
        // user staring at a contradiction; `restart_required` is what turns that
        // into a fact they can act on.
        let running = report("sin90", MountOutcome::Mounted);
        let v = view(&running, false, true, None, None);
        assert_eq!(v.state, "mounted", "it is still serving right now");
        assert!(!v.enabled, "but the config now says off");
        assert!(v.restart_required);

        // And the other direction.
        let off = report("sin90", MountOutcome::Disabled);
        let v = view(&off, true, true, None, None);
        assert_eq!(v.state, "disabled");
        assert!(v.enabled);
        assert!(v.restart_required);
    }

    #[test]
    fn the_view_detects_a_semantically_invalid_registry_itself() {
        // The previous version only fed `registry_usable: false` into `view` by
        // hand, so deleting the detection in `render` would have left it green.
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("os.json");
        std::fs::write(&p, r#"{"domainOs": {"sin09": {"enabled": false}}}"#).unwrap();
        let cfg = crate::os_config::OsConfig::load(&p).unwrap();

        let err = semantic_registry_error(&cfg, ["sin90"].into_iter())
            .expect("an entry disabling a module nothing provides must be reported");
        assert!(err.contains("sin09"), "{err}");
        assert!(
            err.contains("Restarting will not help"),
            "the remediation must say what actually helps: {err}"
        );

        // And a file that names only real modules is clean.
        std::fs::write(&p, r#"{"domainOs": {"sin90": {"enabled": false}}}"#).unwrap();
        let cfg = crate::os_config::OsConfig::load(&p).unwrap();
        assert!(semantic_registry_error(&cfg, ["sin90"].into_iter()).is_none());
    }

    #[test]
    fn a_still_broken_registry_never_asks_for_a_restart() {
        // Restarting into the same bad file produces the same degradation, so
        // "restart to apply" is advice that cannot work. Before this, a
        // semantically-invalid registry reported `enabled_at_start: None` at
        // startup and then, because the file still PARSES, the view read `None` as
        // "unapplied config" and asked for a restart forever.
        let mut r = report("sin90", MountOutcome::Degraded("os.json ...".into()));
        r.enabled_at_start = None;
        assert!(
            !view(&r, true, false, None, None).restart_required,
            "the registry is still unusable; the fix is the file, not a restart"
        );
        // Once it IS usable, the config has genuinely never been applied.
        assert!(view(&r, true, true, None, None).restart_required);
    }

    #[test]
    fn a_settled_module_does_not_ask_for_a_restart() {
        assert!(
            !view(&report("a", MountOutcome::Mounted), true, true, None, None).restart_required
        );
        assert!(
            !view(
                &report("a", MountOutcome::Disabled),
                false,
                true,
                None,
                None
            )
            .restart_required
        );
    }

    #[test]
    fn a_refused_module_never_asks_for_a_restart() {
        // A restart would change nothing — the manifest is inadmissible however the
        // config is set. Telling the user to restart would send them to do
        // something that cannot help.
        let r = report(
            "health",
            MountOutcome::Refused("kernel route segment".into()),
        );
        let v = view(&r, true, true, None, None);
        assert_eq!(v.state, "refused");
        assert!(v.enabled, "the config wants it");
        assert!(
            !v.restart_required,
            "yet a restart cannot deliver it, so do not ask for one"
        );
        assert_eq!(v.detail.as_deref(), Some("kernel route segment"));
    }

    #[test]
    fn an_enabled_but_degraded_module_reports_no_pending_config_change() {
        // Enabled at startup, still enabled, and it failed to come up. The CONFIG
        // has not changed, so there is nothing pending to apply — telling the user
        // to restart would imply their setting had not taken effect, when the truth
        // is in `detail`. (An earlier version compared config against "is it
        // RUNNING?", which reported a pending change here — and its NAME said the
        // opposite of what it asserted, which is how the confusion survived.)
        let r = report(
            "sin90",
            MountOutcome::Degraded("store failed to open".into()),
        );
        let v = view(&r, true, true, None, None);
        assert!(!v.restart_required);
        assert_eq!(
            v.detail.as_deref(),
            Some("store failed to open"),
            "the actionable fact is WHAT failed, not 'restart'"
        );

        // And the case the availability comparison MISSED entirely: disabling an
        // already-degraded module IS a real pending change.
        assert!(view(&r, false, true, None, None).restart_required);
    }

    #[tokio::test]
    async fn an_unknown_name_is_refused_and_the_known_ones_are_named() {
        // The reason the daemon owns `os.json` at all. `agent24 os disable sin09`
        // must fail HERE, naming the modules that exist — not write a file that
        // takes the whole registry down at the next start.
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        let mut st = crate::server::tests::state().await;
        st.os_reports = std::sync::Arc::new(vec![report("sin90", MountOutcome::Mounted)]);
        let token = st.token.to_string();
        let router = crate::server::build_router_with_modules(st, axum::Router::new());

        let res = router
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/v1/os/sin09")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"enabled": false}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::NOT_FOUND);
        let bytes = axum::body::to_bytes(res.into_body(), 64 * 1024)
            .await
            .unwrap();
        let j: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let msg = j["error"]["message"].as_str().unwrap();
        assert!(msg.contains("sin09"), "{msg}");
        assert!(
            msg.contains("sin90"),
            "it must name what DOES exist, or the user is left guessing: {msg}"
        );
    }

    #[test]
    fn a_construction_failure_renders_as_a_named_degraded_module() {
        // RENDERING only. That the mounter actually produces such a report is
        // covered by `domain::tests::a_module_that_fails_to_construct_still_has_a_
        // name_and_a_namespace`; this pins how it reaches the wire.
        let r = MountReport {
            name: "sin90".to_owned(),
            namespace: "/api/v1/sin90".to_owned(),
            version: "0.2.1".to_owned(),
            enabled_at_start: Some(true),
            outcome: MountOutcome::Degraded("could not be constructed: manifest invalid".into()),
            granted: Vec::new(),
            resources: ResourceStatus::NotChecked,
        };
        let v = view(&r, true, true, None, None);
        assert_eq!(v.name, "sin90");
        assert_eq!(v.version, "0.2.1");
        assert_eq!(v.state, "degraded");
        assert!(v.detail.unwrap().contains("could not be constructed"));
        assert_eq!(
            v.namespace, "/api/v1/sin90",
            "and it still owns a namespace"
        );
    }

    #[test]
    fn a_never_constructed_module_renders_as_disabled_with_its_identity() {
        // Rendering only, as above — the mounter side is
        // `domain::tests::a_disabled_module_is_never_constructed`.
        let r = MountReport {
            name: "cos72".to_owned(),
            namespace: "/api/v1/cos72".to_owned(),
            version: "0.1.0".to_owned(),
            enabled_at_start: Some(false),
            outcome: MountOutcome::Disabled,
            granted: Vec::new(),
            resources: ResourceStatus::NotChecked,
        };
        let v = view(&r, false, true, None, None);
        assert_eq!(v.state, "disabled");
        assert!(!v.enabled);
        assert!(!v.restart_required, "config and runtime agree");

        // And once the user enables it, the list says a restart is what applies it.
        assert!(view(&r, true, true, None, None).restart_required);
    }

    #[tokio::test]
    async fn the_registry_endpoint_is_behind_kernel_auth() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        let st = crate::server::tests::state().await;
        let router = crate::server::build_router_with_modules(st, axum::Router::new());
        for (method, uri) in [
            ("GET", "/api/v1/os"),
            ("PATCH", "/api/v1/os/sin90"),
            ("POST", "/api/v1/os/sin90/stop"),
        ] {
            let res = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(uri)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                res.status(),
                axum::http::StatusCode::UNAUTHORIZED,
                "{method} {uri}"
            );
        }
    }

    #[test]
    fn resource_status_is_flattened_without_losing_which_case_it_was() {
        let mut r = report("m", MountOutcome::Mounted);
        r.resources = ResourceStatus::MissingModels(vec!["ornith-9b".into()]);
        let v = view(&r, true, true, None, None);
        assert_eq!(v.resources, "missing");
        assert_eq!(v.missing_models, vec!["ornith-9b".to_owned()]);

        r.resources = ResourceStatus::Unknown("provider down".into());
        let v = view(&r, true, true, None, None);
        assert_eq!(v.resources, "unknown");
        assert!(
            v.missing_models.is_empty(),
            "an unchecked model is not a missing one"
        );

        r.resources = ResourceStatus::NotChecked;
        assert_eq!(view(&r, true, true, None, None).resources, "not_checked");
    }

    // FU-61: `stop_now_os` — `uninstall`'s hot-disable step, distinct from
    // `patch_os` in that it must never write `os.json`.

    #[tokio::test]
    async fn stop_now_os_refuses_an_unknown_name_naming_the_known_ones() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        let mut st = crate::server::tests::state().await;
        st.os_reports = std::sync::Arc::new(vec![report("sin90", MountOutcome::Mounted)]);
        let token = st.token.to_string();
        let router = crate::server::build_router_with_modules(st, axum::Router::new());

        let res = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/os/sin09/stop")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::NOT_FOUND);
        let bytes = axum::body::to_bytes(res.into_body(), 64 * 1024)
            .await
            .unwrap();
        let j: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(j["error"]["code"], "not_found");
        let msg = j["error"]["message"].as_str().unwrap();
        assert!(msg.contains("sin09"), "{msg}");
        assert!(msg.contains("sin90"), "{msg}");
    }

    #[tokio::test]
    async fn stop_now_os_stops_a_running_module_with_a_dedicated_response() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        let tmp = tempfile::Builder::new()
            .prefix("a24")
            .tempdir_in("/tmp")
            .unwrap();
        let st = crate::domain::tests::running_state(tmp.path()).await;
        let token = st.token.to_string();
        let router = crate::server::build_router_with_modules(st, axum::Router::new());

        // FU-61 (round-3 code review Medium): `settled_and_reconciled_calls_
        // last_look` above proves the HELPER calls `last_look` — it does not
        // prove `stop_now_os` calls the HELPER rather than bypassing it
        // (e.g. reverting to a bare `settle(...).await`). Asserting the
        // counter delta around a real `POST .../stop` closes that gap: it
        // is the same runtime thread throughout (`#[tokio::test]`'s default
        // current-thread flavor, matching `LAST_LOOK_CALLS`'s thread-local
        // isolation), so the delta is exactly this call's contribution.
        let before = LAST_LOOK_CALLS.with(std::cell::Cell::get);
        let res = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/os/remote/stop")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            LAST_LOOK_CALLS.with(std::cell::Cell::get),
            before + 1,
            "stop_now_os must reach last_look through settled_and_reconciled"
        );
        assert!(res.status().is_success(), "{}", res.status());
        let bytes = axum::body::to_bytes(res.into_body(), 64 * 1024)
            .await
            .unwrap();
        let j: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        // The dedicated ack shape, NOT `render`'s `DomainOsList` — a
        // regression to `render(&state)` on success would fail this (no
        // `"stopped"` field, and a `DomainOsList` has a `"modules"` array
        // instead), pinning that this route's success path cannot be
        // coupled to `render`'s own failure mode (a concurrently unreadable
        // `os.json` making an actually-successful stop look failed).
        assert_eq!(j["name"], "remote");
        assert_eq!(j["stopped"], true);
        assert!(j.get("modules").is_none(), "{j}");
    }

    #[tokio::test]
    async fn stop_now_os_reports_not_running_without_claiming_a_live_stop() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        // A compiled-in-shaped report with no supervisor at all behind it —
        // `HotStop::NotRunning`. The 2xx wording must not differ from the
        // "was actually running" case (`Stopping`/`Already`): the body alone
        // cannot tell them apart, so the text must not pretend it can.
        let mut st = crate::server::tests::state().await;
        st.os_reports = std::sync::Arc::new(vec![report("sin90", MountOutcome::Mounted)]);
        let token = st.token.to_string();
        let router = crate::server::build_router_with_modules(st, axum::Router::new());

        let res = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/os/sin90/stop")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(res.status().is_success(), "{}", res.status());
        let bytes = axum::body::to_bytes(res.into_body(), 64 * 1024)
            .await
            .unwrap();
        let j: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(j["stopped"], true);
    }

    /// Judgement criterion 11, the two control-plane sites: `patch_os` and
    /// `stop_now_os` share one `stop_failed` hint and one `disable_pending`
    /// hint (code review round 1 Low 2c) rather than each carrying its own
    /// copy that could drift. Standing up a real failing supervisor to drive
    /// these two HTTP handlers end-to-end to their `Failed`/`Pending`
    /// branches is disproportionate for asserting a static string built by
    /// `format!`/a `const` — this pins the shared source directly, which is
    /// what both handlers actually call.
    #[test]
    fn control_plane_stop_failed_and_disable_pending_hints_are_shared_and_correct() {
        let hint = control_plane_stop_failed_hint();
        assert!(
            hint.contains(RESTART_DAEMON_INSTRUCTION),
            "hint does not contain the shared instruction verbatim: {hint}"
        );
        assert!(CONTROL_PLANE_DISABLE_PENDING_HINT.contains("agent24 os list"));
        assert!(!CONTROL_PLANE_DISABLE_PENDING_HINT.is_empty());
    }

    async fn body_json(r: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(r.into_body(), 64 * 1024)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// Code review round 2 Low 3c: the test above only pins the shared
    /// `hint` string in isolation. This exercises the actual response
    /// constructors both `patch_os` and `stop_now_os` call — the real
    /// serialized `message`/`hint`/`code`, without standing up a full
    /// failing-supervisor HTTP fixture (that call was already made in round
    /// 1 and stands; this is a narrower, cheap addition on top of it).
    #[tokio::test]
    async fn all_four_control_plane_error_responses_serialize_correctly() {
        let j = body_json(patch_os_stop_failed_response("sin09", "")).await;
        assert_eq!(j["error"]["code"], "stop_failed");
        let msg = j["error"]["message"].as_str().unwrap();
        assert!(msg.contains("\"sin09\""), "{msg}");
        assert!(msg.contains("os.json"), "{msg}"); // true here: a PATCH really did write it
        assert!(
            j["error"]["hint"]
                .as_str()
                .unwrap()
                .contains(RESTART_DAEMON_INSTRUCTION)
        );

        let j = body_json(patch_os_stop_failed_response(
            "sin09",
            "; writing os.json also reported: disk full",
        ))
        .await;
        assert!(
            j["error"]["message"]
                .as_str()
                .unwrap()
                .contains("disk full")
        );

        let j = body_json(patch_os_disable_pending_response("sin09")).await;
        assert_eq!(j["error"]["code"], "disable_pending");
        assert!(
            j["error"]["message"]
                .as_str()
                .unwrap()
                .contains("\"sin09\"")
        );
        assert_eq!(
            j["error"]["hint"].as_str().unwrap(),
            CONTROL_PLANE_DISABLE_PENDING_HINT
        );

        let j = body_json(stop_now_os_stop_failed_response("sin09")).await;
        assert_eq!(j["error"]["code"], "stop_failed");
        let msg = j["error"]["message"].as_str().unwrap();
        assert!(msg.contains("\"sin09\""), "{msg}");
        // Unlike `patch_os`, `stop_now_os` never wrote `os.json` — its
        // message must not claim it did.
        assert!(!msg.contains("os.json"), "{msg}");
        assert!(
            j["error"]["hint"]
                .as_str()
                .unwrap()
                .contains(RESTART_DAEMON_INSTRUCTION)
        );

        let j = body_json(stop_now_os_disable_pending_response("sin09")).await;
        assert_eq!(j["error"]["code"], "disable_pending");
        assert!(
            j["error"]["message"]
                .as_str()
                .unwrap()
                .contains("\"sin09\"")
        );
        assert_eq!(
            j["error"]["hint"].as_str().unwrap(),
            CONTROL_PLANE_DISABLE_PENDING_HINT
        );
    }

    // ── T8/ME-3g: 判据 1-16 ──────────────────────────────────────────────

    /// A minimal, always-parseable manifest — the same shape
    /// `agent24-os-packages`/`agent24-domain`'s own fixtures use.
    /// `out_of_process_provider` needs a `spawn` command; `in_process_crate`
    /// must not have one (both enforced at parse time).
    fn t8_manifest_yaml(name: &str, version: &str, impl_kind: &str) -> String {
        let spawn = if impl_kind == "out_of_process_provider" {
            format!("spawn:\n  command: bin/{name}\n")
        } else {
            String::new()
        };
        format!(
            "name: {name}\nversion: {version:?}\nroute_namespace: /api/v1/{name}\n\
             event_module: {name}\ndata_dir: ~/.agent24/os/{name}/\n\
             impl_kind: {impl_kind}\n{spawn}"
        )
    }

    /// Installs a package directory under `root` with the given manifest body
    /// (not necessarily valid — some judgement criteria need an unparseable
    /// one). Returns the package's own directory.
    fn t8_install(root: &std::path::Path, name: &str, manifest_body: &str) -> std::path::PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(agent24_os_packages::discovery::MANIFEST_FILE),
            manifest_body,
        )
        .unwrap();
        dir
    }

    async fn t8_patch(
        state: &AppState,
        name: &str,
        enabled: bool,
        path: Option<std::path::PathBuf>,
    ) -> Response {
        let req = Request::builder()
            .method("PATCH")
            .uri(format!("/api/v1/os/{name}"))
            .header("content-type", "application/json")
            .body(Body::from(format!(r#"{{"enabled": {enabled}}}"#)))
            .unwrap();
        patch_os_at(State(state.clone()), Path(name.to_owned()), req, path).await
    }

    async fn t8_patch_raw_body(
        state: &AppState,
        name: &str,
        body: &str,
        path: Option<std::path::PathBuf>,
    ) -> Response {
        let req = Request::builder()
            .method("PATCH")
            .uri(format!("/api/v1/os/{name}"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_owned()))
            .unwrap();
        patch_os_at(State(state.clone()), Path(name.to_owned()), req, path).await
    }

    /// A base state plus a fresh, writable temp `os.json` path — every T8
    /// criterion starts here and then layers on `os_reports`/`package_dirs`/
    /// `packages_root` as needed.
    async fn t8_state() -> (AppState, tempfile::TempDir, std::path::PathBuf) {
        let st = crate::server::tests::state().await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("os.json");
        (st, dir, path)
    }

    /// Judgement criterion 1 — a `Refused` module's `enable` is blocked and
    /// `os.json` is left byte-for-byte as it was (not just "the requested
    /// name is absent" — code review round 1 Low 5: the finalized criterion
    /// is about the file as a whole, including entries that have nothing to
    /// do with this request).
    #[tokio::test]
    async fn criterion1_refused_blocks_and_does_not_persist() {
        let (mut st, _dir, path) = t8_state().await;
        st.os_reports = std::sync::Arc::new(vec![
            report(
                "refused-mod",
                MountOutcome::Refused("catalogue version mismatch".to_owned()),
            ),
            report("unrelated-mod", MountOutcome::Mounted),
        ]);
        // Seed a real, non-empty os.json with an entry unrelated to this
        // request, the way a daemon that has been running for a while would
        // actually have one.
        crate::os_config::OsConfig::set_enabled(&path, "unrelated-mod", true).unwrap();
        let before = std::fs::read(&path).unwrap();

        let res = t8_patch(&st, "refused-mod", true, Some(path.clone())).await;
        assert_eq!(res.status(), StatusCode::CONFLICT);
        let j = body_json(res).await;
        assert_eq!(j["error"]["code"], "admission_refused");
        assert_eq!(j["error"]["message"], "catalogue version mismatch");
        let hint = j["error"]["hint"].as_str().unwrap();
        assert!(hint.contains(RESTART_DAEMON_INSTRUCTION), "{hint}");
        let after = std::fs::read(&path).unwrap();
        assert_eq!(
            before, after,
            "a blocked enable must not touch os.json at all, not even to leave it \
             unchanged content-wise via a rewrite"
        );
    }

    /// Judgement criterion 2 — `Degraded` is not blocked by the "exactly one
    /// report, `Refused`" gate. Positive control: broadening the match arm to
    /// also catch `Degraded` would turn this red.
    #[tokio::test]
    async fn criterion2_degraded_is_not_blocked() {
        let (mut st, _dir, path) = t8_state().await;
        st.os_reports = std::sync::Arc::new(vec![report(
            "degraded-mod",
            MountOutcome::Degraded("store open failed: disk full".to_owned()),
        )]);
        let res = t8_patch(&st, "degraded-mod", true, Some(path.clone())).await;
        assert_eq!(res.status(), StatusCode::OK, "{:?}", body_json(res).await);
        let cfg = crate::os_config::OsConfig::load(&path).unwrap();
        assert!(cfg.is_enabled("degraded-mod"));
    }

    /// Judgement criterion 3 — `enabled: false` on a `Refused` module is
    /// unaffected by the gate (it only checks `update.enabled`).
    #[tokio::test]
    async fn criterion3_disable_of_refused_is_never_blocked() {
        let (mut st, _dir, path) = t8_state().await;
        st.os_reports = std::sync::Arc::new(vec![report(
            "refused-mod",
            MountOutcome::Refused("bad manifest".to_owned()),
        )]);
        let res = t8_patch(&st, "refused-mod", false, Some(path.clone())).await;
        assert_eq!(res.status(), StatusCode::OK, "{:?}", body_json(res).await);
        let cfg = crate::os_config::OsConfig::load(&path).unwrap();
        assert!(!cfg.is_enabled("refused-mod"));
    }

    /// Judgement criterion 4 — the blocked `message` is exactly what `os
    /// list`'s `detail` would show for the same report (both read the same
    /// `why`, so they cannot drift apart).
    #[tokio::test]
    async fn criterion4_message_matches_os_list_detail() {
        let (mut st, _dir, path) = t8_state().await;
        st.os_reports = std::sync::Arc::new(vec![report(
            "refused-mod",
            MountOutcome::Refused("the catalogue lists v2 but the manifest says v1".to_owned()),
        )]);
        let blocked = body_json(t8_patch(&st, "refused-mod", true, Some(path.clone())).await).await;
        let list = body_json(render_at(&st, Some(path))).await;
        let module = list["modules"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["name"] == "refused-mod")
            .unwrap();
        assert_eq!(blocked["error"]["message"], module["detail"]);
    }

    /// Judgement criterion 5 — an unknown name is still the pre-existing 404,
    /// never a 409 (the gate only runs after that check).
    #[tokio::test]
    async fn criterion5_unknown_name_is_404_not_409() {
        let (st, _dir, path) = t8_state().await;
        let res = t8_patch(&st, "nope", true, Some(path)).await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        assert_eq!(body_json(res).await["error"]["code"], "not_found");
    }

    /// Judgement criterion 6 — a duplicate name with one `Mounted` and one
    /// `Refused` report is left alone (today's unconditional-allow). Positive
    /// control: matching on "the first same-name report" instead of "exactly
    /// one" reintroduces the v1/v2 false positive this design closed.
    #[tokio::test]
    async fn criterion6_duplicate_mounted_and_refused_is_not_blocked() {
        let (mut st, _dir, path) = t8_state().await;
        st.os_reports = std::sync::Arc::new(vec![
            report(
                "dup",
                MountOutcome::Refused("empty catalogue version".to_owned()),
            ),
            report("dup", MountOutcome::Mounted),
        ]);
        let res = t8_patch(&st, "dup", true, Some(path.clone())).await;
        assert_eq!(res.status(), StatusCode::OK, "{:?}", body_json(res).await);
        let cfg = crate::os_config::OsConfig::load(&path).unwrap();
        assert!(cfg.is_enabled("dup"));
    }

    /// Judgement criterion 7 — a duplicate name where one report is
    /// `Disabled` (the real claimant) and the other is a collision `Refused`
    /// is ALSO left alone — the "exactly one report" precondition skips both
    /// gates entirely rather than trying to guess which report is the real
    /// one (design doc round 3 High 3).
    #[tokio::test]
    async fn criterion7_duplicate_disabled_and_refused_is_not_blocked() {
        let (mut st, _dir, path) = t8_state().await;
        st.os_reports = std::sync::Arc::new(vec![
            report("dup", MountOutcome::Disabled),
            report(
                "dup",
                MountOutcome::Refused("another module already claims the name \"dup\"".to_owned()),
            ),
        ]);
        // Even if "dup" were (wrongly) treated as package-backed, the
        // multi-report precondition must skip the rescan branch too.
        st.package_dirs = std::sync::Arc::new(std::collections::HashMap::from([(
            "dup".to_owned(),
            std::path::PathBuf::from("/nonexistent"),
        )]));
        let res = t8_patch(&st, "dup", true, Some(path.clone())).await;
        assert_eq!(res.status(), StatusCode::OK, "{:?}", body_json(res).await);
        let cfg = crate::os_config::OsConfig::load(&path).unwrap();
        assert!(cfg.is_enabled("dup"));
    }

    /// Judgement criterion 8 — the admission gate does not reorder the
    /// existing "unknown name -> 404" / "malformed body -> 400" checks.
    #[tokio::test]
    async fn criterion8_gate_does_not_disturb_existing_check_order() {
        let (st, _dir, path) = t8_state().await;
        // Unknown name + malformed body: still 404, the body is never reached.
        let res = t8_patch_raw_body(&st, "nope", "not json", Some(path.clone())).await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);

        // Known-but-Refused name + malformed body: still 400 (body parsing
        // happens before the gate), not 409.
        let (mut st2, _dir2, path2) = t8_state().await;
        st2.os_reports = std::sync::Arc::new(vec![report(
            "refused-mod",
            MountOutcome::Refused("bad".to_owned()),
        )]);
        let res = t8_patch_raw_body(&st2, "refused-mod", "not json", Some(path2)).await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(res).await["error"]["code"], "invalid_request");
    }

    /// Judgement criterion 9 — the primary case: a `Disabled` package whose
    /// on-disk manifest still declares the requested name but now says
    /// `in_process_crate` (delivery-mode self-contradiction for something
    /// installed as a package) is blocked. The manifest's `version` is
    /// deliberately different from anything remembered — that comparison was
    /// removed (design doc round 3 High 1) and must not resurface.
    #[tokio::test]
    async fn criterion9_disabled_package_delivery_mismatch_is_blocked() {
        let (mut st, _dir, path) = t8_state().await;
        let root = tempfile::tempdir().unwrap();
        let pkg_dir = t8_install(
            root.path(),
            "pkgmod",
            &t8_manifest_yaml("pkgmod", "9.9.9", "in_process_crate"),
        );
        st.os_reports = std::sync::Arc::new(vec![report("pkgmod", MountOutcome::Disabled)]);
        st.packages_root = std::sync::Arc::new(root.path().to_path_buf());
        st.package_dirs = std::sync::Arc::new(std::collections::HashMap::from([(
            "pkgmod".to_owned(),
            pkg_dir,
        )]));

        let res = t8_patch(&st, "pkgmod", true, Some(path.clone())).await;
        assert_eq!(res.status(), StatusCode::CONFLICT);
        let j = body_json(res).await;
        assert_eq!(j["error"]["code"], "admission_refused");
        assert!(
            j["error"]["message"]
                .as_str()
                .unwrap()
                .contains("out-of-process provider with a spawn command")
        );
        assert!(
            !path.exists(),
            "a blocked enable must not create os.json at all"
        );
    }

    /// Judgement criterion 9b — same directory, manifest renamed, delivery
    /// mode still valid: must still be blocked. Pins the exact bypass round 5
    /// caught (directory match alone proves "same package", not "same name").
    #[tokio::test]
    async fn criterion9b_disabled_package_renamed_manifest_is_blocked() {
        let (mut st, _dir, path) = t8_state().await;
        let root = tempfile::tempdir().unwrap();
        let pkg_dir = t8_install(
            root.path(),
            "alpha",
            &t8_manifest_yaml("beta", "1.0.0", "out_of_process_provider"),
        );
        st.os_reports = std::sync::Arc::new(vec![report("alpha", MountOutcome::Disabled)]);
        st.packages_root = std::sync::Arc::new(root.path().to_path_buf());
        st.package_dirs = std::sync::Arc::new(std::collections::HashMap::from([(
            "alpha".to_owned(),
            pkg_dir,
        )]));

        let res = t8_patch(&st, "alpha", true, Some(path.clone())).await;
        assert_eq!(res.status(), StatusCode::CONFLICT);
        let msg = body_json(res).await["error"]["message"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(msg.contains("\"beta\""), "{msg}");
        assert!(msg.contains("\"alpha\""), "{msg}");
        assert!(!path.exists());
    }

    /// Judgement criterion 10 — the common case: a `Disabled` package whose
    /// manifest is completely fine is allowed through and persists.
    #[tokio::test]
    async fn criterion10_disabled_package_clean_manifest_is_allowed() {
        let (mut st, _dir, path) = t8_state().await;
        let root = tempfile::tempdir().unwrap();
        let pkg_dir = t8_install(
            root.path(),
            "pkgmod",
            &t8_manifest_yaml("pkgmod", "1.0.0", "out_of_process_provider"),
        );
        st.os_reports = std::sync::Arc::new(vec![report("pkgmod", MountOutcome::Disabled)]);
        st.packages_root = std::sync::Arc::new(root.path().to_path_buf());
        st.package_dirs = std::sync::Arc::new(std::collections::HashMap::from([(
            "pkgmod".to_owned(),
            pkg_dir,
        )]));

        let res = t8_patch(&st, "pkgmod", true, Some(path.clone())).await;
        assert_eq!(res.status(), StatusCode::OK, "{:?}", body_json(res).await);
        let cfg = crate::os_config::OsConfig::load(&path).unwrap();
        assert!(cfg.is_enabled("pkgmod"));
    }

    /// Judgement criterion 11 — a `Disabled` compiled-in module (absent from
    /// `package_dirs`) never triggers the rescan at all, even with no
    /// `packages_root` set up to look at.
    #[tokio::test]
    async fn criterion11_disabled_compiled_in_is_allowed_without_rescan() {
        let (mut st, _dir, path) = t8_state().await;
        st.os_reports = std::sync::Arc::new(vec![report("builtin", MountOutcome::Disabled)]);
        // `package_dirs` stays empty — "builtin" is not in it.
        let res = t8_patch(&st, "builtin", true, Some(path.clone())).await;
        assert_eq!(res.status(), StatusCode::OK, "{:?}", body_json(res).await);
        let cfg = crate::os_config::OsConfig::load(&path).unwrap();
        assert!(cfg.is_enabled("builtin"));
    }

    /// Judgement criterion 12 — the rescan's blocked `hint` is the dedicated
    /// one, not `admission_refused_response`'s shared "check `os list`" hint
    /// (which would be wrong here — the report still just says `Disabled`).
    #[tokio::test]
    async fn criterion12_rescan_hint_is_dedicated_not_shared() {
        let (mut st, _dir, path) = t8_state().await;
        let root = tempfile::tempdir().unwrap();
        let pkg_dir = t8_install(
            root.path(),
            "pkgmod",
            &t8_manifest_yaml("pkgmod", "1.0.0", "in_process_crate"),
        );
        st.os_reports = std::sync::Arc::new(vec![report("pkgmod", MountOutcome::Disabled)]);
        st.packages_root = std::sync::Arc::new(root.path().to_path_buf());
        st.package_dirs = std::sync::Arc::new(std::collections::HashMap::from([(
            "pkgmod".to_owned(),
            pkg_dir,
        )]));

        let hint =
            body_json(t8_patch(&st, "pkgmod", true, Some(path)).await).await["error"]["hint"]
                .as_str()
                .unwrap()
                .to_owned();
        assert!(hint.contains("not in `agent24 os list`"), "{hint}");
        assert!(hint.contains("retry enable"), "{hint}");
        // The other gate's shared hint reads differently — spot check they are
        // not literally the same string (that gate's own tests, e.g.
        // criterion1, pin its exact wording; this just confirms the two do
        // not accidentally collapse into one).
        let shared_hint = body_json(admission_refused_response("x")).await["error"]["hint"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_ne!(hint, shared_hint);
    }

    /// Judgement criterion 13 — the package's own subdirectory is gone
    /// (uninstalled), but `packages_root` itself is fine.
    #[tokio::test]
    async fn criterion13_package_subdirectory_vanished_is_blocked() {
        let (mut st, _dir, path) = t8_state().await;
        let root = tempfile::tempdir().unwrap();
        // Never actually created — this package's directory does not exist.
        let pkg_dir = root.path().join("gone");
        st.os_reports = std::sync::Arc::new(vec![report("gone", MountOutcome::Disabled)]);
        st.packages_root = std::sync::Arc::new(root.path().to_path_buf());
        st.package_dirs = std::sync::Arc::new(std::collections::HashMap::from([(
            "gone".to_owned(),
            pkg_dir,
        )]));

        let res = t8_patch(&st, "gone", true, Some(path.clone())).await;
        assert_eq!(res.status(), StatusCode::CONFLICT);
        let msg = body_json(res).await["error"]["message"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(msg.contains("no longer there"), "{msg}");
        assert!(!path.exists());
    }

    /// Judgement criterion 14 — the package's directory is still there, but
    /// its manifest fails to parse. Matched by directory (not name — a
    /// failed parse may not yield a trustworthy name at all).
    #[tokio::test]
    async fn criterion14_unparseable_manifest_is_blocked_via_scan_refused() {
        let (mut st, _dir, path) = t8_state().await;
        let root = tempfile::tempdir().unwrap();
        let pkg_dir = t8_install(root.path(), "broken", "this is not: [valid yaml at all\n");
        st.os_reports = std::sync::Arc::new(vec![report("broken", MountOutcome::Disabled)]);
        st.packages_root = std::sync::Arc::new(root.path().to_path_buf());
        st.package_dirs = std::sync::Arc::new(std::collections::HashMap::from([(
            "broken".to_owned(),
            pkg_dir,
        )]));

        // The real, independently-observed reason `scan()` gives for this
        // exact broken manifest — asserted against below instead of a guessed
        // substring, so this test actually proves the response came from the
        // `scan.refused` branch (code review round 1 Low 4: deleting that
        // branch entirely, so the request instead fell through to "package
        // vanished," previously left this test green because it only checked
        // the status code).
        let expected_why = agent24_os_packages::discovery::scan(root.path())
            .refused
            .into_iter()
            .find(|r| r.dir == root.path().join("broken"))
            .expect("the broken manifest must show up as a scan refusal")
            .why;

        let res = t8_patch(&st, "broken", true, Some(path.clone())).await;
        assert_eq!(res.status(), StatusCode::CONFLICT);
        let msg = body_json(res).await["error"]["message"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(msg, expected_why);
        assert!(
            !msg.contains("no longer there"),
            "must come from scan.refused, not the package-vanished branch: {msg}"
        );
        assert!(!path.exists());
    }

    /// Judgement criterion 15a — `packages_root` itself fails
    /// `check_packages_root` (world-writable), before any per-package
    /// matching happens.
    #[tokio::test]
    async fn criterion15a_packages_root_check_fails_is_blocked() {
        use std::os::unix::fs::PermissionsExt;
        let (mut st, _dir, path) = t8_state().await;
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
        let pkg_dir = root.path().join("pkgmod");
        st.os_reports = std::sync::Arc::new(vec![report("pkgmod", MountOutcome::Disabled)]);
        st.packages_root = std::sync::Arc::new(root.path().to_path_buf());
        st.package_dirs = std::sync::Arc::new(std::collections::HashMap::from([(
            "pkgmod".to_owned(),
            pkg_dir,
        )]));

        let res = t8_patch(&st, "pkgmod", true, Some(path.clone())).await;
        assert_eq!(res.status(), StatusCode::CONFLICT);
        let msg = body_json(res).await["error"]["message"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(msg.contains("does not check out"), "{msg}");
        assert!(!path.exists());
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    /// Judgement criterion 15b — `packages_root` passes `check_packages_root`
    /// (sane metadata) but `scan()` still reports it unreadable (e.g.
    /// `read_dir` itself failing). Must be reported as a root-level failure,
    /// not misattributed to "this package's directory disappeared" (round 6
    /// Low 1 — `scan()`'s own `Refused{dir: packages_root}` entry has to be
    /// checked before per-package directory matching).
    ///
    /// Drives `classify_scan` directly with a hand-built `Scan` rather than
    /// relying on real `0o300` permission bits actually making `read_dir`
    /// fail — under a root-run test process, discretionary permission bits
    /// on a directory root owns do not restrict it, so the original
    /// permission-based version of this test was invalid in that
    /// environment (code review round 1 Low 3).
    #[test]
    fn criterion15b_packages_root_unreadable_is_blocked_not_misattributed() {
        let packages_root = std::path::PathBuf::from("/tmp/t8-criterion15b-root");
        let pkg_dir = packages_root.join("pkgmod");
        let scan = agent24_os_packages::discovery::Scan {
            found: Vec::new(),
            refused: vec![agent24_os_packages::discovery::Refused {
                dir: packages_root.clone(),
                why: "permission denied".to_owned(),
            }],
        };
        let result = classify_scan(&scan, &packages_root, "pkgmod", &pkg_dir);
        let msg = result.expect_err("a root-level scan.refused entry must block, not allow");
        assert!(
            msg.contains("could not be read"),
            "must be attributed to the ROOT, not to the package's own directory: {msg}"
        );
        assert!(
            !msg.contains("no longer there"),
            "must not be misreported as the package's own directory vanishing: {msg}"
        );
    }
}
