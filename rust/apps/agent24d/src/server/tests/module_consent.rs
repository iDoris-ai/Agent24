    /// A revocation notice that answers `stop` and keeps what it was told.
    fn answering_into(
        stop: crate::module_tool_runtime::RevocationStop,
        told: StdArc<std::sync::Mutex<Vec<crate::module_tool_runtime::GrantEnd>>>,
    ) -> crate::consent_routes::RevocationNotice {
        StdArc::new(move |end| {
            told.lock().unwrap().push(end);
            Box::pin(async move { stop }) as futures::future::BoxFuture<'static, _>
        })
    }

    fn answering(
        stop: crate::module_tool_runtime::RevocationStop,
    ) -> crate::consent_routes::RevocationNotice {
        answering_into(stop, StdArc::default())
    }

    /// K1-6a-G §2.5: a revocation tells the module and answers with what it
    /// said; the list shows it for each revoked grant, and a revocation no
    /// answer was recorded for — or one replaced since — reads unconfirmed,
    /// as does one the module did not confirm, which is audited too. It
    /// tells the grants it ended (§2.5.1), and answers its outbox row.
    #[tokio::test]
    async fn a_revocation_answers_whether_the_module_stopped() {
        use crate::module_tool_runtime::RevocationStop::{Confirmed, NotRunning, Unconfirmed};
        let dir = tempfile::tempdir().unwrap();
        let running = StdArc::new(std::sync::atomic::AtomicBool::new(true));
        let (state, summary) = decision_state(&running, &dir.path().join("os.json")).await;
        let store = state.store.clone();
        let told = StdArc::new(std::sync::Mutex::new(Vec::new()));
        let with = |stop| {
            let mut state = state.clone();
            let cards = state.module_tool_cards.as_mut().unwrap();
            cards.notice = answering_into(stop, StdArc::clone(&told));
            build_router(state)
        };
        let delete = |router: Router, uri: &'static str| async move {
            let response = router
                .oneshot(
                    Request::builder()
                        .method("DELETE")
                        .uri(uri)
                        .header("Authorization", "Bearer testtoken")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            body_json(response).await["stop"].clone()
        };
        let stop_listed = |store: agent24_store::Store| async move {
            let views = authorization_list(store).await;
            views["authorizations"][0]["stop"].clone()
        };
        let grant =
            || store.grant_module_consent(&summary, "2026-10-24T00:00:00Z", "2099-01-01T00:00:00Z");
        let granted = grant().await.unwrap();
        assert_eq!(delete(with(Confirmed), DECIDE).await, "confirmed");
        let unconfirmed = |store: agent24_store::Store| async move {
            store
                .list_audit()
                .await
                .unwrap()
                .iter()
                .filter(|e| e.action == "module_consent.stop_unconfirmed")
                .count()
        };
        assert_eq!(
            unconfirmed(store.clone()).await,
            0,
            "a confirmed stop is not audited"
        );
        let told_end = |operation: &str, refs: Vec<String>| {
            Some(crate::module_tool_runtime::GrantEnd {
                module: "doc-module".to_owned(),
                operation: operation.to_owned(),
                ended_at: String::new(),
                reason: "revoked",
                authorization_refs: Some(refs),
            })
        };
        let pop = || {
            told.lock()
                .unwrap()
                .pop()
                .map(|end| crate::module_tool_runtime::GrantEnd {
                    ended_at: String::new(),
                    ..end
                })
        };
        assert_eq!(
            pop(),
            told_end("read_document", vec![granted.authorization_ref()])
        );
        assert_eq!(stop_listed(store.clone()).await, "confirmed");
        let module = "/api/v1/module-consents/modules/doc-module";
        assert_eq!(delete(with(Unconfirmed), module).await, "unconfirmed");
        // The grant was told already: the module's revocation ends none.
        assert_eq!(pop(), told_end("*", Vec::new()));
        assert_eq!(stop_listed(store.clone()).await, "unconfirmed");
        let (pending, _) = store
            .pending_grant_end_notices("2099-01-01T00:00:00Z")
            .await
            .unwrap();
        assert!(
            pending.is_empty(),
            "each route answers its own: {pending:?}"
        );
        assert_eq!(unconfirmed(store.clone()).await, 1);
        // A later revocation of the key while the module answers: the answer
        // is not this revocation's, so it is not confirmed.
        let mut racing = state.clone();
        racing.module_tool_cards.as_mut().unwrap().notice = {
            let store = store.clone();
            StdArc::new(move |end: crate::module_tool_runtime::GrantEnd| {
                let store = store.clone();
                Box::pin(async move {
                    store
                        .revoke_module_consent_tracked(&end.module, None, "2026-10-24T00:30:00Z")
                        .await
                        .unwrap();
                    Confirmed
                }) as futures::future::BoxFuture<'static, _>
            })
        };
        assert_eq!(delete(build_router(racing), module).await, "unconfirmed");
        assert_eq!(unconfirmed(store.clone()).await, 2);
        assert_eq!(delete(with(NotRunning), module).await, "not_running");
        assert_eq!(stop_listed(store.clone()).await, "not_running");
        assert_eq!(
            unconfirmed(store.clone()).await,
            2,
            "a module not running is not audited"
        );
        // Revoked outside this route (the CLI's lifecycle revocation):
        // nothing recorded, so unconfirmed.
        store
            .revoke_module_consent("doc-module", None, "2026-10-24T01:00:00Z")
            .await
            .unwrap();
        assert_eq!(stop_listed(store.clone()).await, "unconfirmed");
        let no_cards = build_router(AppState {
            module_tool_cards: None,
            ..state.clone()
        });
        assert_eq!(delete(no_cards, module).await, "not_running");
        assert_eq!(unconfirmed(store.clone()).await, 2);
        // The notifier answered the row first (§2.5.1): its answer stands,
        // on the row and on the revocation, and the route claims none.
        let mut late = state.clone();
        late.module_tool_cards.as_mut().unwrap().notice = {
            let store = store.clone();
            StdArc::new(move |end: crate::module_tool_runtime::GrantEnd| {
                let store = store.clone();
                Box::pin(async move {
                    let (pending, _) = store
                        .pending_grant_end_notices("2099-01-01T00:00:00Z")
                        .await
                        .unwrap();
                    let row = pending.last().unwrap();
                    assert!(
                        store
                            .record_grant_end_stop(row.id, agent24_store::GrantEndStop::NotRunning)
                            .await
                            .unwrap()
                    );
                    store
                        .record_revocation_stop(&end.module, None, row.generation, "not_running")
                        .await
                        .unwrap();
                    Confirmed
                }) as futures::future::BoxFuture<'static, _>
            })
        };
        assert_eq!(delete(build_router(late), module).await, "unconfirmed");
        assert_eq!(stop_listed(store.clone()).await, "not_running");
        assert_eq!(unconfirmed(store.clone()).await, 3);
        let fresh = grant().await.unwrap();
        let views = authorization_list(store.clone()).await;
        assert!(views["authorizations"][0].get("stop").is_none(), "{views}");
        // A route's confirmed answer settles what its revocation carried
        // (#1202 G7, #1222 gate 3): an end left unconfirmed is carried by the
        // next revocation, and once that is confirmed, by none after it.
        let carries = |fresh: &agent24_store::ModuleConsentRecord| {
            let told = pop().unwrap().authorization_refs.unwrap();
            told.contains(&fresh.authorization_ref())
        };
        assert_eq!(delete(with(Unconfirmed), DECIDE).await, "unconfirmed");
        assert!(carries(&fresh));
        assert_eq!(delete(with(Confirmed), DECIDE).await, "confirmed");
        assert!(carries(&fresh), "carried while unconfirmed");
        assert_eq!(delete(with(Confirmed), DECIDE).await, "confirmed");
        assert!(!carries(&fresh), "settled by the route's confirmed answer");
    }

    /// Deletes `uri` through `state`, and gives the stop it answered.
    async fn revoked_stop(state: AppState, uri: &str) -> serde_json::Value {
        let response = build_router(state)
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(uri)
                    .header("Authorization", "Bearer testtoken")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        body_json(response).await["stop"].clone()
    }

    /// K1-6a-G §2.5.1 (A5c-2a review round 2): a revocation whose route
    /// cannot audit an unconfirmed answer, or cannot read its row, leaves
    /// both answers to the notifier: the row stays unanswered and the
    /// revocation records nothing; the route claims no stop. Unread, the
    /// module is told no references, and stops by operation.
    #[tokio::test]
    async fn a_revocation_the_route_cannot_finish_is_left_to_the_notifier() {
        use crate::module_tool_runtime::RevocationStop::{Confirmed, Unconfirmed};
        let pool = |store: &agent24_store::Store| agent24_store::test_hooks::pool(store).clone();
        let left = |store: agent24_store::Store| async move {
            let (pending, _) = store
                .pending_grant_end_notices("2099-01-01T00:00:00Z")
                .await
                .unwrap();
            let recorded = store
                .revocation_stop_state("doc-module", "read_document")
                .await
                .unwrap();
            (pending.len(), recorded)
        };
        // The audit fails.
        let dir = tempfile::tempdir().unwrap();
        let running = StdArc::new(std::sync::atomic::AtomicBool::new(true));
        let (mut state, _) = decision_state(&running, &dir.path().join("os.json")).await;
        let store = state.store.clone();
        state.module_tool_cards.as_mut().unwrap().notice = answering(Unconfirmed);
        sqlx::query("CREATE TRIGGER reject_audit BEFORE INSERT ON audit_log BEGIN SELECT RAISE(FAIL, 'offline'); END")
            .execute(&pool(&store))
            .await
            .unwrap();
        assert_eq!(revoked_stop(state, DECIDE).await, "unconfirmed");
        assert_eq!(left(store).await, (1, None));
        // The row cannot be read.
        let dir = tempfile::tempdir().unwrap();
        let (mut state, _) = decision_state(&running, &dir.path().join("os.json")).await;
        let store = state.store.clone();
        let told = StdArc::new(std::sync::Mutex::new(Vec::new()));
        state.module_tool_cards.as_mut().unwrap().notice =
            answering_into(Confirmed, StdArc::clone(&told));
        sqlx::query("CREATE TRIGGER garble AFTER INSERT ON module_grant_end_notices BEGIN UPDATE module_grant_end_notices SET authorization_refs = 'x' WHERE id = NEW.id; END")
            .execute(&pool(&store))
            .await
            .unwrap();
        assert_eq!(revoked_stop(state, DECIDE).await, "unconfirmed");
        assert_eq!(told.lock().unwrap()[0].authorization_refs, None);
        let recorded = store
            .revocation_stop_state("doc-module", "read_document")
            .await
            .unwrap();
        assert_eq!(recorded, None);
    }

    /// K1-6a-G §2.5 (A5b): an expired grant lists what its module answered
    /// when told it ended; one not told yet reads unconfirmed. An expired
    /// refusal started nothing to stop.
    #[tokio::test]
    async fn an_expired_grant_lists_whether_the_module_stopped() {
        let store = agent24_store::Store::open_memory().await.unwrap();
        let summary = |op: &str| {
            agent24_store::ToolPermissionSummary::new(
                "doc-module",
                op,
                "1.2.3",
                agent24_store::ConsentSource::FirstParty,
                Some("local documents".to_owned()),
                None,
                None,
                agent24_store::HostRiskLevel::Low,
            )
        };
        let (decided, expired) = ("2026-01-01T00:00:00Z", "2026-01-02T00:00:00Z");
        let granted = store
            .grant_module_consent(&summary("read_document"), decided, expired)
            .await
            .unwrap();
        store
            .deny_module_consent(&summary("write_document"), decided, expired)
            .await
            .unwrap();
        let stops = |list: serde_json::Value| {
            list["authorizations"]
                .as_array()
                .unwrap()
                .iter()
                .map(|a| (a["tool"].clone(), a["status"].clone(), a["stop"].clone()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            stops(authorization_list(store.clone()).await),
            [
                (
                    "read_document".into(),
                    "expired".into(),
                    "unconfirmed".into()
                ),
                (
                    "write_document".into(),
                    "expired".into(),
                    serde_json::Value::Null
                ),
            ]
        );
        assert!(
            store
                .record_expiry_stop(
                    "doc-module",
                    "read_document",
                    granted.generation,
                    "confirmed"
                )
                .await
                .unwrap()
        );
        assert_eq!(
            stops(authorization_list(store).await)[0].2,
            serde_json::json!("confirmed")
        );
    }

    /// #1290: the list calls a grant what the gate would make of a call —
    /// for the tool and the package as they are now. Only the grant for the
    /// running package reads granted; one with no package identity, one for
    /// another package (replaced in place), one whose package is not
    /// running, one whose tool is not mounted, one of a module not on the
    /// grants protocol (never dispatched, §2.6) and one given for an earlier
    /// version of the tool all read stale.
    #[tokio::test]
    async fn the_list_calls_stale_what_the_gate_would_refuse() {
        let dir = tempfile::tempdir().unwrap();
        let running = StdArc::new(std::sync::atomic::AtomicBool::new(true));
        let (state, summary) = decision_state(&running, &dir.path().join("os.json")).await;
        let (decided, expires) = ("2026-10-24T00:00:00Z", "2099-01-01T00:00:00Z");
        let statuses = |state: AppState| async move {
            let list = listed(state).await;
            let views = list["authorizations"].as_array().unwrap();
            let status = |module: &str| {
                let view = views.iter().find(|view| view["module"] == module).unwrap();
                view["status"].as_str().unwrap().to_owned()
            };
            (
                status("doc-module"),
                status("gone-module"),
                status("old-module"),
            )
        };
        let grant = |summary: agent24_store::ToolPermissionSummary, artifact: Option<&str>| {
            let (store, artifact) = (state.store.clone(), artifact.map(str::to_owned));
            async move {
                let scope = agent24_store::GrantScope {
                    resources: Vec::new(),
                    artifact,
                };
                store
                    .grant_module_consent_scoped(&summary, &scope, decided, expires)
                    .await
                    .unwrap();
            }
        };
        // The tool is gone from the running package: never dispatched.
        let gone = agent24_store::ToolPermissionSummary {
            module: "gone-module".to_owned(),
            ..summary.clone()
        };
        grant(gone, Some("sha256:abc")).await;
        // Version, scope and package all match, but the module is not on the
        // grants protocol: the gate never dispatches it.
        let old_protocol = agent24_store::ToolPermissionSummary {
            module: "old-module".to_owned(),
            ..summary.clone()
        };
        grant(old_protocol, Some("sha256:abc")).await;

        grant(summary.clone(), Some("sha256:abc")).await;
        assert_eq!(
            statuses(state.clone()).await,
            ("granted".to_owned(), "stale".to_owned(), "stale".to_owned())
        );
        running.store(false, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(statuses(state.clone()).await.0, "stale");
        running.store(true, std::sync::atomic::Ordering::SeqCst);
        // Given for 1.0.0 of a tool that is 1.2.3 now: judged by what it is now.
        let earlier = agent24_store::ToolPermissionSummary {
            module_version: "1.0.0".to_owned(),
            ..summary.clone()
        };
        grant(earlier, Some("sha256:abc")).await;
        assert_eq!(statuses(state.clone()).await.0, "stale");
        grant(summary.clone(), Some("sha256:replaced-in-place")).await;
        assert_eq!(statuses(state.clone()).await.0, "stale");
        grant(summary, None).await;
        assert_eq!(statuses(state.clone()).await.0, "stale");
        // No identity on either side is no match.
        running.store(false, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(statuses(state).await.0, "stale");
    }

    async fn authorization_list(store: agent24_store::Store) -> serde_json::Value {
        let mut state = state().await;
        state.store = store;
        listed(state).await
    }

    async fn listed(state: AppState) -> serde_json::Value {
        let response = build_router(state)
            .oneshot(
                Request::builder()
                    .uri("/api/v1/module-consents")
                    .header("Authorization", "Bearer testtoken")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        body_json(response).await
    }

    /// A state with one grants-protocol tool (`doc-module.read_document`,
    /// kinds `doc`) and one older one, running while `running` holds, and
    /// decisions open against `os_json`.
    async fn decision_state(
        running: &StdArc<std::sync::atomic::AtomicBool>,
        os_json: &std::path::Path,
    ) -> (AppState, agent24_store::ToolPermissionSummary) {
        let mut state = state().await;
        let summary = agent24_store::ToolPermissionSummary::new(
            "doc-module",
            "read_document",
            "1.2.3",
            agent24_store::ConsentSource::ManualInstall,
            None,
            None,
            None,
            agent24_store::HostRiskLevel::High,
        );
        let source = |module: &str, takes_grants| crate::consent_routes::ToolCardSource {
            summary: agent24_store::ToolPermissionSummary {
                module: module.to_owned(),
                ..summary.clone()
            },
            resource_kinds: vec!["doc".to_owned()],
            takes_grants,
            host_read: false,
        };
        let running = StdArc::clone(running);
        state.module_tool_cards = Some(crate::consent_routes::ModuleToolCards {
            notice: answering(crate::module_tool_runtime::RevocationStop::NotRunning),
            tools: StdArc::new(std::collections::HashMap::from([
                (
                    ("doc-module".to_owned(), "read_document".to_owned()),
                    source("doc-module", true),
                ),
                (
                    ("old-module".to_owned(), "read_document".to_owned()),
                    source("old-module", false),
                ),
            ])),
            artifact: StdArc::new(move |_: &str| {
                running
                    .load(std::sync::atomic::Ordering::SeqCst)
                    .then(|| "sha256:abc".to_owned())
            }),
        });
        state.module_grants = Some(crate::consent_routes::GrantSettings {
            max_days: 30,
            os_config: os_json.to_owned(),
        });
        (state, summary)
    }

    const DECIDE: &str = "/api/v1/module-consents/modules/doc-module/tools/read_document";

    fn decision(
        summary: &agent24_store::ToolPermissionSummary,
        patch: serde_json::Value,
    ) -> String {
        let mut body = serde_json::json!({
            "decision": "granted",
            "fingerprint": summary.scope_fingerprint(),
            "module_version": "1.2.3",
            "artifact": "sha256:abc",
            "resources": ["doc:a1"],
            "expires_at": (chrono::Utc::now() + chrono::Duration::days(7)).to_rfc3339(),
        });
        for (key, value) in patch.as_object().unwrap() {
            body[key] = value.clone();
        }
        body.to_string()
    }

    async fn post(router: &Router, uri: &str, body: String) -> (StatusCode, serde_json::Value) {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("Authorization", "Bearer testtoken")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        (status, body_json(response).await)
    }

    /// K1-6a-G §2.1, criteria 6, 7 and 14: a decision is written only on the
    /// card as the tool is checked now, with the switch on, the module
    /// enabled and running, a bounded expiry and resources of declared
    /// kinds; anything else writes nothing.
    #[tokio::test]
    async fn a_module_tool_decision_is_written_only_on_the_card_as_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let os_json = dir.path().join("os.json");
        let running = StdArc::new(std::sync::atomic::AtomicBool::new(true));
        let (state, summary) = decision_state(&running, &os_json).await;
        let store = state.store.clone();
        let closed = build_router(AppState {
            module_grants: None,
            ..state.clone()
        });
        let router = build_router(state);
        let code = |(status, body): (StatusCode, serde_json::Value)| {
            (
                status,
                body["error"]["code"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            )
        };
        let expect = |status: StatusCode, code: &str| (status, code.to_owned());
        let ok = decision(&summary, serde_json::json!({}));
        assert_eq!(
            code(post(&closed, DECIDE, ok.clone()).await),
            expect(StatusCode::FORBIDDEN, "grants_closed")
        );
        let (status, _) = post(&router, &format!("{DECIDE}x"), ok.clone()).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(
            code(
                post(
                    &router,
                    "/api/v1/module-consents/modules/old-module/tools/read_document",
                    ok.clone()
                )
                .await
            ),
            expect(StatusCode::CONFLICT, "protocol_too_old")
        );
        let (status, _) = post(&router, DECIDE, "{".to_owned()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let not_text = decision(&summary, serde_json::json!({ "resources": ["doc:a1", 5] }));
        let (status, _) = post(&router, DECIDE, not_text).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let in_days = |days| (chrono::Utc::now() + chrono::Duration::days(days)).to_rfc3339();
        let many: Vec<String> = (0..257).map(|i| format!("doc:d{i}")).collect();
        for patch in [
            serde_json::json!({ "expires_at": null }),
            serde_json::json!({ "expires_at": "next week" }),
            serde_json::json!({ "expires_at": in_days(31) }),
            serde_json::json!({ "expires_at": in_days(-1) }),
            serde_json::json!({ "resources": ["doc:a b"] }),
            serde_json::json!({ "resources": ["page:a1"] }),
            serde_json::json!({ "resources": many }),
            serde_json::json!({ "decision": "denied" }),
        ] {
            assert_eq!(
                code(post(&router, DECIDE, decision(&summary, patch.clone())).await),
                expect(StatusCode::UNPROCESSABLE_ENTITY, "invalid_decision"),
                "{patch}"
            );
        }
        for patch in [
            serde_json::json!({ "fingerprint": "sha256:other" }),
            serde_json::json!({ "module_version": "1.2.4" }),
            serde_json::json!({ "artifact": "sha256:other" }),
        ] {
            assert_eq!(
                code(post(&router, DECIDE, decision(&summary, patch.clone())).await),
                expect(StatusCode::CONFLICT, "card_changed"),
                "{patch}"
            );
        }
        running.store(false, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            code(post(&router, DECIDE, ok.clone()).await),
            expect(StatusCode::CONFLICT, "no_package_identity")
        );
        running.store(true, std::sync::atomic::Ordering::SeqCst);
        std::fs::write(&os_json, r#"{"domainOs":{"doc-module":{"enabled":false}}}"#).unwrap();
        assert_eq!(
            code(post(&router, DECIDE, ok.clone()).await),
            expect(StatusCode::CONFLICT, "module_disabled")
        );
        std::fs::write(&os_json, "not json").unwrap();
        assert_eq!(
            code(post(&router, DECIDE, ok.clone()).await),
            expect(StatusCode::INTERNAL_SERVER_ERROR, "internal")
        );
        assert!(store.list_module_consents().await.unwrap().is_empty());

        std::fs::remove_file(&os_json).unwrap();
        let both = decision(
            &summary,
            serde_json::json!({ "resources": ["doc:b2", "doc:a1", "doc:b2"] }),
        );
        let (status, body) = post(&router, DECIDE, both).await;
        assert_eq!(
            (status, &body["status"]),
            (StatusCode::OK, &serde_json::json!("granted"))
        );
        let records = store.list_module_consents().await.unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].resources, ["doc:a1", "doc:b2"]);
        assert_eq!(records[0].artifact.as_deref(), Some("sha256:abc"));
        let now = chrono::Utc::now().to_rfc3339();
        let lookup = |store: agent24_store::Store| {
            let (fingerprint, now) = (summary.scope_fingerprint(), now.clone());
            async move {
                store
                    .lookup_module_consent(
                        "doc-module",
                        "read_document",
                        "1.2.3",
                        &fingerprint,
                        &now,
                    )
                    .await
                    .unwrap()
            }
        };
        assert!(matches!(
            lookup(store.clone()).await,
            agent24_store::ConsentLookup::Granted(_)
        ));
        let denial = decision(
            &summary,
            serde_json::json!({ "decision": "denied", "resources": [] }),
        );
        let (status, body) = post(&router, DECIDE, denial).await;
        assert_eq!(
            (status, &body["status"]),
            (StatusCode::OK, &serde_json::json!("denied"))
        );
        assert!(matches!(
            lookup(store).await,
            agent24_store::ConsentLookup::Denied(_)
        ));
    }

    /// K1-6a-G §2.1 F7 and #1157's S2: a decision waits for `os_control`,
    /// so no enable, disable or stop lands between its checks and its write.
    #[tokio::test]
    async fn a_module_tool_decision_waits_for_os_control() {
        let dir = tempfile::tempdir().unwrap();
        let running = StdArc::new(std::sync::atomic::AtomicBool::new(true));
        let (state, summary) = decision_state(&running, &dir.path().join("os.json")).await;
        let (store, control) = (state.store.clone(), state.os_control.clone());
        let router = build_router(state);
        let held = control.lock_owned().await;
        let body = decision(&summary, serde_json::json!({}));
        let task = tokio::spawn(async move { post(&router, DECIDE, body).await });
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(!task.is_finished());
        assert!(store.list_module_consents().await.unwrap().is_empty());
        drop(held);
        assert_eq!(task.await.unwrap().0, StatusCode::OK);
    }

    /// The host's switch and bound (K1-6a-G §2.1): off by default and on an
    /// ephemeral daemon; a bound past 90 days, or none, stops the start.
    #[test]
    fn module_tool_decisions_open_only_on_the_host_switch() {
        use crate::consent_routes::{GrantSettings, grant_settings};
        let path = || Some(std::path::PathBuf::from("/x/os.json"));
        let open = |days| {
            Ok(Some(GrantSettings {
                max_days: days,
                os_config: "/x/os.json".into(),
            }))
        };
        assert_eq!(grant_settings(None, None, false, path()), Ok(None));
        assert_eq!(grant_settings(Some("0"), None, false, path()), Ok(None));
        assert_eq!(grant_settings(Some("1"), None, false, path()), open(30));
        assert_eq!(
            grant_settings(Some("1"), Some("90"), false, path()),
            open(90)
        );
        assert_eq!(grant_settings(Some("1"), Some("1"), false, path()), open(1));
        assert_eq!(grant_settings(Some("1"), None, true, path()), Ok(None));
        assert_eq!(grant_settings(Some("1"), None, false, None), Ok(None));
        for bad in ["0", "91", "-1", "30d", ""] {
            assert!(
                grant_settings(None, Some(bad), false, path()).is_err(),
                "{bad}"
            );
        }
        assert!(grant_settings(Some("yes"), None, false, path()).is_err());
    }

    #[tokio::test]
    async fn module_consent_routes_reject_creative_identity() {
        let (state, _host, creative) = capability_state().await;
        let state_again = state.clone();
        // Decisions open, so only the host-authority check can answer 403.
        let state_post = AppState {
            module_grants: Some(crate::consent_routes::GrantSettings {
                max_days: 30,
                os_config: "/nonexistent/os.json".into(),
            }),
            ..state.clone()
        };
        let router = build_router(state);
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/api/v1/module-consents")
                    .header("Authorization", format!("Bearer {creative}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let response = build_router(state_again)
            .oneshot(
                Request::builder()
                    .uri("/api/v1/module-consents/modules/doc-module/tools/read_document/card")
                    .header("Authorization", format!("Bearer {creative}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let response = build_router(state_post)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(DECIDE)
                    .header("Authorization", format!("Bearer {creative}"))
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn capability_startup_requires_the_trusted_host_bootstrap_transport() {
        assert_eq!(
            validate_auth_startup(AuthMode::Capabilities, true, false, false).unwrap(),
            StartupMode::HostChild
        );
        let err = validate_auth_startup(AuthMode::Capabilities, false, false, false)
            .expect_err("capability startup without private host pipes");
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn only_the_host_child_mints_a_product_host() {
        assert!(mints_product_host(StartupMode::HostChild));
        assert!(!mints_product_host(StartupMode::Service));
        assert!(!mints_product_host(StartupMode::Legacy));
    }

    /// CORE-2e T4: every row of the D8 matrix (+ Q5, + ephemeral service).
    #[test]
    fn startup_matrix_matches_d8() {
        use AuthMode::{Capabilities as C, LegacySingleToken as L};
        let m = |a, b, e, eph| validate_auth_startup(a, b, e, eph).ok();
        assert_eq!(m(L, false, false, false), Some(StartupMode::Legacy));
        assert_eq!(m(L, true, false, false), None);
        assert_eq!(m(L, true, true, false), None);
        assert_eq!(m(L, false, true, false), None);
        assert_eq!(m(C, true, false, false), Some(StartupMode::HostChild));
        assert_eq!(m(C, true, false, true), Some(StartupMode::HostChild));
        assert_eq!(m(C, false, true, false), Some(StartupMode::Service));
        assert_eq!(m(C, false, false, false), None);
        assert_eq!(m(C, true, true, false), None);
        assert_eq!(m(C, false, true, true), None);
    }

    #[tokio::test]
    async fn capability_state_does_not_accept_the_empty_legacy_sentinel() {
        let mut capability_state = state().await;
        capability_state.enable_capability_auth(crate::capabilities::CapabilityStore::new(
            "capability-generation".to_owned(),
        ));

        let res = build_router(capability_state)
            .oneshot(
                Request::builder()
                    .uri("/api/v1/models")
                    .header(header::AUTHORIZATION, "Bearer ")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(body_json(res).await["error"]["code"], "unauthorized");
    }

    #[tokio::test]
    async fn creative_bearer_can_read_models_but_not_the_closed_default_route() {
        let store = crate::capabilities::CapabilityStore::new("capability-generation");
        let host = store
            .mint_product_host(
                "host-generation",
                Duration::from_secs(60),
                crate::capabilities::unix_now(),
            )
            .unwrap();
        let creative = store
            .mint_creative(
                host.token(),
                crate::capabilities::CreativeMintRequest::new(
                    "workspace",
                    "attachment",
                    "principal",
                    "sidecar-generation",
                    Duration::from_secs(60),
                    crate::capabilities::unix_now(),
                ),
            )
            .unwrap();
        let (creative_bearer, _, _) = creative.into_bearer_parts();
        let mut capability_state = state().await;
        capability_state.enable_capability_auth(store);
        let router = build_router(capability_state);

        let models = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/models")
                    .header(header::AUTHORIZATION, format!("Bearer {creative_bearer}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(models.status(), StatusCode::OK);

        let unknown = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/not-a-route")
                    .header(header::AUTHORIZATION, format!("Bearer {creative_bearer}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
        assert_eq!(body_json(unknown).await["error"]["code"], "not_found");

        let closed = router
            .oneshot(
                Request::builder()
                    .uri("/api/v1/usage")
                    .header(header::AUTHORIZATION, format!("Bearer {creative_bearer}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(closed.status(), StatusCode::FORBIDDEN);
        assert_eq!(body_json(closed).await["error"]["code"], "forbidden");
    }

    #[tokio::test]
    async fn legacy_state_still_accepts_its_bearer_token() {
        let res = build_router(state().await)
            .oneshot(
                Request::builder()
                    .uri("/api/v1/not-a-route")
                    .header(header::AUTHORIZATION, "Bearer testtoken")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn post_to_health_path_requires_token() {
        // The auth exemption is GET-only — same path, other method: 401
        let res = build_router(state().await)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        let json = body_json(res).await;
        assert_eq!(json["error"]["code"], "unauthorized");
    }

    /// A MOUNTED domain OS is behind the kernel's auth, exactly like a kernel
    /// route.
    ///
    /// This is the regression for the mount-order hazard in
    /// [`build_router_with_modules`]: an axum layer applies only to the routes
    /// already on the router, so the version of this that "obviously compiles" —
    /// `kernel.with_state(state).layer(auth).merge(modules)` — mounts every module
    /// route with NO authentication at all. That failure is silent: the module
    /// works, its tests pass, and the daemon simply serves a module's whole
    /// surface to anyone who can reach the port. Reorder those two lines and this
    /// test goes from 401 to 200.
    #[tokio::test]
    async fn module_routes_are_behind_kernel_auth() {
        use agent24_domain::{DomainModule, DomainOsManifest, KernelCtx};

        struct OpenModule(DomainOsManifest);
        #[async_trait::async_trait]
        impl DomainModule for OpenModule {
            fn manifest(&self) -> &DomainOsManifest {
                &self.0
            }
            async fn open_store(&self, _dir: &std::path::Path) -> agent24_domain::Result<()> {
                Ok(())
            }
            fn routes(&self, _ctx: StdArc<dyn KernelCtx>) -> Router {
                // Deliberately unauthenticated on its own: modules must not have
                // to implement auth, the kernel owns it.
                Router::new().route("/secret", get(|| async { "leaked" }))
            }
        }

        let st = state().await;
        let token = st.token.to_string();
        let tmp = tempfile::tempdir().unwrap();
        let m = StdArc::new(OpenModule(
            DomainOsManifest::from_yaml(
                "name: probe\nversion: \"0.1.0\"\nroute_namespace: /api/v1/probe\n\
                 event_module: probe\ndata_dir: ~/.agent24/os/probe/\n\
                 kernel_capabilities: [events]\nimpl_kind: in_process_crate\n",
            )
            .unwrap(),
        ));
        let entry = crate::domain::Installed {
            name: "probe".to_owned(),
            version: "0.1.0".to_owned(),
            build: crate::domain::Build::InProcess(Box::new(move || {
                Ok(m.clone() as StdArc<dyn DomainModule>)
            })),
        };
        let (modules, reports, _) = crate::domain::mount_all(
            &[entry],
            tmp.path(),
            &st.events,
            Ok(&crate::os_config::OsConfig::default()),
            &NoModels,
            None,
            Err("no process host in this test"),
            &test_approval_broker(&st.events).await,
            crate::domain::CallbackDeps {
                scheduler: st.scheduler.clone(),
                models: None,
            },
        )
        .await;
        assert_eq!(reports[0].outcome, crate::domain::MountOutcome::Mounted);
        let router = build_router_with_modules(st, modules);

        // No token: 401, in the kernel's v1 envelope — not the module's 200.
        let res = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/probe/secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            res.status(),
            StatusCode::UNAUTHORIZED,
            "a module route without a token must 401 — if this is 200, modules \
             were nested AFTER the auth layer and are an authentication hole"
        );
        assert_eq!(body_json(res).await["error"]["code"], "unauthorized");

        // With the token: the module answers.
        let res = router
            .oneshot(
                Request::builder()
                    .uri("/api/v1/probe/secret")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    /// An OUT-OF-PROCESS module is behind the kernel's auth too — and an
    /// unauthenticated caller does not even cause a connection to it.
    ///
    /// The 401 itself is already covered for in-process modules by
    /// [`module_routes_are_behind_kernel_auth`]; what is new here is that the
    /// thing being protected is a socket to another program. "401" and "the
    /// module never heard about it" are different claims: a proxy mounted as a
    /// FALLBACK, or one that dialled upstream before the layer ran, could answer
    /// 401 to the client while the module had already seen the request — and a
    /// module that logs or acts on what it receives is then reachable by anyone
    /// who can reach the port. So the assertion is on the upstream's connection
    /// count, not only on the status.
    ///
    /// The daemon does not mount a proxy in production yet: that is SUP-4,
    /// which starts a supervisor per module and hands its `Current` to
    /// `proxy::mount` — the address travels inside each run's `Generation`
    /// (SUP-3b), not through the mount. This pins the composition
    /// (`proxy::mount` + `build_router_with_modules`) before that consumer
    /// exists, which is the point at which the mount order is still cheap to
    /// get right.
    #[tokio::test]
    async fn a_proxied_module_is_behind_kernel_auth_and_is_not_even_dialled() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let connections = StdArc::new(AtomicUsize::new(0));
        // Pid plus a nanosecond timestamp: a pid-only name left a stale node
        // for a later run reusing this pid to collide with (review of
        // FU-60, round 1).
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let upstream =
            std::env::temp_dir().join(format!("a24-zzproxy-{}-{nanos}.sock", std::process::id()));
        let listener = tokio::net::UnixListener::bind(&upstream).unwrap();
        let counter = connections.clone();
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            while let Ok((mut socket, _)) = listener.accept().await {
                counter.fetch_add(1, Ordering::SeqCst);
                let _ = socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nmodule")
                    .await;
                let _ = socket.shutdown().await;
            }
        });

        let st = state().await;
        let token = st.token.to_string();
        let module = agent24_os_proto::drain::Generation::serving_at(upstream);
        assert!(module.ready());
        let modules = agent24_os_proto::proxy::mount(
            Router::new(),
            "/api/v1/zzproxy",
            agent24_os_proto::drain::Current::new(module),
        );
        let router = build_router_with_modules(st, modules);

        let res = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/zzproxy/anything")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(body_json(res).await["error"]["code"], "unauthorized");
        assert_eq!(
            connections.load(Ordering::SeqCst),
            0,
            "the module was dialled for a request that had no token"
        );

        // The control. Without it, zero connections is also what a proxy
        // pointed at nothing produces, and the 401 proves nothing about the
        // mount order.
        let res = router
            .oneshot(
                Request::builder()
                    .uri("/api/v1/zzproxy/anything")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(connections.load(Ordering::SeqCst), 1);
    }

