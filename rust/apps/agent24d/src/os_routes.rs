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
//! **A toggle does not take effect until the daemon restarts.** Routes are built
//! once at startup, and pretending otherwise would be worse than saying so: the
//! list reports the config state AND the running state separately, and sets
//! `restart_required` when the config has CHANGED since the mount pass — not
//! merely when a module is enabled and not running, which would conflate a pending
//! toggle with a module that is simply unhealthy. A user who toggles a module and
//! then sees it still serving is looking at a fact, not a bug.

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
fn view(report: &MountReport, enabled_now: bool, registry_usable: bool) -> DomainOsView {
    let (state, detail) = match &report.outcome {
        MountOutcome::Mounted => ("mounted", None),
        MountOutcome::Disabled => ("disabled", None),
        MountOutcome::Degraded(why) => ("degraded", Some(why.clone())),
        MountOutcome::Refused(why) => ("refused", Some(why.clone())),
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
    let restart_required = match report.enabled_at_start {
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
        .map(|r| view(r, cfg.is_enabled(&r.name), registry_error.is_none()))
        .collect();
    Json(DomainOsList {
        modules,
        registry_error,
    })
    .into_response()
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
    if let Err(why) = crate::os_config::OsConfig::set_enabled(&path, &name, update.enabled) {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal", &why);
    }
    tracing::info!(
        "domain OS {name:?} set enabled={} in os.json (takes effect on restart)",
        update.enabled
    );
    // Return the whole list so a client sees the new `restart_required` state
    // without a second round trip.
    render(&state)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::domain::{MountOutcome, MountReport, ResourceStatus};

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
        let v = view(&running, false, true);
        assert_eq!(v.state, "mounted", "it is still serving right now");
        assert!(!v.enabled, "but the config now says off");
        assert!(v.restart_required);

        // And the other direction.
        let off = report("sin90", MountOutcome::Disabled);
        let v = view(&off, true, true);
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
            !view(&r, true, false).restart_required,
            "the registry is still unusable; the fix is the file, not a restart"
        );
        // Once it IS usable, the config has genuinely never been applied.
        assert!(view(&r, true, true).restart_required);
    }

    #[test]
    fn a_settled_module_does_not_ask_for_a_restart() {
        assert!(!view(&report("a", MountOutcome::Mounted), true, true).restart_required);
        assert!(!view(&report("a", MountOutcome::Disabled), false, true).restart_required);
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
        let v = view(&r, true, true);
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
        let v = view(&r, true, true);
        assert!(!v.restart_required);
        assert_eq!(
            v.detail.as_deref(),
            Some("store failed to open"),
            "the actionable fact is WHAT failed, not 'restart'"
        );

        // And the case the availability comparison MISSED entirely: disabling an
        // already-degraded module IS a real pending change.
        assert!(view(&r, false, true).restart_required);
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
        let v = view(&r, true, true);
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
        let v = view(&r, false, true);
        assert_eq!(v.state, "disabled");
        assert!(!v.enabled);
        assert!(!v.restart_required, "config and runtime agree");

        // And once the user enables it, the list says a restart is what applies it.
        assert!(view(&r, true, true).restart_required);
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
        let v = view(&r, true, true);
        assert_eq!(v.resources, "missing");
        assert_eq!(v.missing_models, vec!["ornith-9b".to_owned()]);

        r.resources = ResourceStatus::Unknown("provider down".into());
        let v = view(&r, true, true);
        assert_eq!(v.resources, "unknown");
        assert!(
            v.missing_models.is_empty(),
            "an unchecked model is not a missing one"
        );

        r.resources = ResourceStatus::NotChecked;
        assert_eq!(view(&r, true, true).resources, "not_checked");
    }
}
