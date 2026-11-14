    use super::*;
    use axum::http::Method;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    // These kernel paths do not yet have OpenAPI entries. Keep each reason and
    // owning follow-up beside the path: M2-02/M2-04 remove their entries when
    // those contracts land; the remaining exceptions have separate owners.
    const UNDOCUMENTED_KERNEL_ROUTES: &[&str] = &[
        "/capabilities/creative", // OD-M11 (PAUSED): capability contract is out of scope.
        "/events", // WebSocket protocol is documented by protocol/events.schema.json.
        // KB-HOST-01: the desktop host's private interface (UI-BRIDGE U1.5), not a client contract.
        "/host/knowledge/bindings/{binding_id}",
        "/host/knowledge/bindings/{binding_id}/credential",
        // CORE-2 AgentEar S3: the desktop main process's confirmation channel, ProductHost
        // only (slice §3.4) — private to the desktop, not a client contract.
        "/presence/challenges/next",
        "/presence/challenges/{id}",
    ];

    // Module-owned contract paths appear in OpenAPI but are not literal routes
    // in build_router_with_modules. `/sin90/*` is an external contract
    // (ADR-045 D5, Q4 (iii)): it stays here, tagged `x-external-contract`, and the
    // implementation is owned by iDoris-ai/Sin90.
    const MODULE_SURFACE_PATHS: &[&str] = &[
        "/sin90/directions",
        "/sin90/schedule-blocks",
        "/sin90/schedule-blocks/{id}",
        "/sin90/proposals",
        "/sin90/proposals/{id}",
        "/sin90/proposals/{id}/accept",
        "/sin90/attention",
        // Documenting OS (ADR-DOC-02 §4), served by rust/apps/agent24-documents.
        "/documents/capabilities",
        "/documents/documents",
        "/documents/documents/{document_id}",
        "/documents/uploads",
        "/documents/uploads/{upload_id}/chunks",
        "/documents/imports",
        "/documents/jobs/{job_id}",
        "/documents/jobs/{job_id}/cancel",
        "/documents/jobs/{job_id}/retry",
        "/documents/documents/{document_id}/revisions/{revision}/pages/{page}",
        "/documents/documents/{document_id}/revisions/{revision}/text",
        "/documents/documents/{document_id}/find",
        "/documents/documents/{document_id}/extractions",
        "/documents/extractions/{extraction_id}",
    ];

    // Module-internal channel (ADR-045 D6): `_a24/*` is kernel <-> module plumbing,
    // not a client API. It is deliberately not in OpenAPI (api-client would
    // generate it as a callable surface) and the kernel proxy answers 404 to
    // clients for it (`agent24-os-proto` RESERVED_SEGMENT). Every `_a24` literal
    // in a module source must be listed here, so the gate does not have a blind
    // spot for routes that are neither in OpenAPI nor in an exemption table.
    const MODULE_INTERNAL_CHANNEL_ROUTES: &[(&str, &str)] = &[
        ("POST", "/_a24/tools/{op}"),
        ("POST", "/_a24/authorization/revoked"),
    ];

    const DOCUMENTS_ROUTE_SOURCE: (&str, &str) = (
        include_str!("../../../../agent24-documents/src/lib.rs"),
        "pub fn router(",
    );

    // COMM routes are mounted by server::serve, outside the scanned builder, so
    // their literals are scanned from the agent24-comm sources instead
    // (`comm_route_literals`); they are covered by the same gate, no exemption.
    const COMM_ROUTE_SOURCES: [(&str, &str); 3] = [
        (
            include_str!("../../../../../crates/agent24-comm/src/router.rs"),
            "pub fn router(",
        ),
        (
            include_str!("../../../../../crates/agent24-comm/src/groupchat.rs"),
            "pub(crate) fn routes(",
        ),
        (include_str!("../../comm_routes.rs"), "fn runtime_router("),
    ];

    const HTTP_METHODS: [&str; 7] = ["get", "put", "post", "delete", "patch", "head", "options"];

    /// Calls other than the HTTP methods that may follow a route at its top level: they
    /// configure the handler, they do not add or hide a method.
    const ROUTE_MODIFIERS: [&str; 3] = ["layer", "route_layer", "with_state"];

    /// `(METHOD, path)` pairs for the `.route("<path>", <handlers>)` literals inside the
    /// function starting at `anchor`. Methods are the `get(..)`/`post(..)`/... calls at the
    /// top level of the route's own argument list, so `get(a).post(b)` yields both while a
    /// nested `.layer(..)` argument is not read. The scan is fail-closed: a non-literal path,
    /// a route with no readable method, or any other top-level call that could add or hide a
    /// method (`any`, `on`, `merge`, `trace`, ...) panics instead of dropping out of the gate.
    fn route_ops(source: &str, anchor: &str) -> Vec<(String, String)> {
        let start = source
            .find(anchor)
            .unwrap_or_else(|| panic!("{anchor} must exist"));
        let body = &source[start..];
        let body = &body[..body
            .find("\n}\n")
            .expect("function must be brace-terminated")];
        let mut ops = Vec::new();
        for (offset, _) in body.match_indices(".route(") {
            let args = &body[offset + ".route(".len()..];
            let trimmed = args.trim_start();
            assert!(
                trimmed.starts_with('"'),
                "route path must be a string literal, got: {:.40}",
                trimmed
            );
            let open = args.len() - trimmed.len() + 1;
            let path_len = args[open..].find('"').expect("route path literal end");
            let path = args[open..open + path_len].to_owned();
            let chars: Vec<char> = args[open + path_len + 1..].chars().collect();
            let (mut depth, mut i, mut found) = (1_i32, 0, 0);
            while i < chars.len() && depth > 0 {
                let c = chars[i];
                if c == '/' && chars.get(i + 1) == Some(&'/') {
                    while i < chars.len() && chars[i] != '\n' {
                        i += 1;
                    }
                    continue;
                }
                if c == '/' && chars.get(i + 1) == Some(&'*') {
                    i += 2;
                    while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                        i += 1;
                    }
                    i += 2;
                    continue;
                }
                if c == '"' {
                    i += 1;
                    while i < chars.len() && chars[i] != '"' {
                        i += if chars[i] == '\\' { 2 } else { 1 };
                    }
                    i += 1;
                    continue;
                }
                if c == '(' {
                    depth += 1;
                } else if c == ')' {
                    depth -= 1;
                } else if c.is_ascii_alphabetic() || c == '_' {
                    let begin = i;
                    while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                        i += 1;
                    }
                    let ident: String = chars[begin..i].iter().collect();
                    let mut j = i;
                    while j < chars.len() && chars[j].is_whitespace() {
                        j += 1;
                    }
                    if depth == 1 && chars.get(j) == Some(&'(') {
                        if HTTP_METHODS.contains(&ident.as_str()) {
                            ops.push((ident.to_ascii_uppercase(), path.clone()));
                            found += 1;
                        } else {
                            assert!(
                                ROUTE_MODIFIERS.contains(&ident.as_str()),
                                "route {path}: unsupported top-level call `{ident}(..)`; the \
                                 gate cannot tell which methods it serves"
                            );
                        }
                    }
                    continue;
                }
                i += 1;
            }
            assert!(
                found > 0,
                "route {path} has no readable get/post/... handler"
            );
        }
        ops
    }

    /// `"METHOD path"` for every operation under `paths` in the OpenAPI document.
    fn openapi_ops(openapi_source: &str) -> std::collections::BTreeSet<(String, String)> {
        let document: serde_yaml::Value =
            serde_yaml::from_str(openapi_source).expect("protocol/openapi.yaml must parse as YAML");
        let paths = document
            .get("paths")
            .and_then(serde_yaml::Value::as_mapping)
            .expect("OpenAPI document must contain a paths mapping");
        let mut ops = std::collections::BTreeSet::new();
        for (path, item) in paths {
            let path = path.as_str().expect("path keys are strings");
            for method in HTTP_METHODS {
                if item.get(method).is_some() {
                    ops.insert((method.to_ascii_uppercase(), path.to_owned()));
                }
            }
        }
        ops
    }

    fn route_openapi_mismatches(
        server_source: &str,
        openapi_source: &str,
    ) -> (Vec<String>, Vec<String>) {
        use std::collections::BTreeSet;

        let mut routes: BTreeSet<(String, String)> = BTreeSet::new();
        for (method, path) in route_ops(server_source, "pub fn build_router_with_modules") {
            if let Some(path) = path.strip_prefix("/api/v1") {
                routes.insert((method, path.to_owned()));
            }
        }
        // CORE-5f-2: the docked router is a second router, scanned the same way.
        for (method, path) in route_ops(include_str!("../../docked_router.rs"), "pub fn docked_router") {
            if let Some(path) = path.strip_prefix("/api/v1") {
                routes.insert((method, path.to_owned()));
            }
        }
        assert!(
            routes.len() >= 20,
            "route scan found only {} operations; build_router_with_modules may have changed shape",
            routes.len()
        );

        let openapi = openapi_ops(openapi_source);

        let undocumented: BTreeSet<&str> = UNDOCUMENTED_KERNEL_ROUTES.iter().copied().collect();
        let module_paths: BTreeSet<&str> = MODULE_SURFACE_PATHS.iter().copied().collect();
        let mut comm = 0;
        for (source, anchor) in COMM_ROUTE_SOURCES {
            for (method, path) in route_ops(source, anchor) {
                routes.insert((method, format!("/comm{path}")));
                comm += 1;
            }
        }
        assert!(comm >= 20, "comm route scan found only {comm} operations");
        let expected_openapi: BTreeSet<(String, String)> = routes
            .iter()
            .filter(|(_, path)| !undocumented.contains(path.as_str()))
            .cloned()
            .collect();
        let actual_openapi: BTreeSet<(String, String)> = openapi
            .iter()
            .filter(|(_, path)| !module_paths.contains(path.as_str()))
            .cloned()
            .collect();

        let missing_from_openapi = expected_openapi
            .difference(&actual_openapi)
            .map(|(method, path)| format!("{method} {path}"))
            .collect();
        let extra_in_openapi = actual_openapi
            .difference(&expected_openapi)
            .map(|(method, path)| format!("{method} {path}"))
            .collect();
        (missing_from_openapi, extra_in_openapi)
    }

    fn assert_route_openapi_covered(server_source: &str, openapi_source: &str) {
        let (missing_from_openapi, extra_in_openapi) =
            route_openapi_mismatches(server_source, openapi_source);
        assert!(
            missing_from_openapi.is_empty(),
            "kernel operations (METHOD path) missing from OpenAPI (except UNDOCUMENTED_KERNEL_ROUTES): {missing_from_openapi:?}"
        );
        assert!(
            extra_in_openapi.is_empty(),
            "OpenAPI operations (METHOD path) without a kernel route (except MODULE_SURFACE_PATHS): {extra_in_openapi:?}"
        );
    }

    #[test]
    fn kernel_routes_and_openapi_paths_have_no_unlisted_drift() {
        let routes = include_str!("../../server.rs");
        let openapi = include_str!("../../../../../../protocol/openapi.yaml");
        assert_route_openapi_covered(routes, openapi);
    }

    #[test]
    fn route_openapi_gate_rejects_an_undocumented_kernel_route() {
        let routes = include_str!("../../server.rs").replace(
            ".route(\"/api/v1/health\"",
            ".route(\"/api/v1/not-in-openapi\", get(health))\n        .route(\"/api/v1/health\"",
        );
        let openapi = include_str!("../../../../../../protocol/openapi.yaml");
        let (missing_from_openapi, extra_in_openapi) = route_openapi_mismatches(&routes, openapi);
        assert_eq!(missing_from_openapi, ["GET /not-in-openapi"]);
        assert!(extra_in_openapi.is_empty());
    }

    /// The comm scan reads `.route(` literals out of two functions. A route
    /// added through `.merge(`/`.nest(` of another module (or a second
    /// `routes()`) would be invisible to it AND absent from OpenAPI, i.e. the
    /// gate would pass an unregistered route. Pin the composition instead.
    fn comm_composition_is_scanned(router_source: &str) -> Result<(), String> {
        let start = router_source
            .find("pub fn router(")
            .ok_or("pub fn router( must exist")?;
        let body = &router_source[start..];
        let body = &body[..body
            .find("\n}\n")
            .ok_or("router must be brace-terminated")?];
        let composed: Vec<&str> = body
            .match_indices(".merge(")
            .chain(body.match_indices(".nest("))
            .chain(body.match_indices(".fallback"))
            .chain(body.match_indices(".route_service("))
            .map(|(i, _)| body[i..].lines().next().unwrap_or("").trim())
            .collect();
        if composed == [".merge(crate::groupchat::routes())"] {
            Ok(())
        } else {
            Err(format!(
                "comm router composes unscanned routes; extend COMM_ROUTE_SOURCES: {composed:?}"
            ))
        }
    }

    #[test]
    fn comm_route_scan_covers_every_composed_module() {
        let real = COMM_ROUTE_SOURCES[0].0;
        comm_composition_is_scanned(real).unwrap();
        let sneaky = real.replace(
            ".merge(crate::groupchat::routes())",
            ".merge(crate::groupchat::routes())\n        .merge(crate::other::routes())",
        );
        assert!(comm_composition_is_scanned(&sneaky).is_err());
        let nested = real.replace(
            ".merge(crate::groupchat::routes())",
            ".merge(crate::groupchat::routes())\n        .nest(\"/x\", y())",
        );
        assert!(comm_composition_is_scanned(&nested).is_err());
    }

    #[test]
    fn route_openapi_gate_covers_comm_routes() {
        let routes = include_str!("../../server.rs");
        let openapi = include_str!("../../../../../../protocol/openapi.yaml")
            .replace("  /comm/groupchat:\n", "  /comm/groupchat-gone:\n");
        let (missing_from_openapi, extra_in_openapi) = route_openapi_mismatches(routes, &openapi);
        assert_eq!(
            missing_from_openapi,
            ["GET /comm/groupchat", "POST /comm/groupchat"]
        );
        assert_eq!(
            extra_in_openapi,
            ["GET /comm/groupchat-gone", "POST /comm/groupchat-gone"]
        );
    }

    #[test]
    fn route_openapi_gate_rejects_an_unimplemented_openapi_path() {
        let routes = include_str!("../../server.rs");
        let openapi = include_str!("../../../../../../protocol/openapi.yaml").replace(
            "paths:\n",
            "paths:\n  /not-in-router:\n    get:\n      responses: {}\n",
        );
        let (missing_from_openapi, extra_in_openapi) = route_openapi_mismatches(routes, &openapi);
        assert!(missing_from_openapi.is_empty());
        assert_eq!(extra_in_openapi, ["GET /not-in-router"]);
    }

    /// Positive control for the method-level gate: dropping ONE method of a path that
    /// still has another method must turn the gate red. A path-only comparison cannot
    /// see this, because `/approvals/{id}` stays present either way.
    #[test]
    fn route_openapi_gate_rejects_a_missing_method_on_a_present_path() {
        let routes = include_str!("../../server.rs");
        let openapi = include_str!("../../../../../../protocol/openapi.yaml");
        assert_route_openapi_covered(routes, openapi);

        let without_get = openapi.replace(
            "    get:\n      operationId: getApproval\n",
            "    x-dropped:\n      operationId: getApproval\n",
        );
        assert_ne!(without_get, openapi, "the getApproval operation must exist");
        let (missing, extra) = route_openapi_mismatches(routes, &without_get);
        assert_eq!(missing, ["GET /approvals/{id}"]);
        assert!(extra.is_empty());
    }

    /// The reverse direction: an OpenAPI method the router does not serve.
    #[test]
    fn route_openapi_gate_rejects_an_unserved_method_on_a_present_path() {
        let original = include_str!("../../server.rs");
        let routes = original.replace(
            "get(crate::approvals::get_approval).post(crate::approvals::decide_approval)",
            "post(crate::approvals::decide_approval)",
        );
        assert_ne!(routes, original, "the approvals route literal must exist");
        let openapi = include_str!("../../../../../../protocol/openapi.yaml");
        let (missing, extra) = route_openapi_mismatches(&routes, openapi);
        assert!(missing.is_empty());
        assert_eq!(extra, ["GET /approvals/{id}"]);
    }

    /// B1: the registered `GET /api/v1/approvals/{id}` is a documented operation.
    #[test]
    fn get_approval_by_id_is_documented_and_registered() {
        let openapi = openapi_ops(include_str!("../../../../../../protocol/openapi.yaml"));
        assert!(openapi.contains(&("GET".to_owned(), "/approvals/{id}".to_owned())));
        let routes = route_ops(
            include_str!("../../server.rs"),
            "pub fn build_router_with_modules",
        );
        assert!(routes.contains(&("GET".to_owned(), "/api/v1/approvals/{id}".to_owned())));
    }

    /// The method reader sees chained handlers, ignores nested arguments, and refuses a
    /// route it cannot read instead of dropping it from the gate.
    #[test]
    fn route_ops_reads_chained_methods_and_rejects_unreadable_routes() {
        let src = "pub fn r() {\n    x\n        .route(\"/a/{id}\", get(h).post(g))\n        .route(\"/b\", put(h).layer(Limit::max(1)))\n        .route(\"/c\", axum::routing::delete(h))\n}\n";
        assert_eq!(
            route_ops(src, "pub fn r("),
            [
                ("GET".to_owned(), "/a/{id}".to_owned()),
                ("POST".to_owned(), "/a/{id}".to_owned()),
                ("PUT".to_owned(), "/b".to_owned()),
                ("DELETE".to_owned(), "/c".to_owned()),
            ]
        );
        for bad in [
            ".route(\"/d\", any(h))",
            ".route(\"/d\", any(h).post(g))",
            ".route(\"/d\", get(h).on(MethodFilter::DELETE, g))",
            ".route(\"/d\", get(h).merge(other()))",
            ".route(\"/d\", get(h).trace(g))",
            ".route(PATH, get(h))\n        .route(\"/b\", get(h))",
        ] {
            let src = format!("pub fn r() {{\n    x\n        {bad}\n}}\n");
            assert!(
                std::panic::catch_unwind(|| route_ops(&src, "pub fn r(")).is_err(),
                "{bad} must be refused"
            );
        }
        // comments and string literals are not code
        let noisy = "pub fn r() {\n    x.route(\"/e\", get(h) /* any(x) */ // on(y)\n        .layer(L::new(\"on(\")))\n}\n";
        assert_eq!(
            route_ops(noisy, "pub fn r("),
            [("GET".to_owned(), "/e".to_owned())]
        );
    }

    /// B3 / ADR-045 D6: the `_a24/*` routes of `router()` in the documents module are listed
    /// as module-internal channels, in both directions (a new one fails, a stale entry
    /// fails). Separately, the count of `"/_a24` literals in the whole non-test source of
    /// `agent24-documents/src/lib.rs` must equal the table size, so an `_a24` route added
    /// outside `router()` is caught too. They never appear in OpenAPI.
    /// (Other files of the documents crate are not scanned; `_a24` routes live in `lib.rs`.)
    #[test]
    fn module_internal_channel_routes_match_documents_source() {
        let scan = |source: &str| -> Vec<(String, String)> {
            let mut ops: Vec<_> = route_ops(source, DOCUMENTS_ROUTE_SOURCE.1)
                .into_iter()
                .filter(|(_, path)| path.split('/').any(|seg| seg.eq_ignore_ascii_case("_a24")))
                .collect();
            ops.sort();
            ops
        };
        let mut listed: Vec<(String, String)> = MODULE_INTERNAL_CHANNEL_ROUTES
            .iter()
            .map(|(m, p)| ((*m).to_owned(), (*p).to_owned()))
            .collect();
        listed.sort();
        assert_eq!(scan(DOCUMENTS_ROUTE_SOURCE.0), listed);

        // positive controls: an unlisted `_a24` route, and a listed route that is gone
        let extra = DOCUMENTS_ROUTE_SOURCE.0.replace(
            ".route(\"/_a24/authorization/revoked\"",
            ".route(\"/_a24/new/thing\", post(authorization::revoked))\n        .route(\"/_a24/authorization/revoked\"",
        );
        assert_ne!(extra, DOCUMENTS_ROUTE_SOURCE.0);
        assert_ne!(scan(&extra), listed);
        let gone = DOCUMENTS_ROUTE_SOURCE
            .0
            .replace("\"/_a24/authorization/revoked\"", "\"/gone\"");
        assert_ne!(gone, DOCUMENTS_ROUTE_SOURCE.0);
        assert_ne!(scan(&gone), listed);

        let non_test = DOCUMENTS_ROUTE_SOURCE
            .0
            .split("#[cfg(test)]")
            .next()
            .expect("split yields at least one part");
        assert_eq!(
            non_test.matches("\"/_a24").count(),
            MODULE_INTERNAL_CHANNEL_ROUTES.len(),
            "an `_a24` literal outside the table (or outside router()) in agent24-documents/src/lib.rs"
        );

        // every `_a24` route is kept out of the client contract
        let openapi = include_str!("../../../../../../protocol/openapi.yaml");
        assert!(
            openapi_ops(openapi)
                .iter()
                .all(|(_, path)| !path.contains("_a24")),
            "OpenAPI must not contain _a24 paths"
        );
        // the scan sees the module's ordinary routes too
        assert!(
            route_ops(DOCUMENTS_ROUTE_SOURCE.0, DOCUMENTS_ROUTE_SOURCE.1)
                .iter()
                .any(|(_, p)| p == "/documents/{document_id}"),
        );
    }

    /// D5 / Q4 (iii): every `/sin90/*` operation is marked as an external contract, so the
    /// exemption in MODULE_SURFACE_PATHS is visible in the contract itself.
    #[test]
    fn sin90_operations_are_marked_external_contract() {
        let doc: serde_yaml::Value =
            serde_yaml::from_str(include_str!("../../../../../../protocol/openapi.yaml")).unwrap();
        let paths = doc
            .get("paths")
            .and_then(serde_yaml::Value::as_mapping)
            .unwrap();
        let mut seen = 0;
        for (path, item) in paths {
            let path = path.as_str().unwrap();
            if !path.starts_with("/sin90/") {
                continue;
            }
            for method in HTTP_METHODS {
                if let Some(op) = item.get(method) {
                    seen += 1;
                    assert_eq!(
                        op.get("x-external-contract")
                            .and_then(serde_yaml::Value::as_str),
                        Some("iDoris-ai/Sin90"),
                        "{method} {path}"
                    );
                }
            }
        }
        assert_eq!(seen, 10, "ADR-045 D5 counts 10 sin90 operations");
    }

