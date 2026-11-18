    async fn put_default(
        app: &axum::Router,
        bearer: &str,
        body: &str,
    ) -> axum::response::Response {
        app.clone()
            .oneshot(
                Request::builder()
                    .method(Method::PUT)
                    .uri("/api/v1/models/default")
                    .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn memory_state() -> (AppState, String, String, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let kv = agent24_memory::KvStore::open(&dir.path().join("memory.db"))
            .await
            .unwrap();
        let (mut state, host, creative) = capability_state().await;
        state.memory_kv = Some(kv);
        state.router.set_default_model("EnvFallback");
        (state, host, creative, dir)
    }

    #[tokio::test]
    async fn put_default_model_saves_and_takes_effect_then_survives_reopen() {
        let (state, host, _creative, dir) = memory_state().await;
        let router = std::sync::Arc::clone(&state.router);
        let app = build_router(state);
        let saved = put_default(&app, &host, r#"{"model":"  Qwen3-30B  "}"#).await;
        assert_eq!(saved.status(), StatusCode::OK);
        assert_eq!(body_json(saved).await["model"], "Qwen3-30B");
        assert_eq!(router.default_model().as_deref(), Some("Qwen3-30B"));

        let listed = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/models")
                    .header(header::AUTHORIZATION, format!("Bearer {host}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(listed.status(), StatusCode::OK);
        assert_eq!(body_json(listed).await["default_model"], "Qwen3-30B");

        let reopened = agent24_memory::KvStore::open(&dir.path().join("memory.db"))
            .await
            .unwrap();
        let fresh = ModelRouter::with_defaults(vec![]);
        fresh.set_default_model("EnvFallback");
        crate::default_model::apply_saved(&fresh, &reopened).await;
        assert_eq!(fresh.default_model().as_deref(), Some("Qwen3-30B"));
    }

    #[tokio::test]
    async fn put_default_model_rejects_invalid_ids() {
        let (state, host, _, _dir) = memory_state().await;
        let app = build_router(state);
        for body in [
            "{}",
            r#"{"model":""}"#,
            r#"{"model":"   "}"#,
            &format!(r#"{{"model":"{}"}}"#, "m".repeat(121)),
            &format!(r#"{{"model":"{}"}}"#, "m".repeat(129)),
            "{\"model\":\"ab\\u0001c\"}",
            "{\"model\":\"ab\\u200bc\"}",
        ] {
            let res = put_default(&app, &host, body).await;
            assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{body}");
            assert_eq!(body_json(res).await["error"]["code"], "invalid_request");
        }
    }

    #[tokio::test]
    async fn put_default_model_refuses_non_host_audiences() {
        let (host_state, host, creative, _) = memory_state().await;
        let app = build_router(host_state);
        let creative_res = put_default(&app, &creative, r#"{"model":"Qwen3-30B"}"#).await;
        assert_eq!(creative_res.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            body_json(creative_res).await["error"]["code"],
            "forbidden"
        );

        let legacy = build_router(state().await);
        let legacy_res = put_default(&legacy, "testtoken", r#"{"model":"Qwen3-30B"}"#).await;
        assert_eq!(legacy_res.status(), StatusCode::FORBIDDEN);

        let host_ok = put_default(&app, &host, r#"{"model":"Qwen3-30B"}"#).await;
        assert_eq!(host_ok.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn put_default_model_is_unavailable_without_a_memory_base() {
        let (state, host, _) = capability_state().await;
        assert!(state.memory_kv.is_none());
        let app = build_router(state);
        let res = put_default(&app, &host, r#"{"model":"Qwen3-30B"}"#).await;
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            body_json(res).await["error"]["code"],
            "default_model_unavailable"
        );
    }
