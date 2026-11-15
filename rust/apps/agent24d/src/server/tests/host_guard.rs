    // ---- B1: Host / Origin guard --------------------------------------

    async fn guarded_status(host: Option<&str>, origin: Option<&str>) -> (StatusCode, String) {
        let router = guard_host_origin(build_router(state().await), 4242);
        let mut b = Request::builder().uri("/api/v1/health");
        if let Some(h) = host {
            b = b.header(header::HOST, h);
        }
        if let Some(o) = origin {
            b = b.header(header::ORIGIN, o);
        }
        let res = router
            .oneshot(b.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = res.status();
        let code = if status == StatusCode::OK {
            String::new()
        } else {
            body_json(res).await["error"]["code"]
                .as_str()
                .unwrap_or("")
                .to_owned()
        };
        (status, code)
    }

    #[tokio::test]
    async fn guard_accepts_the_three_loopback_hosts_case_insensitively() {
        for h in [
            "127.0.0.1:4242",
            "localhost:4242",
            "[::1]:4242",
            "LocalHost:4242",
        ] {
            assert_eq!(guarded_status(Some(h), None).await.0, StatusCode::OK, "{h}");
        }
    }

    #[tokio::test]
    async fn guard_rejects_wrong_host_even_on_health() {
        for h in [
            "evil.example:4242",
            "127.0.0.1:4243",
            "127.0.0.1",
            "localhost.evil.com:4242",
            "127.0.0.1:4242.evil.com",
        ] {
            let (s, c) = guarded_status(Some(h), None).await;
            assert_eq!((s, c.as_str()), (StatusCode::FORBIDDEN, "bad_host"), "{h}");
        }
    }

    #[tokio::test]
    async fn guard_rejects_a_request_with_no_host_as_400() {
        let (s, c) = guarded_status(None, None).await;
        assert_eq!((s, c.as_str()), (StatusCode::BAD_REQUEST, "bad_host"));
    }

    #[tokio::test]
    async fn guard_rejects_a_browser_origin_but_allows_null_and_none() {
        let (s, c) = guarded_status(Some("127.0.0.1:4242"), Some("http://evil.example")).await;
        assert_eq!((s, c.as_str()), (StatusCode::FORBIDDEN, "forbidden_origin"));
        let (s, _) = guarded_status(Some("127.0.0.1:4242"), Some("null")).await;
        assert_eq!(s, StatusCode::OK);
        let (s, _) = guarded_status(Some("127.0.0.1:4242"), None).await;
        assert_eq!(s, StatusCode::OK);
    }

    #[tokio::test]
    async fn guard_runs_before_auth_and_in_front_of_module_routes() {
        // Wrong Host on an authenticated route is the guard's answer, not 401.
        let router = guard_host_origin(build_router(state().await), 4242);
        let res = router
            .oneshot(
                Request::builder()
                    .uri("/api/v1/models")
                    .header(header::HOST, "evil.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn legacy_ready_line_has_token_unless_told_to_omit_it() {
        let with = legacy_ready_line(4242, "sekret", false, "g1");
        assert_eq!(with["token"], "sekret");
        assert_eq!(with["port"], 4242);
        let without = legacy_ready_line(4242, "sekret", true, "g1");
        assert!(without.get("token").is_none());
        assert!(!without.to_string().contains("sekret"));
        assert_eq!(without["type"], "ready");
        assert_eq!(without["port"], 4242);
        assert_eq!(without["auth_mode"], "legacy_single_token");
    }

    #[test]
    fn serve_layers_the_host_guard_on_the_router_it_serves() {
        let src = include_str!("../serve.rs");
        let start = src.find("pub async fn serve(").expect("serve must exist");
        assert!(src[start..].contains("guard_host_origin(router, local.port())"));
    }

    #[tokio::test]
    async fn other_routes_401_without_token_with_v1_envelope() {
        let res = build_router(state().await)
            .oneshot(
                Request::builder()
                    .uri("/api/v1/models")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        let json = body_json(res).await;
        assert_eq!(json["error"]["code"], "unauthorized");
    }

    #[tokio::test]
    async fn wrong_token_401_correct_token_reaches_404_envelope() {
        let router = build_router(state().await);
        let res = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/models")
                    .header("Authorization", "Bearer wrong")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

        let res = router
            .oneshot(
                Request::builder()
                    .uri("/api/v1/definitely-not-a-route")
                    .header("Authorization", "Bearer testtoken")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        // authorized but unknown route → v1 404 envelope
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        let json = body_json(res).await;
        assert_eq!(json["error"]["code"], "not_found");
    }

    #[tokio::test]
    async fn creative_is_default_denied_except_for_models() {
        let (state, _, creative) = capability_state().await;
        let router = build_router(state);
        let models = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/models")
                    .header("Authorization", format!("Bearer {creative}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(models.status(), StatusCode::OK);

        for (method, path) in [
            ("POST", "/api/v1/chat"),
            ("GET", "/api/v1/sessions"),
            ("POST", "/api/v1/runs"),
            ("GET", "/api/v1/peer/runs"),
            ("GET", "/api/v1/events"),
            ("GET", "/api/v1/approvals"),
            ("GET", "/api/v1/tools"),
            ("POST", "/api/v1/shutdown"),
        ] {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(path)
                        .header("Authorization", format!("Bearer {creative}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{method} {path}");
        }

        let unknown = router
            .oneshot(
                Request::builder()
                    .uri("/api/v1/future-unregistered-route")
                    .header("Authorization", format!("Bearer {creative}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    }

    async fn attached_probe() -> StatusCode {
        StatusCode::NO_CONTENT
    }

    fn concrete_path(template: &str) -> String {
        let mut path = String::new();
        let mut rest = template;
        while let Some(start) = rest.find('{') {
            path.push_str(&rest[..start]);
            let end = rest[start..].find('}').expect("template brace");
            path.push('x');
            rest = &rest[start + end + 1..];
        }
        path.push_str(rest);
        path
    }

    #[tokio::test]
    async fn creative_is_refused_on_every_mapped_route_except_models() {
        let (state, _host, creative) = capability_state().await;
        let router = router_with_comm(state);
        for policy in crate::capabilities::policies() {
            // Docked-only routes are not on the business router; the docked
            // router's own tests cover Creative there.
            if policy.operation == crate::capabilities::Operation::DiagnosticRead {
                continue;
            }
            // X11-2g: this POST is on the catalog so the route scan matches,
            // and capabilities mode answers 404 instead of the creative 403.
            if policy.method == "POST" && policy.template == "/api/v1/inbound/events" {
                continue;
            }
            // The bridge mint route is dark unless A24_DEV_TOOL=1. A live
            // token then gets 404, same as the wechat POST above.
            if policy.method == "POST" && policy.template == "/api/v1/capabilities/bridge" {
                continue;
            }
            let path = concrete_path(policy.template);
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method(policy.method)
                        .uri(&path)
                        .header(header::AUTHORIZATION, format!("Bearer {creative}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let expect = if policy.template == "/api/v1/models"
                || policy.level == crate::capabilities::AssuranceLevel::Public
            {
                StatusCode::OK
            } else {
                StatusCode::FORBIDDEN
            };
            assert_eq!(response.status(), expect, "{} {path}", policy.method);
        }
    }

    #[tokio::test]
    async fn capability_head_follows_get_and_does_not_skip_auth() {
        let (state, _host, creative) = capability_state().await;
        let router = build_router(state);

        let health = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::HEAD)
                    .uri("/api/v1/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(health.status(), StatusCode::OK);

        let models = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::HEAD)
                    .uri("/api/v1/models")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(models.status(), StatusCode::UNAUTHORIZED);

        let creative_models = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::HEAD)
                    .uri("/api/v1/models")
                    .header(header::AUTHORIZATION, format!("Bearer {creative}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(creative_models.status(), StatusCode::FORBIDDEN);

        let usage = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::HEAD)
                    .uri("/api/v1/usage")
                    .header(header::AUTHORIZATION, format!("Bearer {creative}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(usage.status(), StatusCode::FORBIDDEN);

        let post_only = router
            .oneshot(
                Request::builder()
                    .method(Method::HEAD)
                    .uri("/api/v1/chat")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(post_only.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn capability_missing_invalid_and_revoked_tokens_cannot_enumerate_routes() {
        let mut app = state().await;
        let now = crate::capabilities::unix_now();
        let store = crate::capabilities::CapabilityStore::new("daemon-test");
        let live = store
            .mint_product_host("live-host", Duration::from_secs(3_600), now)
            .unwrap();
        let (live_bearer, _, _) = live.into_bearer_parts();
        let revoked = store
            .mint_product_host("revoked-host", Duration::from_secs(3_600), now)
            .unwrap();
        let (revoked_bearer, revoked_id, _) = revoked.into_bearer_parts();
        assert!(store.revoke_by_id(&revoked_id));
        app.enable_capability_auth(store);
        let router = build_router(app);
        let invalid = "00".repeat(32);
        let probes = [
            (Method::POST, "/api/v1/models"),
            (Method::GET, "/api/v1/not-a-route"),
        ];
        let tokens: [(&str, Option<&str>); 3] = [
            ("missing", None),
            ("invalid", Some(invalid.as_str())),
            ("revoked", Some(revoked_bearer.as_str())),
        ];
        for (label, token) in tokens {
            for (method, path) in &probes {
                let mut request = Request::builder().method(method.clone()).uri(*path);
                if let Some(token) = token {
                    request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
                }
                let response = router
                    .clone()
                    .oneshot(request.body(Body::empty()).unwrap())
                    .await
                    .unwrap();
                assert_eq!(
                    response.status(),
                    StatusCode::UNAUTHORIZED,
                    "{label} {method} {path}"
                );
                assert_eq!(
                    body_json(response).await["error"]["code"],
                    "unauthorized",
                    "{label} {method} {path}"
                );
            }
        }

        for method in [Method::GET, Method::HEAD] {
            let health = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method.clone())
                        .uri("/api/v1/health")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(health.status(), StatusCode::OK, "{method} /api/v1/health");
        }

        let denied_method = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/models")
                    .header(header::AUTHORIZATION, format!("Bearer {live_bearer}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(denied_method.status(), StatusCode::METHOD_NOT_ALLOWED);

        let unknown = router
            .oneshot(
                Request::builder()
                    .uri("/api/v1/not-a-route")
                    .header(header::AUTHORIZATION, format!("Bearer {live_bearer}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
        assert_eq!(body_json(unknown).await["error"]["code"], "not_found");
    }

    #[tokio::test]
    async fn capability_unknown_method_is_405_and_unknown_path_does_not_run_fallback() {
        let (mut state, host, creative) = capability_state().await;
        let post_health = build_router(state.clone())
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/health")
                    .header(header::AUTHORIZATION, format!("Bearer {host}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(post_health.status(), StatusCode::METHOD_NOT_ALLOWED);

        state.module_namespaces = Arc::new(BTreeSet::from(["/api/v1/probe".to_owned()]));
        let router = Router::new()
            .route("/api/v1/mounted/secret", get(attached_probe))
            .fallback(attached_probe)
            .layer(middleware::from_fn_with_state(state, auth));

        let mounted = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/mounted/secret")
                    .header(header::AUTHORIZATION, format!("Bearer {host}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(mounted.status(), StatusCode::NO_CONTENT);
        let creative_mounted = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/mounted/secret")
                    .header(header::AUTHORIZATION, format!("Bearer {creative}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(creative_mounted.status(), StatusCode::FORBIDDEN);

        let namespace = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/probe/secret")
                    .header(header::AUTHORIZATION, format!("Bearer {host}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(namespace.status(), StatusCode::NO_CONTENT);

        for path in ["/api/v1/probe-x", "/api/v1/missing"] {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .header(header::AUTHORIZATION, format!("Bearer {host}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
            assert_eq!(body_json(response).await["error"]["code"], "not_found");
        }
    }

    #[tokio::test]
    async fn legacy_head_health_stays_unauthorized() {
        let res = build_router(state().await)
            .oneshot(
                Request::builder()
                    .method(Method::HEAD)
                    .uri("/api/v1/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

