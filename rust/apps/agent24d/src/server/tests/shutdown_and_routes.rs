    /// A disable's stop is cut off at a fixed instant after the shutdown
    /// began — the budget any module gets — however late its waiter is first
    /// polled (review of SUP-5, round 2). The token is cancelled directly:
    /// `request` would arm the watchdog, which ends this process.
    #[tokio::test(start_paused = true)]
    async fn a_disables_stop_is_cut_off_at_a_fixed_instant_after_the_shutdown_began() {
        let shutdown = Shutdown::new(CancellationToken::new());
        let began = tokio::time::Instant::now();
        let _ = shutdown.deadlines();
        shutdown.token().cancel();
        tokio::time::sleep(Duration::from_millis(250)).await;
        shutdown.modules_cut_off().await;
        let p = crate::lifecycle::Params::default();
        assert_eq!(began.elapsed(), p.drain + p.stop_grace);
    }

    /// In-flight model calls (and the usage recorder) are cut off at the drain plus the 2s
    /// call-cancel grace, however long the stop grace is: attached modules get no SIGTERM,
    /// so the 15s a package gets to flush must not keep their calls alive (ADR-038 D5/D6).
    #[tokio::test(start_paused = true)]
    async fn model_calls_are_cut_off_by_the_cancel_grace_not_the_stop_grace() {
        let shutdown = Shutdown::new(CancellationToken::new());
        let began = tokio::time::Instant::now();
        let _ = shutdown.deadlines();
        shutdown.token().cancel();
        shutdown.calls_cut_off().await;
        let p = crate::lifecycle::Params::default();
        assert_eq!(
            began.elapsed(),
            p.drain + crate::lifecycle::CALL_CANCEL_GRACE
        );
        assert!(
            began.elapsed() < p.drain + p.stop_grace,
            "not the stop grace"
        );
        // a stop grace shorter than the cancel grace still ends it first
        let short = crate::lifecycle::Params {
            drain: Duration::from_millis(100),
            stop_grace: Duration::from_millis(500),
        };
        assert_eq!(
            short.deadlines(began).calls - began,
            Duration::from_millis(600)
        );
    }

    /// The shutdown waits for the stops `os disable` began, not only for the
    /// modules it stops itself (SUP-5): those end when the shutdown's module
    /// budget cuts them short, and a daemon that exited first would leave the
    /// kill unsent.
    #[tokio::test]
    async fn the_shutdown_waits_for_the_stops_disables_began() {
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop = {
            let done = done.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(100)).await;
                done.store(true, std::sync::atomic::Ordering::SeqCst);
            })
        };
        stop_supervisors(
            crate::domain::Closed {
                running: Vec::new(),
                disabling: vec![crate::domain::Disabling {
                    name: "x".into(),
                    record: agent24_os_proto::stop_record::StopRecordHandle::default(),
                    task: stop,
                }],
            },
            Duration::ZERO,
        )
        .await;
        assert!(done.load(std::sync::atomic::Ordering::SeqCst));
    }

    /// No provider answered — the honest default for a test daemon with no
    /// providers configured. Modules under test declare no models, so the check
    /// is Satisfied regardless; ME-2's resource cases have their own tests.
    struct NoModels;
    impl crate::domain::ModelInventory for NoModels {
        fn available(&self) -> std::result::Result<&[String], String> {
            Err("no providers in tests".to_owned())
        }
    }

    /// A daemon state wired for tests. `pub(crate)` so the mounter's tests in
    /// `domain.rs` can merge a module into the REAL kernel router — testing the
    /// degraded 503 against a stub router would not show which fallback wins.
    pub(crate) async fn state() -> AppState {
        state_with_guardian(None).await
    }

    pub(crate) async fn state_with_router(router: Arc<ModelRouter>) -> AppState {
        state_with_guardian_and_router(None, router).await
    }

    async fn state_with_guardian(
        guardian: Option<StdArc<agent24_policy::guardian::Guardian>>,
    ) -> AppState {
        state_with_guardian_and_router(guardian, Arc::new(ModelRouter::with_defaults(vec![]))).await
    }

    async fn state_with_guardian_and_router(
        guardian: Option<StdArc<agent24_policy::guardian::Guardian>>,
        router: Arc<ModelRouter>,
    ) -> AppState {
        AppState::new(AppDeps {
            token: "testtoken".to_owned(),
            router,
            tools: agent24_tools::ToolRegistry::new(),
            store: Store::open_memory().await.unwrap(),
            shutdown: Shutdown::new(CancellationToken::new()),
            guardian,
            memory: None,
            workspace_service: None,
            mcp_servers: Vec::new(),
            risk_overrides: StdArc::new(agent24_policy::overrides::RiskOverrideStore::from_rows(
                Vec::new(),
            )),
            // A real (if immediately-dropped) temp directory, not an empty
            // path standing in for one — T8/ME-3g's admission gate never
            // reaches for this unless a test explicitly populates
            // `package_dirs`, so nothing here actually gets read by default.
            packages_root: Arc::new(tempfile::tempdir().expect("tempdir").path().to_path_buf()),
        })
    }

    /// A guardian whose assessor always returns the given verdict — lets us test
    /// the daemon's wiring without a live model.
    struct StubAssessor(agent24_policy::guardian::RiskLevel);

    #[async_trait::async_trait]
    impl agent24_policy::guardian::RiskAssessor for StubAssessor {
        async fn assess(
            &self,
            _input: &agent24_policy::guardian::AssessInput<'_>,
            _cancel: &CancellationToken,
        ) -> Result<agent24_policy::guardian::RiskAssessment, agent24_policy::guardian::AssessError>
        {
            Ok(agent24_policy::guardian::RiskAssessment {
                level: self.0,
                rationale: "stub".to_owned(),
            })
        }
    }

    fn stub_guardian(
        level: agent24_policy::guardian::RiskLevel,
        always_review: Vec<String>,
    ) -> StdArc<agent24_policy::guardian::Guardian> {
        StdArc::new(
            agent24_policy::guardian::Guardian::new(StdArc::new(StubAssessor(level)))
                .always_review(always_review),
        )
    }

    async fn body_json(res: Response) -> serde_json::Value {
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[test]
    fn idoris_shutdown_budget_includes_drain_and_frozen_term_grace() {
        let shutdown = Shutdown::new(CancellationToken::new());
        shutdown
            .idoris_budget
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let deadlines = shutdown.deadlines();
        assert!(deadlines.persist - deadlines.began >= std::time::Duration::from_secs(50));
        assert!(deadlines.watchdog > deadlines.persist);
        assert!(deadlines.modules < deadlines.persist);
    }

    pub(crate) async fn capability_state() -> (AppState, String, String) {
        let mut state = state().await;
        let now = crate::capabilities::unix_now();
        let store = crate::capabilities::CapabilityStore::new("daemon-test");
        let host = store
            .mint_product_host("host-test", Duration::from_secs(3_600), now)
            .unwrap();
        let (host_bearer, _, _) = host.into_bearer_parts();
        let host_authorization = store
            .validate_bearer(
                &host_bearer,
                crate::capabilities::Operation::ModelsRead,
                &crate::capabilities::Resource::global(),
                now,
            )
            .unwrap();
        let creative = store
            .mint_creative_authorized(
                &host_authorization,
                crate::capabilities::CreativeMintRequest::new(
                    "workspace-test",
                    "attachment-test",
                    "principal-test",
                    "sidecar-test",
                    Duration::from_secs(300),
                    now,
                ),
            )
            .unwrap();
        let (creative_bearer, _, _) = creative.into_bearer_parts();
        state.enable_capability_auth(store);
        state.enable_desktop_presence();
        (state, host_bearer, creative_bearer)
    }

    #[tokio::test]
    async fn component_install_requires_host_authority_before_dispatch() {
        let (state, host, creative) = capability_state().await;
        let modules = Router::new().route(
            "/api/v1/comm/install",
            axum::routing::post(|| async { StatusCode::NO_CONTENT }),
        );
        let app = build_router_with_modules(state, modules);
        for (bearer, expected) in [
            (None, StatusCode::UNAUTHORIZED),
            (Some(creative), StatusCode::FORBIDDEN),
            (Some(host), StatusCode::NO_CONTENT),
        ] {
            let mut req = Request::builder()
                .method(Method::POST)
                .uri("/api/v1/comm/install");
            if let Some(bearer) = bearer {
                req = req.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
            }
            let res = app
                .clone()
                .oneshot(req.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(res.status(), expected);
        }
    }

    #[tokio::test]
    async fn key_backup_is_product_host_only_in_capabilities_mode() {
        let (state, host, creative) = capability_state().await;
        let modules = Router::new()
            .route(
                "/api/v1/comm/identity/export",
                axum::routing::post(|| async { StatusCode::NO_CONTENT }),
            )
            .route(
                "/api/v1/comm/identity/import",
                axum::routing::post(|| async { StatusCode::NO_CONTENT }),
            );
        let app = build_router_with_modules(state, modules);
        for (uri, bearer, expected) in [
            (
                "/api/v1/comm/identity/export",
                None,
                StatusCode::UNAUTHORIZED,
            ),
            (
                "/api/v1/comm/identity/export",
                Some(creative.clone()),
                StatusCode::FORBIDDEN,
            ),
            (
                "/api/v1/comm/identity/export",
                Some(host.clone()),
                StatusCode::NO_CONTENT,
            ),
            (
                "/api/v1/comm/identity/import",
                None,
                StatusCode::UNAUTHORIZED,
            ),
            (
                "/api/v1/comm/identity/import",
                Some(creative),
                StatusCode::FORBIDDEN,
            ),
            (
                "/api/v1/comm/identity/import",
                Some(host),
                StatusCode::NO_CONTENT,
            ),
        ] {
            let mut req = Request::builder().method(Method::POST).uri(uri);
            if let Some(bearer) = bearer {
                req = req.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
            }
            let res = app
                .clone()
                .oneshot(req.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(res.status(), expected);
        }
    }

    /// Loop 9 3c: the Lite's embed-only iDoris key goes to the desktop's
    /// ProductHost only; the host passes auth (no iDoris host here: 422).
    #[tokio::test]
    async fn lite_embedding_key_is_product_host_only() {
        let (capabilities, host, creative) = capability_state().await;
        let app = build_router(capabilities);
        for (bearer, expected) in [
            (None, StatusCode::UNAUTHORIZED),
            (Some(creative), StatusCode::FORBIDDEN),
            (Some(host), StatusCode::UNPROCESSABLE_ENTITY),
        ] {
            let mut req = Request::builder()
                .method(Method::POST)
                .uri("/api/v1/idoris/lite-embedding");
            if let Some(bearer) = bearer {
                req = req.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
            }
            let res = app
                .clone()
                .oneshot(req.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(res.status(), expected);
        }
        let legacy = build_router(state().await)
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/idoris/lite-embedding")
                    .header(header::AUTHORIZATION, "Bearer testtoken")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(legacy.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn key_backup_is_refused_in_legacy_mode_even_with_the_right_token() {
        let modules = Router::new()
            .route(
                "/api/v1/comm/identity/export",
                axum::routing::post(|| async { StatusCode::NO_CONTENT }),
            )
            .route(
                "/api/v1/comm/identity/import",
                axum::routing::post(|| async { StatusCode::NO_CONTENT }),
            )
            .route(
                "/api/v1/comm/contact",
                axum::routing::post(|| async { StatusCode::NO_CONTENT }),
            );
        let app = build_router_with_modules(state().await, modules);
        for (uri, expected) in [
            ("/api/v1/comm/identity/export", StatusCode::FORBIDDEN),
            ("/api/v1/comm/identity/import", StatusCode::FORBIDDEN),
            ("/api/v1/comm/contact", StatusCode::NO_CONTENT), // other routes unchanged
        ] {
            let res = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri(uri)
                        .header(header::AUTHORIZATION, "Bearer testtoken")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(res.status(), expected, "{uri}");
        }
    }

    /// Through the REAL comm router (nested exactly as production nests it),
    /// not a stub handler: the restriction must hold on the actual route.
    #[tokio::test]
    async fn key_backup_restriction_holds_on_the_real_comm_router() {
        // Export names a path; restore carries the backup text itself (the
        // desktop reads the file, the daemon never opens a caller-named path).
        let body = |uri: &str| {
            Body::from(if uri.ends_with("/export") {
                r#"{"nickname":"a","path":"/tmp/x.ncryptsec","backup_password":"long-enough-1"}"#
            } else {
                r#"{"nickname":"a","ncryptsec":"ncryptsec1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq","backup_password":"long-enough-1"}"#
            })
        };
        for uri in [
            "/api/v1/comm/identity/export",
            "/api/v1/comm/identity/import",
        ] {
            let post = |bearer: &str| {
                Request::builder()
                    .method(Method::POST)
                    .uri(uri)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                    .body(body(uri))
                    .unwrap()
            };
            let dir = tempfile::tempdir().unwrap();
            let slot = Arc::new(crate::comm_routes::CommDaemonSlot::new());

            // legacy mode, correct token: refused
            let comm =
                crate::comm_routes::build(dir.path(), Duration::from_millis(500), &slot).await;
            let legacy = build_router_with_modules(state().await, comm);
            let res = legacy.oneshot(post("testtoken")).await.unwrap();
            assert_eq!(res.status(), StatusCode::FORBIDDEN);

            // capabilities mode: Creative refused, ProductHost reaches the handler
            // (no Hyphae binary here, so it answers its own not_configured)
            let (state, host, creative) = capability_state().await;
            let comm =
                crate::comm_routes::build(dir.path(), Duration::from_millis(500), &slot).await;
            let app = build_router_with_modules(state, comm);
            let res = app.clone().oneshot(post(&creative)).await.unwrap();
            assert_eq!(res.status(), StatusCode::FORBIDDEN);
            let res = app.oneshot(post(&host)).await.unwrap();
            assert_ne!(res.status(), StatusCode::FORBIDDEN);
            assert_eq!(res.status(), StatusCode::CONFLICT);
        }
    }

    #[tokio::test]
    async fn hot_comm_router_preserves_core2_matched_templates() {
        let (state, host, _) = capability_state().await;
        let dir = tempfile::tempdir().unwrap();
        let slot = Arc::new(crate::comm_routes::CommDaemonSlot::new());
        let comm = crate::comm_routes::build(dir.path(), Duration::from_millis(500), &slot).await;
        let app = build_router_with_modules(state, comm);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/comm/identity")
                    .header(header::AUTHORIZATION, format!("Bearer {host}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(body_json(response).await["error"], "not_configured");
    }

    #[tokio::test]
    async fn health_needs_no_token() {
        let res = build_router(state().await)
            .oneshot(
                Request::builder()
                    .uri("/api/v1/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let json = body_json(res).await;
        assert_eq!(json["status"], "ok");
        assert_eq!(json["backend"], "rust");
        assert!(json["version"].as_str().is_some());
    }

    #[tokio::test]
    async fn health_reports_protocol_version_and_applied_migrations() {
        let mut app = state().await;
        let memory = agent24_memory::KvStore::open_memory().await.unwrap();
        let store_applied: Option<i64> =
            sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations WHERE success = 1")
                .fetch_one(agent24_store::test_hooks::pool(&app.store))
                .await
                .unwrap();
        let memory_applied: Option<i64> =
            sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations WHERE success = 1")
                .fetch_one(agent24_memory::test_hooks::pool(&memory))
                .await
                .unwrap();
        let store_applied = store_applied.unwrap();
        let memory_applied = memory_applied.unwrap();
        assert!(store_applied > 0, "run database has no applied migration");
        assert!(
            memory_applied > 0,
            "memory database has no applied migration"
        );
        app.memory_kv = Some(memory);

        let res = build_router(app)
            .oneshot(
                Request::builder()
                    .uri("/api/v1/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let json = body_json(res).await;
        assert_eq!(json["status"], "ok");
        assert_eq!(json["backend"], "rust");
        assert!(json["version"].as_str().is_some());
        assert_eq!(
            json["protocol_version"].as_u64(),
            Some(u64::from(agent24_protocol::PROTOCOL_VERSION))
        );
        assert_eq!(json["migrations"]["store"].as_i64(), Some(store_applied));
        assert_eq!(json["migrations"]["memory"].as_i64(), Some(memory_applied));
    }

    #[tokio::test]
    async fn module_consent_listing_route_is_available_to_the_host() {
        let router = build_router(state().await);
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/api/v1/module-consents")
                    .header("Authorization", "Bearer testtoken")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert!(body["authorizations"].is_array());
    }

    #[tokio::test]
    async fn module_consent_export_contains_metadata_only_and_revoke_broadcasts() {
        let state = state().await;
        let summary = agent24_store::ToolPermissionSummary::new(
            "doc-module",
            "read_document",
            "1.2.3",
            agent24_store::ConsentSource::FirstParty,
            Some("local documents".to_owned()),
            None,
            Some("https://example.invalid".to_owned()),
            agent24_store::HostRiskLevel::Medium,
        );
        state
            .store
            .grant_module_consent(&summary, "2026-10-08T00:00:00Z", "2099-01-01T00:00:00Z")
            .await
            .unwrap();
        let second_summary = agent24_store::ToolPermissionSummary::new(
            "doc-module",
            "write_document",
            "1.2.3",
            agent24_store::ConsentSource::FirstParty,
            None,
            Some("local documents".to_owned()),
            None,
            agent24_store::HostRiskLevel::Medium,
        );
        state
            .store
            .grant_module_consent(
                &second_summary,
                "2026-10-08T00:00:00Z",
                "2099-01-01T00:00:00Z",
            )
            .await
            .unwrap();
        let mut notifications = state.store.subscribe_module_consent_revocations();
        let router = build_router(state);

        let exported = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/module-consents/export")
                    .header("Authorization", "Bearer testtoken")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(exported.status(), StatusCode::OK);
        assert_eq!(
            exported.headers()[header::CONTENT_DISPOSITION],
            "attachment; filename=module-authorizations.json"
        );
        let bytes = exported.into_body().collect().await.unwrap().to_bytes();
        let serialized = String::from_utf8(bytes.to_vec()).unwrap();
        let export: serde_json::Value = serde_json::from_str(&serialized).unwrap();
        let authorization = &export["authorizations"][0];
        assert_eq!(authorization["module"], "doc-module");
        assert_eq!(authorization["tool"], "read_document");
        assert_eq!(
            authorization["scope_summary"]["readable"],
            "local documents"
        );
        // No module is mounted here and the grant names no package: the gate
        // would refuse it, so the export says stale too (#1290).
        assert_eq!(authorization["status"], "stale");
        for forbidden in ["body", "content", "credential", "token", "password"] {
            assert!(!serialized.to_lowercase().contains(forbidden));
        }

        let revoked = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri("/api/v1/module-consents/modules/doc-module/tools/read_document")
                    .header("Authorization", "Bearer testtoken")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(revoked.status(), StatusCode::OK);
        let notice = notifications.recv().await.unwrap();
        assert_eq!(notice.module, "doc-module");
        assert_eq!(notice.op.as_deref(), Some("read_document"));

        let revoked = router
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri("/api/v1/module-consents/modules/doc-module")
                    .header("Authorization", "Bearer testtoken")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(revoked.status(), StatusCode::OK);
        let notice = notifications.recv().await.unwrap();
        assert_eq!(notice.module, "doc-module");
        assert_eq!(notice.op, None);
    }

    /// K1-6a-G §2.1: a card shows the summary the gate compares a grant
    /// with — its fingerprint, version and the running package's identity —
    /// and there is none for a tool that does not exist or a module with no
    /// running package.
    #[tokio::test]
    async fn a_module_tool_card_shows_what_a_grant_would_be_checked_against() {
        let none_assembled = build_router(state().await)
            .oneshot(
                Request::builder()
                    .uri("/api/v1/module-consents/modules/doc-module/tools/read_document/card")
                    .header("Authorization", "Bearer testtoken")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(none_assembled.status(), StatusCode::NOT_FOUND);
        let mut state = state().await;
        let mut summary = agent24_store::ToolPermissionSummary::new(
            "doc-module",
            "read_document",
            "1.2.3",
            agent24_store::ConsentSource::ManualInstall,
            Some("local documents".to_owned()),
            None,
            None,
            agent24_store::HostRiskLevel::High,
        );
        summary.reversible_draft = true;
        let running = StdArc::new(std::sync::atomic::AtomicBool::new(true));
        let source = |takes_grants| crate::consent_routes::ToolCardSource {
            summary: summary.clone(),
            resource_kinds: vec!["doc".to_owned()],
            takes_grants,
            host_read: false,
        };
        let gate = source(true).gate_request();
        let mut old = summary.clone();
        old.module = "old-module".to_owned();
        state.module_tool_cards = Some(crate::consent_routes::ModuleToolCards {
            notice: answering(crate::module_tool_runtime::RevocationStop::NotRunning),
            tools: StdArc::new(std::collections::HashMap::from([
                (
                    ("doc-module".to_owned(), "read_document".to_owned()),
                    source(true),
                ),
                (
                    ("old-module".to_owned(), "read_document".to_owned()),
                    crate::consent_routes::ToolCardSource {
                        summary: old,
                        ..source(false)
                    },
                ),
            ])),
            artifact: {
                let running = StdArc::clone(&running);
                StdArc::new(move |_: &str| {
                    running
                        .load(std::sync::atomic::Ordering::SeqCst)
                        .then(|| "sha256:abc".to_owned())
                })
            },
        });
        let router = build_router(state);
        let get = |uri: &'static str| {
            let router = router.clone();
            async move {
                router
                    .oneshot(
                        Request::builder()
                            .uri(uri)
                            .header("Authorization", "Bearer testtoken")
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap()
            }
        };
        let card = "/api/v1/module-consents/modules/doc-module/tools/read_document/card";
        let response = get(card).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["fingerprint"], gate.current_scope_fingerprint);
        assert_eq!(body["module_version"], gate.module_version);
        assert_eq!(body["module_version"], "1.2.3");
        assert_eq!(body["artifact"], "sha256:abc");
        assert_eq!(body["resource_kinds"], serde_json::json!(["doc"]));
        assert_eq!(body["host_risk"], "high");
        assert_eq!(body["source"], "manual_install");
        assert_eq!(body["scope_summary"]["readable"], "local documents");
        // What the manifest declares, shown as declared; the gate still
        // grants no draft exception on this path (`gate_request`).
        assert_eq!(body["reversible_draft"], true);
        assert!(!gate.reversible_draft_declared);
        let response =
            get("/api/v1/module-consents/modules/doc-module/tools/write_document/card").await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let response =
            get("/api/v1/module-consents/modules/old-module/tools/read_document/card").await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(
            body_json(response).await["error"]["code"],
            "protocol_too_old"
        );
        running.store(false, std::sync::atomic::Ordering::SeqCst);
        let response = get(card).await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(
            body_json(response).await["error"]["code"],
            "no_package_identity"
        );
    }

