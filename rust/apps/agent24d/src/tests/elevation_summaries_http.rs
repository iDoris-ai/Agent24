//! K-B3b (#1715): the presence text of an approval decision, a schedule
//! change and shutdown, end to end at the HTTP boundary: refused with
//! `presence_required`, elevated with the text the daemon derived, and the
//! approved request let through.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use crate::capabilities::first_party::{DERIVED_ROUTES, body_digest, resolve_target, test_peer};
use crate::capabilities::presence::*;

use crate::capabilities::{AssuranceLevel, Operation, policies};
use crate::enroll_api::{ElevateRequest, EnrollAuthority, EnrollErrorCode, IssuedToken};
use agent24_protocol::{
    Approval, ApprovalStatus, ModuleApprovalKind, Run, RunInput, RunMode, RunStatus,
    ScheduleAction, ScheduleCreate, ScheduleSpec, Session, ToolCall, ToolCallStatus, Usage,
};
use http_body_util::BodyExt;
use std::sync::Mutex;
use tower::ServiceExt;

const APPROVAL: &str = "apr_kb3b";
const RUN: &str = "run_kb3b";
const TOOL_CALL: &str = "tc_kb3b";
const PROMPT_MARK: &str = "PROMPT-MARK-9d1f";
const REASON_MARK: &str = "REASON-MARK-77ae";
const SECRET_MARK: &str = "SECRET-MARK-40c2";
const ALL_DECISIONS: [&str; 5] = [
    "approve",
    "approve_for_session",
    "approve_for_target",
    "deny",
    "abort",
];

struct Capture(Mutex<Vec<String>>);
#[async_trait::async_trait]
impl PresenceProvider for Capture {
    async fn confirm(&self, req: PresenceRequest, _cancel: CancellationToken) -> PresenceOutcome {
        self.0.lock().unwrap().push(req.summary);
        PresenceOutcome::Allowed
    }
}

struct Daemon {
    state: AppState,
    token: IssuedToken,
    router: axum::Router,
    capture: Arc<Capture>,
}

async fn daemon() -> Daemon {
    daemon_with(|_| {}).await
}

async fn daemon_with(configure: impl FnOnce(&mut AppState)) -> Daemon {
    let (mut state, _, _) = tests::capability_state().await;
    let capture = Arc::new(Capture(Mutex::new(Vec::new())));
    state.presence_provider = Some(capture.clone());
    configure(&mut state);
    let token = state
        .first_party
        .as_ref()
        .unwrap()
        .issue(&test_peer("daemon-test"))
        .unwrap();
    let router = build_router(state.clone());
    Daemon {
        state,
        token,
        router,
        capture,
    }
}

/// Decisions open, `doc-module.read_document` on the grants protocol and
/// `old-module.read_document` not.
fn with_cards(state: &mut AppState) {
    let summary = agent24_store::ToolPermissionSummary::new(
        "doc-module",
        "read_document",
        "1.0.0",
        agent24_store::ConsentSource::ManualInstall,
        None,
        None,
        None,
        agent24_store::HostRiskLevel::High,
    );
    let card = |takes_grants| crate::consent_routes::ToolCardSource {
        summary: summary.clone(),
        resource_kinds: vec!["path".to_owned(), "url".to_owned()],
        takes_grants,
    };
    state.module_tool_cards = Some(crate::consent_routes::ModuleToolCards {
        notice: Arc::new(|_| {
            Box::pin(async { crate::module_tool_runtime::RevocationStop::NotRunning })
                as futures::future::BoxFuture<'static, _>
        }),
        tools: Arc::new(std::collections::HashMap::from([
            (
                ("doc-module".to_owned(), "read_document".to_owned()),
                card(true),
            ),
            (
                ("old-module".to_owned(), "read_document".to_owned()),
                card(false),
            ),
        ])),
        artifact: Arc::new(|_: &str| Some("a".to_owned())),
    });
    state.module_grants = Some(crate::consent_routes::GrantSettings {
        max_days: 30_000_000,
        os_config: std::path::PathBuf::from("/nonexistent/os.json"),
    });
}

fn request(
    token: &str,
    method: &str,
    path: &str,
    body: &str,
    grant: Option<&str>,
) -> Request<Body> {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(grant) = grant {
        request = request.header("X-Agent24-Elevation", grant);
    }
    request.body(Body::from(body.to_owned())).unwrap()
}

