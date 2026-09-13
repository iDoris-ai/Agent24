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

use agent24_domain::http::error_response;
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
    hot_disabled: bool,
) -> DomainOsView {
    use agent24_os_proto::supervisor::Status;
    // A disable whose stop failed is not applied: the module is reported as
    // what it is — degraded, with the failure — and a restart is still what
    // settles it (review of SUP-5, round 1).
    let hot_disabled = hot_disabled
        && !matches!(
            live,
            Some(Status::StopFailed { .. } | Status::Panicked | Status::Killed)
        );
    let (state, detail) = match (&report.outcome, live) {
        // Stopped by `os disable` while the daemon runs: disabled, whatever the
        // mount said — still `stopping` while it drains (SUP-5).
        (MountOutcome::Mounted, Some(Status::Stopped)) if hot_disabled => ("disabled", None),
        (MountOutcome::Mounted, _) if hot_disabled => ("disabled", Some("stopping".to_owned())),
        // A package started at mount: its supervisor says where it is NOW. A
        // mount verdict alone called a module that had since given up
        // "mounted" (review of SUP-4, round 1).
        (MountOutcome::Mounted, Some(status)) => match status {
            Status::Running => ("mounted", None),
            Status::Starting { attempt } => ("mounted", Some(format!("starting (run {attempt})"))),
            Status::Stopping => ("mounted", Some("stopping".to_owned())),
            Status::Backoff { failures, delay } => (
                "degraded",
                Some(format!(
                    "restarting in {}ms after {failures} failed run(s)",
                    delay.as_millis()
                )),
            ),
            Status::GaveUp { failures, within } => (
                "degraded",
                Some(format!(
                    "gave up after {failures} failed runs within {}s",
                    within.as_secs()
                )),
            ),
            Status::StopFailed { error } => ("degraded", Some(error.clone())),
            Status::Stopped => ("degraded", Some("stopped".to_owned())),
            Status::Panicked => ("degraded", Some("its supervisor panicked".to_owned())),
            Status::Killed => ("degraded", Some("killed".to_owned())),
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
    // A hot disable has already applied "off": for the comparison below, what
    // is running is what a start with it disabled would have given.
    let running_enabled = if hot_disabled {
        Some(false)
    } else {
        report.enabled_at_start
    };
    let restart_required = match running_enabled {
        _ if matches!(report.outcome, MountOutcome::Refused(_)) => false,
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
    // Read the config fresh: it may have been changed since startup, by this very
    // process, and the point of the view is to show that divergence.
    let cfg = crate::os_config::config_path()
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

fn hot_disabled(state: &AppState, name: &str) -> bool {
    state
        .supervisors
        .as_ref()
        .is_some_and(|s| s.is_disabled(name))
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
    /// Its supervisor is draining and stopping it, and it refuses new
    /// requests.
    Stopping,
    /// Its supervisor was asked to, but the module still admitted requests
    /// when the wait ran out.
    Pending,
    /// An earlier disable already stopped it.
    AlreadyStopped,
    /// Nothing running to stop: a compiled-in module, a package not started
    /// at mount, or a daemon already shutting down.
    NotRunning,
}

/// Stop a running out-of-process module now (SUP-5): its supervisor drains
/// and stops it in the background, and the shutdown — if it begins
/// meanwhile — waits for that stop, cutting it off at `cut_off`. Waits up to
/// `within` for the module's generation to refuse new requests, so a request
/// sent after the disable answers is not admitted.
async fn stop_now(
    supervisors: Option<&crate::domain::Supervisors>,
    cut_off: impl std::future::Future<Output = ()> + Send + 'static,
    name: &str,
    within: std::time::Duration,
) -> HotStop {
    let Some(supervisors) = supervisors else {
        return HotStop::NotRunning;
    };
    let Some(current) = supervisors.disable(name, DISABLE_DRAIN, cut_off) else {
        return if supervisors.is_disabled(name) {
            HotStop::AlreadyStopped
        } else {
            HotStop::NotRunning
        };
    };
    if admission_closed(&current, within).await {
        HotStop::Stopping
    } else {
        HotStop::Pending
    }
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

pub async fn patch_os(
    State(state): State<AppState>,
    Path(name): Path<String>,
    req: Request<Body>,
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
    // Held to the end: the write, the hot disable and the answer are one step
    // against another toggle.
    let _control = state.os_control.lock().await;
    let path = match crate::os_config::config_path() {
        Some(p) => p,
        None => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "HOME not set",
            );
        }
    };
    // Off the async workers: the file lock waits for any other writer, for
    // as long as that takes (review of SUP-5, round 2).
    let written = tokio::task::spawn_blocking({
        let (path, name) = (path.clone(), name.clone());
        move || crate::os_config::OsConfig::set_enabled(&path, &name, update.enabled)
    })
    .await;
    match written {
        Ok(Ok(_)) => {}
        Ok(Err(why)) => {
            return error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal", &why);
        }
        Err(e) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                &format!("the config write failed: {e}"),
            );
        }
    }
    // A disable of a running out-of-process module applies now; anything else
    // at the next start.
    let hot = if update.enabled {
        HotStop::NotRunning
    } else {
        stop_now(
            state.supervisors.as_deref(),
            state.shutdown.modules_cut_off(),
            &name,
            ADMISSION_CLOSED_WITHIN,
        )
        .await
    };
    let effect = match hot {
        HotStop::Stopping => "stopping it now",
        HotStop::Pending => "asked its supervisor to stop it; not yet refusing requests",
        HotStop::AlreadyStopped => "already stopped by an earlier disable",
        HotStop::NotRunning => "takes effect on restart",
    };
    tracing::info!(
        "domain OS {name:?} set enabled={} in os.json ({effect})",
        update.enabled
    );
    // Not a success: a request sent after this answer could still be
    // admitted (review of SUP-5, round 2).
    if hot == HotStop::Pending {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "disable_pending",
            &format!(
                "os.json now disables {name:?} and its supervisor was asked to stop it, but it \
                 still admitted requests after {ADMISSION_CLOSED_WITHIN:?}; `agent24 os list` \
                 shows when it has stopped"
            ),
        );
    }
    // Return the whole list so a client sees the new `restart_required` state
    // without a second round trip.
    render(&state)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::domain::{MountOutcome, MountReport, ResourceStatus};

    /// What a disable reports: `Stopping` once the module refuses new
    /// requests; `Pending` if it still admitted them when the wait ran out —
    /// here at once, on this single-threaded runtime, before its supervisor
    /// has run at all — and `AlreadyStopped` for a second disable; nothing
    /// to stop without supervisors (review of SUP-5, round 2).
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
            HotStop::AlreadyStopped
        );
        for stop in host.supervisors.close().disabling {
            stop.await.unwrap();
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
            stop.await.unwrap();
        }
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
        let v = view(&r, false, true, Some(&Status::Stopping), true);
        assert_eq!(
            (v.state.as_str(), v.detail.as_deref()),
            ("disabled", Some("stopping"))
        );
        assert!(!v.restart_required);
        let v = view(&r, false, true, Some(&Status::Stopped), true);
        assert_eq!((v.state.as_str(), v.detail.as_deref()), ("disabled", None));
        assert!(!v.restart_required);
        let v = view(&r, false, true, Some(&Status::Running), false);
        assert!(v.restart_required, "a pending disable not applied yet");
        // A stop that failed is not a disable applied.
        let failed = Status::StopFailed {
            error: "still there".into(),
        };
        let v = view(&r, false, true, Some(&failed), true);
        assert_eq!(
            (v.state.as_str(), v.detail.as_deref()),
            ("degraded", Some("still there"))
        );
        assert!(v.restart_required);
    }

    /// A package mounted at start is reported by its supervisor's status NOW:
    /// one that has given up is degraded, with the reason — not "mounted"
    /// (review of SUP-4, round 1). Control: a running one is mounted.
    #[test]
    fn a_mounted_package_is_reported_by_its_live_status() {
        use agent24_os_proto::supervisor::Status;
        let r = report("pkg", MountOutcome::Mounted);
        let gave_up = Status::GaveUp {
            failures: 5,
            within: std::time::Duration::from_secs(4),
        };
        let v = view(&r, true, true, Some(&gave_up), false);
        assert_eq!(v.state, "degraded");
        assert!(
            v.detail.as_deref().is_some_and(|d| d.contains("gave up")),
            "{:?}",
            v.detail
        );
        let v = view(&r, true, true, Some(&Status::Running), false);
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
        let v = view(&running, false, true, None, false);
        assert_eq!(v.state, "mounted", "it is still serving right now");
        assert!(!v.enabled, "but the config now says off");
        assert!(v.restart_required);

        // And the other direction.
        let off = report("sin90", MountOutcome::Disabled);
        let v = view(&off, true, true, None, false);
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
            !view(&r, true, false, None, false).restart_required,
            "the registry is still unusable; the fix is the file, not a restart"
        );
        // Once it IS usable, the config has genuinely never been applied.
        assert!(view(&r, true, true, None, false).restart_required);
    }

    #[test]
    fn a_settled_module_does_not_ask_for_a_restart() {
        assert!(
            !view(&report("a", MountOutcome::Mounted), true, true, None, false).restart_required
        );
        assert!(
            !view(
                &report("a", MountOutcome::Disabled),
                false,
                true,
                None,
                false
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
        let v = view(&r, true, true, None, false);
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
        let v = view(&r, true, true, None, false);
        assert!(!v.restart_required);
        assert_eq!(
            v.detail.as_deref(),
            Some("store failed to open"),
            "the actionable fact is WHAT failed, not 'restart'"
        );

        // And the case the availability comparison MISSED entirely: disabling an
        // already-degraded module IS a real pending change.
        assert!(view(&r, false, true, None, false).restart_required);
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
        let v = view(&r, true, true, None, false);
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
        let v = view(&r, false, true, None, false);
        assert_eq!(v.state, "disabled");
        assert!(!v.enabled);
        assert!(!v.restart_required, "config and runtime agree");

        // And once the user enables it, the list says a restart is what applies it.
        assert!(view(&r, true, true, None, false).restart_required);
    }

    #[tokio::test]
    async fn the_registry_endpoint_is_behind_kernel_auth() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        let st = crate::server::tests::state().await;
        let router = crate::server::build_router_with_modules(st, axum::Router::new());
        for (method, uri) in [("GET", "/api/v1/os"), ("PATCH", "/api/v1/os/sin90")] {
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
        let v = view(&r, true, true, None, false);
        assert_eq!(v.resources, "missing");
        assert_eq!(v.missing_models, vec!["ornith-9b".to_owned()]);

        r.resources = ResourceStatus::Unknown("provider down".into());
        let v = view(&r, true, true, None, false);
        assert_eq!(v.resources, "unknown");
        assert!(
            v.missing_models.is_empty(),
            "an unchecked model is not a missing one"
        );

        r.resources = ResourceStatus::NotChecked;
        assert_eq!(view(&r, true, true, None, false).resources, "not_checked");
    }
}
