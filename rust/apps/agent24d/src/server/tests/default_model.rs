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

    #[tokio::test]
    async fn recommendation_rejects_other_roles_and_returns_fallback_when_idoris_is_absent() {
        let app = build_router(state().await);
        let request = |uri: &str| {
            Request::builder()
                .uri(uri)
                .header(header::AUTHORIZATION, "Bearer testtoken")
                .body(Body::empty())
                .unwrap()
        };
        let rejected = app.clone().oneshot(request("/api/v1/models/recommendation?role=vision")).await.unwrap();
        assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
        let missing_role = app.clone().oneshot(request("/api/v1/models/recommendation")).await.unwrap();
        assert_eq!(missing_role.status(), StatusCode::BAD_REQUEST);
        let response = app.clone().oneshot(request("/api/v1/models/recommendation?role=chat")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["source"], "fallback");
        assert_eq!(body["recommendation"]["model_id"], "Qwen3-8B-4bit");
        assert_eq!(body["recommendation"]["reason"], "idoris_unavailable");
        let local = app.oneshot(request("/api/v1/models/local")).await.unwrap();
        assert_eq!(body_json(local).await["models"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn startup_seed_without_idoris_does_not_save_or_replace_default() {
        let dir = tempfile::tempdir().unwrap();
        let kv = agent24_memory::KvStore::open(&dir.path().join("seed.db")).await.unwrap();
        let router = std::sync::Arc::new(ModelRouter::with_defaults(vec![]));
        router.set_default_model("EnvDefault");
        crate::default_model::seed_from_idoris(std::sync::Arc::clone(&router), kv.clone(), None).await;
        assert_eq!(router.default_model().as_deref(), Some("EnvDefault"));
        assert!(kv.fetch::<crate::default_model::SavedDefaultChatModel>(
            crate::default_model::NAMESPACE, crate::default_model::KEY
        ).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn installed_idoris_recommendation_seeds_only_an_empty_default() {
        let dir = tempfile::tempdir().unwrap();
        let kv = agent24_memory::KvStore::open(&dir.path().join("seed-success.db"))
            .await
            .unwrap();
        let router = ModelRouter::with_defaults(vec![]);
        let recommendation = serde_json::json!({"model_id":"Qwen3.8-27B-OptiQ-4bit", "installed":true});
        let selected = crate::default_model::seed_recommendation(
            &kv, "idoris_http", &recommendation,
        ).await.unwrap().unwrap();
        router.set_default_model(selected);
        assert_eq!(router.default_model().as_deref(), Some("Qwen3.8-27B-OptiQ-4bit"));
        assert_eq!(
            kv.fetch::<crate::default_model::SavedDefaultChatModel>(
                crate::default_model::NAMESPACE,
                crate::default_model::KEY,
            ).await.unwrap().unwrap().model,
            "Qwen3.8-27B-OptiQ-4bit",
        );

        crate::default_model::store_default(&kv, "UserChoice").await.unwrap();
        let ignored = crate::default_model::seed_recommendation(
            &kv, "idoris_http", &recommendation,
        ).await.unwrap();
        assert!(ignored.is_none());
        assert_eq!(router.default_model().as_deref(), Some("Qwen3.8-27B-OptiQ-4bit"));
    }

    #[tokio::test]
    async fn fallback_or_uninstalled_recommendations_are_not_saved() {
        let dir = tempfile::tempdir().unwrap();
        let kv = agent24_memory::KvStore::open(&dir.path().join("seed-fallback.db"))
            .await
            .unwrap();
        for (source, recommendation) in [
            ("fallback", serde_json::json!({"model_id":"Qwen3-8B-4bit", "installed":true})),
            ("idoris_http", serde_json::json!({"model_id":"Qwen3-8B-4bit", "installed":false})),
            ("idoris_http", serde_json::json!({"model_id":null, "installed":false})),
        ] {
            assert!(crate::default_model::seed_recommendation(
                &kv, source, &recommendation,
            ).await.unwrap().is_none());
        }
        assert!(kv.fetch::<crate::default_model::SavedDefaultChatModel>(
            crate::default_model::NAMESPACE, crate::default_model::KEY,
        ).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn put_default_connected_replaces_before_saving_and_preserves_value_on_failure() {
        use agent24_idoris::{idoris_host::{ConnectRequest, IdorisHost, Installation}, managed_idoris::ManagedIdoris};
        use std::sync::Arc;
        let (mut state, host, _creative, dir) = memory_state().await;
        let kv = state.memory_kv.clone().unwrap();
        crate::default_model::store_default(&kv, "old-model").await.unwrap();
        let managed = Arc::new(ManagedIdoris::new(state.router.clone(), dir.path().join("connection.json")).unwrap());
        let idoris = IdorisHost::system(dir.path().to_path_buf(), dir.path().to_path_buf(), managed, state.router.clone()).unwrap();
        let connection: ConnectRequest = serde_json::from_value(serde_json::json!({
            "source":"openai_compat", "endpoint":"http://127.0.0.1:3456/", "daily_model":"old-model", "deep_model":"deep-model", "core_reserved_bytes":0
        })).unwrap();
        idoris.record_installation(Installation {
            connection: Some(connection),
            version: agent24_idoris::IdorisLock::embedded().tag.trim_start_matches('v').into(),
            release_dir: dir.path().join("missing-release"),
            archive_path: dir.path().join("archive.tar.gz"),
            components_dir: Some(dir.path().join("components")),
            key_id: Some("vk_fixture".into()), data_port: 23456, admin_port: 23457,
            roles: vec![agent24_idoris::setup::Role::Daily, agent24_idoris::setup::Role::Deep],
            core_reserved_bytes: Some(0), omlx_url: None, omlx_key_id: None,
            desired_running: false, verification: None, install_root: None, notarized_verified_at: None,
        }).await.unwrap();
        state.idoris_host = Some(idoris.clone());
        let router = state.router.clone();
        let app = build_router(state);
        let response = put_default(&app, &host, r#"{"model":"new-model"}"#).await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body_json(response).await["error"]["code"], "not_installed");
        assert_eq!(kv.fetch::<crate::default_model::SavedDefaultChatModel>(crate::default_model::NAMESPACE, crate::default_model::KEY).await.unwrap().unwrap().model, "old-model");
        assert_eq!(router.default_model().as_deref(), Some("EnvFallback"));
        // A replace operation was attempted, rather than silently storing the id.
        let operations: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.path().join("operations.json")).unwrap()).unwrap();
        assert!(operations.to_string().contains("connect"));
    }