impl Daemon {
    /// The first, refused request; then the elevate the CLI would send. The
    /// text the user is asked about, or why there is nothing to approve.
    async fn ask(&self, method: &str, path: &str, body: &str) -> Result<String, EnrollErrorCode> {
        self.ask_then(method, path, body, true).await
    }

    /// `send_approved: false` stops after the elevate: an approved shutdown
    /// would arm the shutdown watchdog, which exits the test process.
    async fn ask_then(
        &self,
        method: &str,
        path: &str,
        body: &str,
        send_approved: bool,
    ) -> Result<String, EnrollErrorCode> {
        let refused = self
            .router
            .clone()
            .oneshot(request(&self.token.secret, method, path, body, None))
            .await
            .unwrap();
        assert_eq!(refused.status(), StatusCode::FORBIDDEN, "{method} {path}");
        let bytes = refused.into_body().collect().await.unwrap().to_bytes();
        let code: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            code["error"]["code"], "presence_required",
            "{method} {path}"
        );
        self.capture.0.lock().unwrap().clear();
        let (tx, _rx) = tokio::sync::oneshot::channel();
        let grant = self
            .state
            .first_party
            .as_ref()
            .unwrap()
            .elevate(
                &test_peer("daemon-test"),
                &self.token.token_id,
                ElevateRequest {
                    method: method.into(),
                    path: path.into(),
                    body_sha256: body_digest(body.as_bytes()),
                    client_summary: "client says: harmless".into(),
                },
                tx,
                CancellationToken::new(),
            )
            .await?;
        let shown = self.capture.0.lock().unwrap().clone();
        assert_eq!(shown.len(), 1);
        assert!(!shown[0].contains("client says"), "{}", shown[0]);
        if !send_approved {
            return Ok(shown[0].clone());
        }
        // The approved request passes the gate; the handler answers (it may
        // still refuse on its own grounds, e.g. module grants closed here).
        let approved = self
            .router
            .clone()
            .oneshot(request(
                &self.token.secret,
                method,
                path,
                body,
                Some(grant.secret.as_str()),
            ))
            .await
            .unwrap();
        if approved.status() == StatusCode::FORBIDDEN {
            let bytes = approved.into_body().collect().await.unwrap().to_bytes();
            let code: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            let code = code["error"]["code"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            assert!(
                !code.contains("presence") && !code.contains("elevation"),
                "{method} {path}: {code}"
            );
        }
        Ok(shown[0].clone())
    }

    /// A pending tool approval as the agent leaves it: a session, its run,
    /// the gated tool call, then the approval (policy's own summary).
    async fn seed_approval(
        &self,
        title: &str,
        payload: serde_json::Value,
        decisions: &[&str],
    ) -> Approval {
        let store = &self.state.store;
        let now = agent24_core::util::now_iso8601();
        store
            .insert_session(&Session {
                id: "sess_kb3b".into(),
                title: title.into(),
                channel: "desktop".into(),
                workspace_id: None,
                created_at: now.clone(),
                updated_at: now.clone(),
            })
            .await
            .unwrap();
        store
            .insert_run(&Run {
                id: RUN.into(),
                session_id: Some("sess_kb3b".into()),
                workspace_id: None,
                status: RunStatus::AwaitingApproval,
                input: RunInput {
                    prompt: PROMPT_MARK.into(),
                    workspace_id: None,
                    model_override: None,
                    mode: RunMode::Normal,
                    knowledge_binding_id: None,
                },
                output: None,
                error: None,
                usage: Usage {
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    total_tokens: 0,
                    cost_usd: 0.0,
                },
                schedule_id: None,
                created_at: now.clone(),
                started_at: Some(now.clone()),
                ended_at: None,
            })
            .await
            .unwrap();
        let input = payload.as_object().unwrap().clone();
        store
            .insert_tool_call(&ToolCall {
                id: TOOL_CALL.into(),
                run_id: RUN.into(),
                tool: "shell_exec".into(),
                input: input.clone(),
                status: ToolCallStatus::Running,
                output_summary: None,
                started_at: now.clone(),
                ended_at: None,
            })
            .await
            .unwrap();
        let approval = Approval {
            id: APPROVAL.into(),
            run_id: RUN.into(),
            tool_call_id: TOOL_CALL.into(),
            kind: "exec".into(),
            // As agent24-policy writes it.
            summary: format!("shell_exec: {}", agent24_tools::summarize_input(&input)),
            payload: input,
            available_decisions: decisions.iter().map(|d| (*d).to_owned()).collect(),
            standing_target: decisions
                .contains(&"approve_for_target")
                .then(|| "#ops".to_owned()),
            status: ApprovalStatus::Pending,
            decision: None,
            expires_at: "2999-01-01T00:00:00Z".into(),
            created_at: now,
            decided_at: None,
        };
        store.insert_approval(&approval).await.unwrap();
        approval
    }
}

