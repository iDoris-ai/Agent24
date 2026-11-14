    #[test]
    fn token_is_32_bytes_hex_and_unique() {
        let a = generate_token();
        let b = generate_token();
        assert_eq!(a.len(), 64);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn guardian_is_off_unless_explicitly_enabled() {
        // Fail-safe: absent / empty / typo / "0" / "no" all leave it OFF.
        assert!(!guardian_enabled(None));
        assert!(!guardian_enabled(Some("")));
        assert!(!guardian_enabled(Some("0")));
        assert!(!guardian_enabled(Some("no")));
        assert!(!guardian_enabled(Some("ture"))); // typo must not enable
        // Only an explicit opt-in turns it on.
        assert!(guardian_enabled(Some("1")));
        assert!(guardian_enabled(Some("true")));
        assert!(guardian_enabled(Some("TRUE")));
        assert!(guardian_enabled(Some(" 1 ")));
    }

    #[tokio::test]
    async fn approval_recovery_scan_expires_tokenless_run_and_stops_with_shutdown() {
        let state = state().await;
        seed_run(&state.store, "run_expired").await;
        sqlx::query("UPDATE runs SET status='awaiting_approval' WHERE id='run_expired'")
            .execute(agent24_store::test_hooks::pool(&state.store))
            .await
            .unwrap();
        state
            .store
            .insert_approval(&agent24_protocol::Approval {
                id: "apr_expired".to_owned(),
                run_id: "run_expired".to_owned(),
                tool_call_id: "tc_expired".to_owned(),
                kind: "exec".to_owned(),
                summary: "expired".to_owned(),
                payload: serde_json::Map::new(),
                available_decisions: vec!["approve".to_owned()],
                standing_target: None,
                status: agent24_protocol::ApprovalStatus::Pending,
                decision: None,
                expires_at: "2020-01-01T00:00:00Z".to_owned(),
                created_at: "2019-12-31T23:59:00Z".to_owned(),
                decided_at: None,
            })
            .await
            .unwrap();

        let cancel = CancellationToken::new();
        let task = spawn_approval_recovery_scan_every(
            Arc::clone(&state.broker),
            Arc::clone(&state.runs),
            Duration::from_millis(10),
            cancel.clone(),
        );
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if state
                    .store
                    .get_run("run_expired")
                    .await
                    .unwrap()
                    .unwrap()
                    .status
                    == agent24_protocol::RunStatus::Cancelled
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("recovery scan should cancel the parked run");

        assert_eq!(
            state
                .store
                .get_approval("apr_expired")
                .await
                .unwrap()
                .unwrap()
                .status,
            agent24_protocol::ApprovalStatus::TimedOut
        );
        assert_eq!(
            state
                .store
                .get_run("run_expired")
                .await
                .unwrap()
                .unwrap()
                .status,
            agent24_protocol::RunStatus::Cancelled
        );

        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("shutdown should stop the recovery scan")
            .unwrap();
    }

    #[test]
    fn startup_recovers_timed_out_runs_before_strict_workspace_orphan_sweep() {
        let server = include_str!("../../server.rs");
        let steps = include_str!("../../serve_steps.rs");
        let serve = &server[server
            .find("pub async fn serve(")
            .expect("serve must exist")..];
        let serve = &serve[..serve.find("\n}\n").expect("serve must be brace-terminated")];
        assert!(
            serve.contains("recover_durable_runs("),
            "serve must run durable recovery"
        );
        let marker = "fn recover_durable_runs(";
        let start = steps.find(marker).expect("recover_durable_runs");
        let body = &steps[start..];
        let body = &body[..body
            .find("\n}\n")
            .expect("function must be brace-terminated")];
        let timeout = body
            .find(".timeout_expired(&approval_now)")
            .expect("startup must time out expired approvals");
        let recovery = body
            .find(".recover_timed_out_approval_runs()")
            .expect("startup must recover token-less timed-out runs");
        let restore = body
            .find(".restore_pending_approvals()")
            .expect("startup must restore durable pending approvals");
        let legacy_orphans = body
            .find(".sweep_orphan_runs(&now)")
            .expect("startup must sweep legacy orphans");
        let workspace_orphans = body
            .find(".sweep_workspace_orphan_runs(&workspace_now)")
            .expect("startup must sweep workspace orphans");
        assert!(
            timeout < recovery
                && recovery < restore
                && restore < legacy_orphans
                && legacy_orphans < workspace_orphans,
            "startup order must be timeout -> timed-out recovery -> durable restore -> legacy orphan -> workspace orphan"
        );
    }

    #[test]
    fn always_review_defaults_to_exec_and_parses_lists() {
        // Default keeps shell_exec human-gated even with the guardian on.
        assert_eq!(parse_always_review(None), vec!["exec".to_owned()]);
        assert_eq!(
            parse_always_review(Some("exec, fs_write ,network")),
            vec![
                "exec".to_owned(),
                "fs_write".to_owned(),
                "network".to_owned()
            ]
        );
        // Explicitly empty = operator allows every kind to be auto-approvable.
        assert!(parse_always_review(Some("")).is_empty());
        assert!(parse_always_review(Some(" , ")).is_empty());
    }

    /// An approval row has a FK to its run, so escalation tests must seed one.
    async fn seed_run(store: &Store, id: &str) {
        let now = agent24_core::util::now_iso8601();
        store
            .insert_run(&agent24_protocol::Run {
                id: id.to_owned(),
                session_id: None,
                workspace_id: None,
                status: agent24_protocol::RunStatus::Running,
                input: agent24_protocol::RunInput {
                    prompt: "p".to_owned(),
                    workspace_id: None,
                    model_override: None,
                    mode: agent24_protocol::RunMode::Normal,
                    knowledge_binding_id: None,
                },
                output: None,
                error: None,
                usage: agent24_protocol::Usage {
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    total_tokens: 0,
                    cost_usd: 0.0,
                },
                schedule_id: None,
                created_at: now.clone(),
                started_at: Some(now),
                ended_at: None,
            })
            .await
            .unwrap();
    }

    /// Drive one gated call through the daemon's real broker.
    async fn gated_call(state: &AppState, tool: &str, kind: &str) -> agent24_policy::Verdict {
        state
            .broker
            .request(
                req(
                    "run_1",
                    Some("sess_1"),
                    "tc_1",
                    tool,
                    kind,
                    format!("{tool}: x"),
                    serde_json::Map::new(),
                ),
                &CancellationToken::new(),
            )
            .await
    }

    #[tokio::test]
    async fn wired_guardian_auto_approves_low_risk_without_a_human() {
        // Codex follow-up: prove the daemon's broker really consults the injected
        // guardian. A low verdict on a non-always-review kind auto-approves with
        // NO approval row (nobody was asked) — and it returns immediately, so no
        // 300s human-approval path is involved.
        let state = state_with_guardian(Some(stub_guardian(
            agent24_policy::guardian::RiskLevel::Low,
            vec![],
        )))
        .await;
        let verdict = gated_call(&state, "fs_write", "fs_write").await;
        assert_eq!(verdict, agent24_policy::Verdict::Approved);
        assert!(state.store.list_approvals(None).await.unwrap().is_empty());
        let audits = state.store.list_audit().await.unwrap();
        assert!(audits.iter().any(|a| a.action == "approval.auto_approved"));
    }

    #[tokio::test]
    async fn wired_guardian_never_auto_approves_an_always_review_kind() {
        // The default always-review list keeps shell_exec ("exec") human-gated
        // even when the model says low. Escalation is audited; we cancel rather
        // than wait out the approval timeout.
        let state = state_with_guardian(Some(stub_guardian(
            agent24_policy::guardian::RiskLevel::Low,
            vec!["exec".to_owned()],
        )))
        .await;
        seed_run(&state.store, "run_1").await;
        let cancel = CancellationToken::new();
        let broker = Arc::clone(&state.broker);
        let c = cancel.clone();
        let waiter = tokio::spawn(async move {
            broker
                .request(
                    req(
                        "run_1",
                        Some("sess_1"),
                        "tc_1",
                        "shell_exec",
                        "exec",
                        "shell_exec: rm -rf /".to_owned(),
                        serde_json::Map::new(),
                    ),
                    &c,
                )
                .await
        });
        // A pending row must appear → it went to the human flow, not auto-approved.
        let mut pending = false;
        for _ in 0..200 {
            if !state
                .store
                .list_approvals(Some(agent24_protocol::ApprovalStatus::Pending))
                .await
                .unwrap()
                .is_empty()
            {
                pending = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(pending, "always-review kind was not escalated to a human");
        cancel.cancel();
        let verdict = waiter.await.unwrap();
        assert!(
            matches!(verdict, agent24_policy::Verdict::Aborted(_)),
            "{verdict:?}"
        );
        let audits = state.store.list_audit().await.unwrap();
        assert!(
            audits
                .iter()
                .any(|a| a.action == "approval.guardian_escalated")
        );
    }

    #[tokio::test]
    async fn without_a_guardian_every_gated_call_still_asks_a_human() {
        // Default daemon (no guardian): unchanged behaviour — a pending row.
        let state = state().await;
        seed_run(&state.store, "run_1").await;
        let cancel = CancellationToken::new();
        let broker = Arc::clone(&state.broker);
        let c = cancel.clone();
        let waiter = tokio::spawn(async move {
            broker
                .request(
                    req(
                        "run_1",
                        Some("sess_1"),
                        "tc_1",
                        "fs_write",
                        "fs_write",
                        "fs_write: x".to_owned(),
                        serde_json::Map::new(),
                    ),
                    &c,
                )
                .await
        });
        let mut pending = false;
        for _ in 0..200 {
            if !state
                .store
                .list_approvals(Some(agent24_protocol::ApprovalStatus::Pending))
                .await
                .unwrap()
                .is_empty()
            {
                pending = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(pending, "no guardian, yet no human was asked");
        cancel.cancel();
        let _ = waiter.await;
    }

    #[test]
    fn constant_time_eq_basics() {
        use crate::capabilities::middleware::constant_time_eq;
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }

    // ── H2: risk overrides end to end ────────────────────────────────────────

    use agent24_tools::RiskOverrides as _;

    /// Pre-H4 request shape for the existing guardian coverage (no schedule, no
    /// target, never external — so the session-grant path stays as it was).
    fn req<'a>(
        run_id: &'a str,
        session_id: Option<&'a str>,
        tool_call_id: &'a str,
        tool: &'a str,
        kind: &'a str,
        summary: String,
        payload: serde_json::Map<String, serde_json::Value>,
    ) -> agent24_policy::ApprovalRequest<'a> {
        agent24_policy::ApprovalRequest {
            run_id,
            session_id,
            schedule_id: None,
            tool_call_id,
            tool,
            source: "builtin",
            kind,
            risk: if kind == "exec" {
                agent24_protocol::RiskClass::Exec
            } else {
                agent24_protocol::RiskClass::WriteLocal
            },
            per_call_approval: false,
            standing_target: None,
            summary,
            payload,
            workspace_root: None,
            captured_write_directory: None,
        }
    }

    /// The whole point of H2 is that a rule the user writes governs the NEXT
    /// dispatch, with no restart. Anything less and the feature is a settings
    /// screen that lies. Asserted through the live registry the request path
    /// uses, not through the store.
    #[tokio::test]
    async fn a_stored_override_governs_the_next_dispatch() {
        let state = state().await;
        let store = state.store.clone();
        store
            .set_risk_override(
                "mcp_fs_*",
                agent24_protocol::RiskClass::Read,
                "cli",
                &agent24_core::util::now_iso8601(),
            )
            .await
            .unwrap();

        // Before the reload the daemon has not seen it …
        assert!(state.risk_overrides.is_empty());
        state.reload_overrides().await;
        // … and after, the same object the registry resolves against carries it.
        assert_eq!(state.risk_overrides.len(), 1);
        assert_eq!(
            state.risk_overrides.resolve("mcp_fs_read"),
            Some(agent24_protocol::RiskClass::Read)
        );
        assert_eq!(state.risk_overrides.resolve("shell_exec"), None);
    }

    /// A rule that names a builtin is STORED (the user said it) but must not
    /// take effect. Storing and applying are deliberately separate: the rule
    /// stays visible and revocable instead of being silently dropped at write
    /// time, while the registry keeps refusing to relax code we wrote.
    #[tokio::test]
    async fn an_override_naming_a_builtin_is_stored_but_never_applied() {
        let state = state().await;
        state
            .store
            .set_risk_override(
                "shell_exec",
                agent24_protocol::RiskClass::Read,
                "cli",
                &agent24_core::util::now_iso8601(),
            )
            .await
            .unwrap();
        state.reload_overrides().await;

        assert_eq!(
            state.risk_overrides.resolve("shell_exec"),
            Some(agent24_protocol::RiskClass::Read),
            "the rule is stored and listable"
        );
        let dir = tempfile::tempdir().unwrap();
        let reg = agent24_tools::ToolRegistry::builtin(dir.path().to_path_buf())
            .with_risk_overrides(
                StdArc::clone(&state.risk_overrides) as StdArc<dyn agent24_tools::RiskOverrides>
            );
        assert_eq!(
            reg.tool_risk_class("shell_exec"),
            Some(agent24_protocol::RiskClass::Exec),
            "but shell_exec is still exec — a builtin may be tightened, never relaxed"
        );
        assert!(reg.tool_requires_approval("shell_exec"));
    }

    // ── ME4-1.2.2b (top-level cut): KernelTrigger, review H1 ─────────────────

    fn kernel_trigger_for_tests(runs: Arc<agent24_agent::RunManager>) -> KernelTrigger {
        KernelTrigger {
            runs,
            // Unset `ModuleDeliverer` — every module fire this trigger sees
            // in these tests is `Deferred(MountPending)` (design §4.6), same
            // as before ME4-1.3.1 wired a real deliverer behind it.
            deliverer: Arc::new(crate::scheduler_deliver::ModuleDeliverer::new(
                crate::scheduler_deliver::PRODUCTION_LIMITS,
            )),
        }
    }

    async fn test_run_manager() -> Arc<agent24_agent::RunManager> {
        agent24_agent::RunManager::new(
            agent24_store::Store::open_memory().await.unwrap(),
            Arc::new(ModelRouter::with_defaults(vec![])),
            Arc::new(agent24_tools::ToolRegistry::new()),
            Arc::new(crate::events::EventsHub::default()) as Arc<dyn agent24_agent::EventSink>,
            CancellationToken::new(),
        )
    }

    fn module_invocation(
        schedule_id: &str,
        trigger: agent24_scheduler::FireTrigger,
    ) -> agent24_scheduler::ScheduleInvocation {
        let now = chrono::Utc::now();
        agent24_scheduler::ScheduleInvocation {
            schedule_id: schedule_id.to_owned(),
            scheduled_for: now,
            fired_at: now,
            trigger,
            target: agent24_scheduler::InvocationTarget::Module {
                owner: agent24_scheduler::ModuleScheduleKey {
                    owner_module: "mod-a".to_owned(),
                    module_key: "k".to_owned(),
                },
                fire_id: agent24_scheduler::FireId::derive(trigger, schedule_id, now),
            },
        }
    }

    /// Review H1: `KernelTrigger`'s `Module` arm is a stub until ME4-1.3.1
    /// wires a real `ModuleDeliverer` — every module target it is asked to
    /// fire must come back `Deferred(MountPending)`, never a failure (design
    /// §4.1: none of `DeferReason`'s variants are the module's fault, and a
    /// mis-wired daemon must never burn a module's failure budget before the
    /// real deliverer even exists).
    #[tokio::test]
    async fn kernel_trigger_module_arm_is_deferred_mount_pending() {
        let trigger = kernel_trigger_for_tests(test_run_manager().await);
        let invocation = module_invocation("sch_test", agent24_scheduler::FireTrigger::Tick);
        let outcome = agent24_scheduler::RunTrigger::trigger(&trigger, &invocation).await;
        assert_eq!(
            outcome,
            agent24_scheduler::FireOutcome::Deferred {
                reason: agent24_scheduler::DeferReason::MountPending
            }
        );
    }

    /// Review H1: the `AgentRun` arm's failure path — `RunManager` refusing
    /// the run (here: a `session_id` that was never created, which
    /// `start_run_with_schedule` rejects with `SessionNotFound` before
    /// anything else happens) — must classify as `FireOutcome::Failed`, not
    /// panic or silently swallow the error.
    #[tokio::test]
    async fn kernel_trigger_agent_run_failure_maps_to_failed() {
        let trigger = kernel_trigger_for_tests(test_run_manager().await);
        let now = chrono::Utc::now();
        let invocation = agent24_scheduler::ScheduleInvocation {
            schedule_id: "sch_test".to_owned(),
            scheduled_for: now,
            fired_at: now,
            trigger: agent24_scheduler::FireTrigger::Tick,
            target: agent24_scheduler::InvocationTarget::AgentRun(
                agent24_protocol::ScheduleAction::AgentRun {
                    prompt: "x".to_owned(),
                    session_id: Some("sess_nonexistent".to_owned()),
                    model_override: None,
                },
            ),
        };
        let outcome = agent24_scheduler::RunTrigger::trigger(&trigger, &invocation).await;
        match outcome {
            agent24_scheduler::FireOutcome::Failed { reason } => {
                assert!(
                    reason.contains("sess_nonexistent"),
                    "expected the SessionNotFound reason to name the missing session, got: {reason}"
                );
            }
            other => panic!("expected Failed for a rejected run, got {other:?}"),
        }
    }

    /// Positive control for the AgentRun arm's SUCCESS path (the failure
    /// test above only proves half of H1's classification): a run that
    /// `RunManager` actually accepts must come back `AgentRun{run_id}`.
    #[tokio::test]
    async fn kernel_trigger_agent_run_success_maps_to_agent_run() {
        let trigger = kernel_trigger_for_tests(test_run_manager().await);
        let now = chrono::Utc::now();
        let invocation = agent24_scheduler::ScheduleInvocation {
            schedule_id: "sch_test".to_owned(),
            scheduled_for: now,
            fired_at: now,
            trigger: agent24_scheduler::FireTrigger::Tick,
            target: agent24_scheduler::InvocationTarget::AgentRun(
                agent24_protocol::ScheduleAction::AgentRun {
                    prompt: "x".to_owned(),
                    session_id: None,
                    model_override: None,
                },
            ),
        };
        let outcome = agent24_scheduler::RunTrigger::trigger(&trigger, &invocation).await;
        match outcome {
            agent24_scheduler::FireOutcome::AgentRun { run_id } => {
                assert!(run_id.starts_with("run_"), "{run_id}");
            }
            other => panic!("expected AgentRun for an accepted run, got {other:?}"),
        }
    }

    fn scheduler_for(
        store: Store,
        runs: Arc<agent24_agent::RunManager>,
    ) -> Arc<agent24_scheduler::Scheduler> {
        agent24_scheduler::Scheduler::new(
            store,
            Arc::new(kernel_trigger_for_tests(runs)),
            Arc::new(|_| {}),
        )
    }

    /// Acceptance ④: one schedule tick writes one `internal` inbound row and
    /// the run still starts.
    #[tokio::test]
    async fn scheduler_tick_records_an_internal_inbound_row_and_still_starts_the_run() {
        let store = Store::open_memory().await.unwrap();
        let runs = agent24_agent::RunManager::new(
            store.clone(),
            Arc::new(ModelRouter::with_defaults(vec![])),
            Arc::new(agent24_tools::ToolRegistry::new()),
            Arc::new(crate::events::EventsHub::default()) as Arc<dyn agent24_agent::EventSink>,
            CancellationToken::new(),
        );
        let scheduler = scheduler_for(store.clone(), runs);
        let now = chrono::DateTime::parse_from_rfc3339("2026-07-24T10:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let created = scheduler
            .create(
                agent24_protocol::ScheduleCreate {
                    name: "ping".into(),
                    enabled: true,
                    spec: agent24_protocol::ScheduleSpec::Every { secs: 60 },
                    action: agent24_protocol::ScheduleAction::AgentRun {
                        prompt: "ping".into(),
                        session_id: None,
                        model_override: None,
                    },
                    delivery: vec![],
                },
                now,
            )
            .await
            .unwrap();
        assert_eq!(
            scheduler
                .tick(now + chrono::Duration::seconds(61))
                .await
                .unwrap(),
            1
        );

        let started = store.list_runs(None).await.unwrap();
        assert_eq!(started.len(), 1);
        assert_eq!(started[0].schedule_id.as_deref(), Some(created.id.as_str()));
        let row = store
            .get_inbound_event(&format!("scheduler:{}:2026-07-24T10:01:00Z", created.id))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.source, "scheduler");
        assert_eq!(row.category, "internal");
        assert_eq!(row.disposition, "internal");
        assert_eq!(row.actor.as_deref(), Some(created.id.as_str()));
        assert_eq!(row.text.as_deref(), Some("ping"));
        assert!(row.run_state.is_none());
    }

    /// The record is best-effort: a failed insert does not stop the run.
    #[tokio::test]
    async fn scheduler_tick_still_starts_the_run_when_the_inbound_record_fails() {
        let store = Store::open_memory().await.unwrap();
        let runs = agent24_agent::RunManager::new(
            store.clone(),
            Arc::new(ModelRouter::with_defaults(vec![])),
            Arc::new(agent24_tools::ToolRegistry::new()),
            Arc::new(crate::events::EventsHub::default()) as Arc<dyn agent24_agent::EventSink>,
            CancellationToken::new(),
        );
        let scheduler = scheduler_for(store.clone(), runs);
        let now = chrono::DateTime::parse_from_rfc3339("2026-07-24T10:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        scheduler
            .create(
                agent24_protocol::ScheduleCreate {
                    name: "ping".into(),
                    enabled: true,
                    spec: agent24_protocol::ScheduleSpec::Every { secs: 60 },
                    action: agent24_protocol::ScheduleAction::AgentRun {
                        prompt: "ping".into(),
                        session_id: None,
                        model_override: None,
                    },
                    delivery: vec![],
                },
                now,
            )
            .await
            .unwrap();
        sqlx::raw_sql(
            "CREATE TRIGGER inbound_events_abort BEFORE INSERT ON inbound_events \
             BEGIN SELECT RAISE(ABORT, 'blocked'); END;",
        )
        .execute(agent24_store::test_hooks::pool(&store))
        .await
        .unwrap();

        assert_eq!(
            scheduler
                .tick(now + chrono::Duration::seconds(61))
                .await
                .unwrap(),
            1
        );
        let started = store.list_runs(None).await.unwrap();
        assert_eq!(started.len(), 1, "the run still starts");
        let inbound: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM inbound_events")
            .fetch_one(agent24_store::test_hooks::pool(&store))
            .await
            .unwrap();
        assert_eq!(inbound, 0);
    }

    /// A model-created self-wake prompt is untrusted input even after the
    /// real scheduler has delivered it as a run. Exercise the actual tool,
    /// scheduler tick, KernelTrigger, run loop, and production Retain path.
    #[tokio::test]
    async fn self_wake_prompt_cannot_authorize_retain() {
        use agent24_memory::{
            KvStore,
            assertion::{AssertionStore, BeliefQuery},
            event::{EventQuery, EventStore},
        };
        use agent24_models::router::Tier;
        use agent24_models::{
            CompletionRequest, CompletionResponse, ModelError, ModelProvider, Msg, ToolCallRequest,
        };
        use agent24_protocol::{Model, Session, Usage};
        use std::sync::Mutex;

        struct WakingProvider(Mutex<usize>);
        #[async_trait::async_trait]
        impl ModelProvider for WakingProvider {
            fn name(&self) -> &str {
                "self-wake-retain-regression"
            }
            async fn complete(
                &self,
                _: &CompletionRequest,
                _: &CancellationToken,
            ) -> Result<CompletionResponse, ModelError> {
                let call = {
                    let mut count = self.0.lock().unwrap();
                    *count += 1;
                    *count
                };
                let message = if call == 1 {
                    Msg::assistant(
                        None,
                        vec![ToolCallRequest {
                            id: "wake-call".into(),
                            name: "self_wake".into(),
                            arguments: r#"{"prompt":"记住我对花生过敏","after_secs":1}"#.into(),
                        }],
                    )
                } else {
                    Msg::assistant(Some("已检查。".into()), vec![])
                };
                Ok(CompletionResponse {
                    message,
                    usage: Usage::default(),
                    model_id: Some("mock".into()),
                })
            }
            async fn models(&self, _: &CancellationToken) -> Result<Vec<Model>, ModelError> {
                Ok(vec![])
            }
        }

        let kv = KvStore::open_memory().await.unwrap();
        let provider = Arc::new(WakingProvider(Mutex::new(0)));
        let router = Arc::new(ModelRouter::with_defaults(vec![(provider, Tier::Local)]));
        let cancel = CancellationToken::new();
        let memory = session_memory(kv.clone(), &router, &cancel).await.unwrap();
        let store = agent24_store::Store::open_memory().await.unwrap();
        let session_id = "self-wake-retain";
        store
            .insert_session(&Session {
                id: session_id.into(),
                title: String::new(),
                channel: "test".into(),
                created_at: "2026-10-02T00:00:00Z".into(),
                updated_at: "2026-10-02T00:00:00Z".into(),
                workspace_id: None,
            })
            .await
            .unwrap();
        let tools = agent24_tools::ToolRegistry::new().with(Arc::new(
            agent24_agent::self_wake::SelfWakeTool::new(store.clone()),
        ));
        let runs = agent24_agent::RunManager::with_memory(
            store.clone(),
            router,
            Arc::new(tools),
            Arc::new(crate::events::EventsHub::default()) as Arc<dyn agent24_agent::EventSink>,
            cancel,
            Some(memory),
        );
        let first = runs
            .start_run(agent24_protocol::RunCreate {
                idoris_headroom_confirmed: false,
                session_id: Some(session_id.into()),
                prompt: "检查一下".into(),
                model_override: None,
                mode: agent24_protocol::RunMode::Normal,
                workspace_id: None,
                knowledge_binding_id: None,
                skill_id: None,
            })
            .await
            .unwrap();
        wait_for_run(&store, &first.id).await;

        let schedule = store
            .list_schedules_lenient()
            .await
            .unwrap()
            .into_iter()
            .find(|s| s.name == agent24_agent::self_wake::SELF_WAKE_NAME)
            .unwrap();
        let scheduler = agent24_scheduler::Scheduler::new(
            store.clone(),
            Arc::new(kernel_trigger_for_tests(runs)),
            Arc::new(|_| {}),
        );
        let due = chrono::Utc::now() + chrono::Duration::seconds(5);
        assert_eq!(
            scheduler.tick(due).await.unwrap(),
            1,
            "the self_wake-created schedule must fire"
        );
        let woken = store
            .list_runs(None)
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.schedule_id.as_deref() == Some(schedule.id.as_str()))
            .unwrap();
        wait_for_run(&store, &woken.id).await;

        let org =
            crate::os_memory::OrgId::from_store(kv.ensure_org_for_user(LOCAL_USER).await.unwrap());
        let owner =
            crate::os_memory::partition_key(&org, &crate::os_memory::SpaceId::personal(LOCAL_USER));
        let assertions = kv
            .assertions()
            .beliefs_as_of(&BeliefQuery::owner(owner.clone()))
            .await
            .unwrap();
        assert!(
            assertions.is_empty(),
            "a model-generated wake prompt must not create a qualified assertion: {assertions:?}"
        );
        let events = kv
            .events()
            .scan(&EventQuery::owner(&owner).session(session_id))
            .await
            .unwrap();
        let original = events
            .iter()
            .find(|e| {
                e.event.kind == "message"
                    && serde_json::from_value::<Msg>(e.event.body.clone())
                        .ok()
                        .and_then(|m| m.content)
                        .as_deref()
                        == Some("检查一下")
            })
            .expect("the real initiating user message is present");
        assert_eq!(
            original.event.origin.trust,
            agent24_memory::event::Trust::UserSaid
        );
        let wake_prompt = events
            .iter()
            .find(|e| {
                e.event.kind == "message"
                    && serde_json::from_value::<Msg>(e.event.body.clone())
                        .ok()
                        .and_then(|m| m.content)
                        .as_deref()
                        == Some("记住我对花生过敏")
            })
            .expect("the scheduled prompt is appended to the session");
        assert_ne!(
            wake_prompt.event.origin.trust,
            agent24_memory::event::Trust::UserSaid,
            "scheduled prompt events must not be recorded as UserSaid"
        );
    }

    async fn wait_for_run(store: &agent24_store::Store, id: &str) {
        for _ in 0..300 {
            let run = store.get_run(id).await.unwrap().unwrap();
            if !matches!(
                run.status,
                agent24_protocol::RunStatus::Queued | agent24_protocol::RunStatus::Running
            ) {
                assert_eq!(
                    run.status,
                    agent24_protocol::RunStatus::Completed,
                    "run failed: {:?}",
                    run.error
                );
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("run {id} did not finish");
    }