fn decision(kind: &str) -> String {
    if kind == "deny" {
        serde_json::json!({ "type": kind, "reason": REASON_MARK }).to_string()
    } else {
        serde_json::json!({ "type": kind }).to_string()
    }
}

/// The preview: what follows `arguments: ` in the text.
fn preview_of(text: &str) -> &str {
    text.split_once("arguments: ").unwrap().1
}

#[tokio::test]
async fn each_decision_names_its_scope_the_approval_the_run_the_title_and_the_tool() {
    for (kind, scope) in [
        ("approve", "approve once"),
        (
            "approve_for_session",
            "approve for the rest of this session",
        ),
        ("approve_for_target", "approve always for target \"#ops\""),
        ("deny", "deny (with your reason)"),
        ("abort", "abort the whole run"),
    ] {
        let d = daemon().await;
        d.seed_approval(
            "Leave plan",
            serde_json::json!({ "argv": ["ls", "-la"], "cwd": "/w" }),
            &ALL_DECISIONS,
        )
        .await;
        let text = d
            .ask(
                "POST",
                &format!("/api/v1/approvals/{APPROVAL}"),
                &decision(kind),
            )
            .await
            .unwrap();
        assert!(
            text.contains(&format!(
                "{scope}: approval \"{APPROVAL}\" of run \"{RUN}\""
            )),
            "{text}"
        );
        assert!(text.contains("conversation \"Leave plan\""), "{text}");
        assert!(text.contains("tool \"shell_exec\""), "{text}");
        assert!(
            text.contains(r#"arguments: {"argv":["ls","-la"],"cwd":"/w"}"#),
            "{text}"
        );
        // Neither the run's input text nor the deny reason.
        assert!(
            !text.contains(PROMPT_MARK) && !text.contains(REASON_MARK),
            "{text}"
        );
    }
}

#[tokio::test]
async fn the_preview_shows_what_the_ipc_dialog_showed_less_any_credential() {
    // No credential: byte for byte what follows "shell_exec: " in the
    // approval's own summary.
    let d = daemon().await;
    let approval = d
        .seed_approval(
            "t",
            serde_json::json!({ "argv": ["rm", "-r", "/tmp/build"], "cwd": "/w" }),
            &["approve"],
        )
        .await;
    let text = d
        .ask(
            "POST",
            &format!("/api/v1/approvals/{APPROVAL}"),
            &decision("approve"),
        )
        .await
        .unwrap();
    assert_eq!(
        preview_of(&text),
        approval.summary.strip_prefix("shell_exec: ").unwrap()
    );

    // A credential at any depth: the summary has it, the text only "***".
    let d = daemon().await;
    let approval = d
        .seed_approval(
            "t",
            serde_json::json!({
                "url": "https://x.example",
                "headers": { "Authorization": SECRET_MARK, "X-Trace": "keep" },
                "items": [{ "github_token": SECRET_MARK }],
                "Api_Key": SECRET_MARK,
            }),
            &["approve"],
        )
        .await;
    assert!(approval.summary.contains(SECRET_MARK));
    let text = d
        .ask(
            "POST",
            &format!("/api/v1/approvals/{APPROVAL}"),
            &decision("approve"),
        )
        .await
        .unwrap();
    assert!(!text.contains(SECRET_MARK), "{text}");
    assert_eq!(preview_of(&text).matches("\"***\"").count(), 3, "{text}");
    assert!(
        text.contains("https://x.example") && text.contains("\"X-Trace\":\"keep\""),
        "{text}"
    );
}

#[tokio::test]
async fn a_long_preview_is_cut_and_the_whole_text_fits_the_dialog() {
    let d = daemon().await;
    let long = "a".repeat(1_000);
    d.seed_approval(
        &"标题".repeat(200),
        serde_json::json!({ "argv": [long] }),
        &["approve"],
    )
    .await;
    let text = d
        .ask(
            "POST",
            &format!("/api/v1/approvals/{APPROVAL}"),
            &decision("approve"),
        )
        .await
        .unwrap();
    let (kept, marker) = preview_of(&text).split_once('…').unwrap();
    assert!(
        kept.len() <= crate::elevation_summaries::PREVIEW_MAX_BYTES,
        "{kept}"
    );
    assert!(marker.starts_with(" [truncated "), "{marker}");
    // The title was cut, not the preview; the dialog string is in bounds.
    assert!(text.contains("…\"; tool"), "{text}");
    assert!(text.contains("conversation \"标题标题"), "{text}");
    // The whole dialog string, prefix included.
    assert!(
        text.encode_utf16().count() <= 500,
        "{}",
        text.encode_utf16().count()
    );
}

#[tokio::test]
async fn hidden_characters_are_shown_escaped_and_the_text_is_kept() {
    let d = daemon().await;
    d.seed_approval(
        "a\u{200B}b",
        serde_json::json!({ "argv": ["echo", "safe\u{202E}txt.exe", "x\u{200B}y"] }),
        &["approve"],
    )
    .await;
    let text = d
        .ask(
            "POST",
            &format!("/api/v1/approvals/{APPROVAL}"),
            &decision("approve"),
        )
        .await
        .unwrap();
    assert!(
        text.contains("safe\\u{202E}txt.exe") && text.contains("x\\u{200B}y"),
        "{text}"
    );
    assert!(text.contains("\"a\\u{200B}b\""), "{text}");
    assert!(
        !text.contains('\u{202E}') && !text.contains('\u{200B}'),
        "{text:?}"
    );
}

#[tokio::test]
async fn a_blank_title_shows_as_untitled() {
    let d = daemon().await;
    d.seed_approval("   ", serde_json::json!({}), &["approve"])
        .await;
    let text = d
        .ask(
            "POST",
            &format!("/api/v1/approvals/{APPROVAL}"),
            &decision("approve"),
        )
        .await
        .unwrap();
    assert!(text.contains("conversation \"untitled\""), "{text}");
}

/// Nothing to approve, so `not_elevated_operation`: an unknown decision, one
/// the approval does not offer, no such approval, one already decided, one
/// whose tool call cannot be found.
#[tokio::test]
async fn nothing_is_approvable_when_a_field_cannot_be_found() {
    let not_elevated = |r: Result<String, EnrollErrorCode>| {
        assert!(
            matches!(r, Err(EnrollErrorCode::NotElevatedOperation)),
            "{r:?}"
        );
    };
    let path = format!("/api/v1/approvals/{APPROVAL}");
    let d = daemon().await;
    d.seed_approval("t", serde_json::json!({}), &["approve", "deny"])
        .await;
    not_elevated(d.ask("POST", &path, &decision("approve_harder")).await);
    not_elevated(d.ask("POST", &path, &decision("abort")).await);
    not_elevated(
        d.ask(
            "POST",
            "/api/v1/approvals/apr_missing",
            &decision("approve"),
        )
        .await,
    );
    // approve_for_target offered without a standing target.
    sqlx::query("UPDATE approvals SET available_decisions='[\"approve_for_target\"]' WHERE id=?")
        .bind(APPROVAL)
        .execute(agent24_store::test_hooks::pool(&d.state.store))
        .await
        .unwrap();
    not_elevated(d.ask("POST", &path, &decision("approve_for_target")).await);

    let d = daemon().await;
    d.seed_approval("t", serde_json::json!({}), &["approve"])
        .await;
    sqlx::query("DELETE FROM tool_calls WHERE id=?")
        .bind(TOOL_CALL)
        .execute(agent24_store::test_hooks::pool(&d.state.store))
        .await
        .unwrap();
    not_elevated(d.ask("POST", &path, &decision("approve")).await);

    let d = daemon().await;
    d.seed_approval("t", serde_json::json!({}), &["approve"])
        .await;
    sqlx::query("UPDATE approvals SET status='denied' WHERE id=?")
        .bind(APPROVAL)
        .execute(agent24_store::test_hooks::pool(&d.state.store))
        .await
        .unwrap();
    not_elevated(d.ask("POST", &path, &decision("approve")).await);
}

#[tokio::test]
async fn module_approvals_name_the_module_action_and_arguments() {
    let d = daemon().await;
    let payload = serde_json::json!({ "doc": "q3.md", "password": SECRET_MARK });
    d.state
        .store
        .insert_module_approval(
            "mapr_kb3b",
            "doc-module",
            "req-1",
            ModuleApprovalKind::Advise,
            false,
            "publish",
            Some("team-wiki"),
            &payload,
            "digest",
            &agent24_core::util::now_iso8601(),
            "2999-01-01T00:00:00Z",
        )
        .await
        .unwrap();
    let path = "/api/v1/module-approvals/mapr_kb3b";
    let denied = r#"{"decision":"denied"}"#;
    let approved = r#"{"decision":"approved"}"#;
    // The deny text first, without sending it; then approve it for real.
    let deny_text = d.ask_then("POST", path, denied, false).await.unwrap();
    let approve_text = d.ask("POST", path, approved).await.unwrap();
    for (verb, text) in [("deny", deny_text), ("approve", approve_text)] {
        assert!(
            text.contains(&format!(
                "{verb} module approval \"mapr_kb3b\": module \"doc-module\", action \"publish\"; target \"team-wiki\"; arguments: "
            )),
            "{text}"
        );
        assert!(
            text.contains("\"doc\":\"q3.md\"") && !text.contains(SECRET_MARK),
            "{text}"
        );
    }
    // Decided: nothing left to approve.
    let again = d.ask("POST", path, denied).await;
    assert!(
        matches!(again, Err(EnrollErrorCode::NotElevatedOperation)),
        "{again:?}"
    );
}

#[tokio::test]
async fn a_module_consent_names_module_tool_expiry_and_resource_kinds_only() {
    let d = daemon_with(with_cards).await;
    let body = serde_json::json!({
        "decision": "granted", "fingerprint": "f", "module_version": "1.0.0", "artifact": "a",
        "resources": ["path:/Users/someone/SECRET-PATH", "path:/tmp", "url:https://x"],
        "expires_at": "2999-01-01T00:00:00Z",
    })
    .to_string();
    let text = d
        .ask(
            "POST",
            "/api/v1/module-consents/modules/doc-module/tools/read_document",
            &body,
        )
        .await
        .unwrap();
    assert!(
        text.ends_with(
            "grant consent for module \"doc-module\" tool \"read_document\" until \"2999-01-01T00:00:00Z\"; 3 resource(s) of kind \"path, url\""
        ),
        "{text}"
    );
    assert!(!text.contains("SECRET-PATH"), "{text}");
}

/// No card, a card off the grants protocol, or decisions closed on this
/// host: the handler refuses, so there is nothing to approve.
#[tokio::test]
async fn a_module_consent_without_a_grants_card_has_nothing_to_approve() {
    let body = serde_json::json!({
        "decision": "granted", "fingerprint": "f", "module_version": "1.0.0", "artifact": "a",
        "resources": [], "expires_at": "2999-01-01T00:00:00Z",
    })
    .to_string();
    let path =
        |module: &str| format!("/api/v1/module-consents/modules/{module}/tools/read_document");
    let d = daemon_with(with_cards).await;
    for module in ["missing", "old-module"] {
        let r = d.ask("POST", &path(module), &body).await;
        assert!(
            matches!(r, Err(EnrollErrorCode::NotElevatedOperation)),
            "{module}: {r:?}"
        );
    }
    let closed = daemon_with(|state| {
        with_cards(state);
        state.module_grants = None;
    })
    .await;
    let r = closed.ask("POST", &path("doc-module"), &body).await;
    assert!(
        matches!(r, Err(EnrollErrorCode::NotElevatedOperation)),
        "{r:?}"
    );
    let ok = d.ask("POST", &path("doc-module"), &body).await.unwrap();
    assert!(ok.contains("no resources"), "{ok}");
}

#[tokio::test]
async fn schedule_changes_name_the_schedule_and_never_its_prompt() {
    let d = daemon().await;
    let created = d
        .state
        .scheduler
        .create(
            ScheduleCreate {
                name: "Weekly report".into(),
                enabled: true,
                spec: ScheduleSpec::Every { secs: 3600 },
                action: ScheduleAction::AgentRun {
                    prompt: PROMPT_MARK.into(),
                    session_id: None,
                    model_override: None,
                },
                delivery: vec![],
            },
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    let id = created.id.clone();
    let named = format!("schedule \"Weekly report\" (\"{id}\")");
    let create = serde_json::json!({
        "name": "Nightly", "enabled": false,
        "spec": { "type": "cron", "expr": "0 3 * * *", "tz": "Asia/Shanghai" },
        "action": { "type": "agent_run", "prompt": PROMPT_MARK },
    })
    .to_string();
    let patch = serde_json::json!({ "name": "Weekly", "action": { "type": "agent_run", "prompt": PROMPT_MARK } }).to_string();
    for (method, path, body, expected) in [
        (
            "POST",
            "/api/v1/schedules".to_owned(),
            create.as_str(),
            "create schedule \"Nightly\" (cron \"0 3 * * *\" in \"Asia/Shanghai\"; disabled)"
                .to_owned(),
        ),
        (
            "POST",
            format!("/api/v1/schedules/{id}/run_now"),
            "",
            format!("run {named} now"),
        ),
        (
            "POST",
            format!("/api/v1/schedules/{id}/suspend"),
            "",
            format!("suspend {named}"),
        ),
        (
            "POST",
            format!("/api/v1/schedules/{id}/resume"),
            "",
            format!("resume {named}"),
        ),
        (
            "PATCH",
            format!("/api/v1/schedules/{id}"),
            patch.as_str(),
            format!("change {named}: name, action"),
        ),
    ] {
        let text = d.ask(method, &path, body).await.unwrap();
        assert!(text.ends_with(&expected), "{text}");
        assert!(!text.contains(PROMPT_MARK), "{text}");
    }
    let renamed = format!("schedule \"Weekly\" (\"{id}\")");
    let text = d
        .ask("DELETE", &format!("/api/v1/schedules/{id}"), "")
        .await
        .unwrap();
    assert!(text.ends_with(&format!("delete {renamed}")), "{text}");
    // No such schedule: nothing to approve.
    let gone = d.ask("DELETE", "/api/v1/schedules/sch_missing", "").await;
    assert!(
        matches!(gone, Err(EnrollErrorCode::NotElevatedOperation)),
        "{gone:?}"
    );
}

#[tokio::test]
async fn shutdown_has_its_fixed_text() {
    let d = daemon().await;
    let text = d
        .ask_then("POST", "/api/v1/shutdown", "", false)
        .await
        .unwrap();
    assert!(
        text.ends_with(crate::elevation_summaries::SHUTDOWN_TEXT),
        "{text}"
    );
}

/// A query is none of what a grant binds: nothing is remembered, so there is
/// nothing to approve (and a granted request with one is refused anyway).
#[tokio::test]
async fn a_query_string_leaves_nothing_to_approve() {
    let d = daemon().await;
    let refused = d
        .router
        .clone()
        .oneshot(request(
            &d.token.secret,
            "POST",
            "/api/v1/shutdown?now=1",
            "",
            None,
        ))
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);
    let (tx, _rx) = tokio::sync::oneshot::channel();
    let r = d
        .state
        .first_party
        .as_ref()
        .unwrap()
        .elevate(
            &test_peer("daemon-test"),
            &d.token.token_id,
            ElevateRequest {
                method: "POST".into(),
                path: "/api/v1/shutdown".into(),
                body_sha256: body_digest(b""),
                client_summary: String::new(),
            },
            tx,
            CancellationToken::new(),
        )
        .await;
    assert!(
        matches!(r, Err(EnrollErrorCode::NotElevatedOperation)),
        "{r:?}"
    );
}

/// The level table is unchanged: every derived route is an Elevated row of
/// its operation, and these are all of the three operations' Elevated rows.
#[test]
fn derived_routes_are_the_elevated_rows_of_the_three_operations() {
    let mut elevated: Vec<(String, String)> = policies()
        .iter()
        .filter(|p| {
            matches!(
                p.operation,
                Operation::ApprovalDecision | Operation::ScheduleManage | Operation::Shutdown
            ) && p.level == AssuranceLevel::Elevated
        })
        .map(|p| (p.method.to_owned(), p.template.to_owned()))
        .collect();
    let mut derived: Vec<(String, String)> = DERIVED_ROUTES
        .iter()
        .map(|(m, t)| ((*m).to_owned(), (*t).to_owned()))
        .collect();
    elevated.sort();
    derived.sort();
    assert_eq!(derived, elevated);
    assert_eq!(
        Operation::ApprovalDecision.assurance_level(),
        Some(AssuranceLevel::Elevated)
    );
    for (method, template) in DERIVED_ROUTES {
        let path = template
            .replace("{id}", "x")
            .replace("{module}", "m")
            .replace("{tool}", "t");
        let target = resolve_target(&ElevateRequest {
            method: method.into(),
            path,
            body_sha256: body_digest(b""),
            client_summary: String::new(),
        })
        .unwrap();
        assert_eq!(target.template, template);
    }
}
