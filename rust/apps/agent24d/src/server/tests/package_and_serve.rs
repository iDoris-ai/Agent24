
    #[test]
    fn a_refused_documents_package_is_recorded_under_that_name() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("documents");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("domain-os.yml"),
            "name: documents\nversion: \"1.2.3\"\nroute_namespace: /api/v1/documents\n\
             event_module: documents\ndata_dir: ~/.agent24/os/documents/\n\
             kernel_capabilities: []\nimpl_kind: out_of_process_provider\n\
             spawn:\n  command: bin/documents\n",
        )
        .unwrap();
        let scan = agent24_os_packages::discovery::scan(
            root.path(),
            &tokio_util::sync::CancellationToken::new(),
        );
        let refusals: Vec<_> = scan
            .refused
            .into_iter()
            .filter(|refused| refused.integrity.is_some() && refused.dir != root.path())
            .collect();
        let (rows, dirs) = super::package_dirs_for_startup(&[], &refusals, &[]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "documents");
        assert_eq!(
            dirs.get("documents").map(std::path::PathBuf::as_path),
            Some(dir.as_path())
        );
    }

    #[test]
    fn workspace_recovery_clock_preserves_fractional_millis() {
        let now = std::time::UNIX_EPOCH + std::time::Duration::from_millis(1_900);
        let instant = super::workspace_recovery_instant_at(now).unwrap();
        assert_eq!(instant.as_str(), "1970-01-01T00:00:01.900Z");
    }

    // ---- ME4-4.2.2b2 H1 (Opus review round on top of `bb6fb0e`) -------------

    /// A real end-to-end daemon test
    /// (`apps/agent24d/tests/me4_model_shutdown_wiring.rs`) empirically
    /// CANNOT reliably pin this one line: `Shutdown::request()` also revokes
    /// the module's `Generation` as part of stopping it — design
    /// §3.3's OWN "代次撤销" row, pre-existing `os-proto` machinery that
    /// independently cancels an in-flight model call within tens of
    /// milliseconds of the SAME `spawn_cancel_root(shutdown.calls_cut_off())`
    /// cut-off. Measured with a real subprocess module (SIGTERM-ignoring, one
    /// proxied request kept in flight so its own drain doesn't finish early)
    /// and a real hung TCP provider: correct code closes the provider
    /// connection ~630–670ms after `POST /api/v1/shutdown`; wiring the cancel
    /// root to `CancellationToken::new()` (never fires) still closes it at
    /// ~670–760ms via the OTHER path — overlapping ranges, not a reliable
    /// red/green signal for CI. This structural test is the deterministic
    /// fallback the design's own review round asked for when the daemon-level
    /// test can't be made to pin it: it pins the EXACT call, so a mutation to
    /// either half — the constructor (`CancellationToken::new()` instead of
    /// `spawn_cancel_root`) or the argument (anything other than
    /// `shutdown.modules_cut_off()`, e.g. `shutdown.token().cancelled()` to
    /// fire at the START of shutdown instead of at cut-off) — turns it red.
    #[test]
    fn the_model_callback_cancel_root_is_spawned_from_calls_cut_off() {
        // Scoped to `serve()`'s OWN body — never to the whole file, which
        // would make this test tautological (the string above, in this
        // test's own source, would always make a whole-file `.contains`
        // pass regardless of what `serve()` actually does). Same technique
        // as `module_routes_are_behind_kernel_auth`'s sibling
        // `build_router_with_modules` scan below: find the function, cut at
        // the first column-zero `}` (every brace inside a rustfmt'd function
        // body is indented).
        let server = include_str!("../serve.rs");
        let steps = include_str!("../../serve_steps.rs");
        let serve = &server[server
            .find("pub async fn serve(")
            .expect("serve must exist")..];
        let serve = &serve[..serve.find("\n}\n").expect("serve must be brace-terminated")];
        assert!(
            serve.contains("start_usage_and_model_deps("),
            "serve() must build model deps through start_usage_and_model_deps"
        );
        let marker = "fn start_usage_and_model_deps(";
        let start = steps.find(marker).expect("start_usage_and_model_deps");
        let body = &steps[start..];
        let body = &body[..body
            .find("\n}\n")
            .expect("function must be brace-terminated")];
        assert!(
            body.contains("spawn_cancel_root(shutdown.calls_cut_off())"),
            "serve() must build ModelCallbackDeps.cancel_root as \
             spawn_cancel_root(shutdown.calls_cut_off()) — design \
             docs/design/ME4-S2-model-callback.md §3.3"
        );
    }

    /// #1202 G6 (Codex A5c-2a round 2): `serve` builds each module tool's
    /// authorization through `tool_authorization`, which refuses a module
    /// not on the grants protocol, never `store_authorization` directly,
    /// which would let a grant from an older daemon hold. S2-1: that absence
    /// covers `serve()` itself and all of `serve_steps.rs`. Scanned with
    /// `//`-comments stripped, as
    /// `stop_usage_writer_is_called_after_the_supervisors_have_stopped`.
    #[test]
    fn serve_authorizes_module_tools_by_protocol() {
        let server = include_str!("../serve.rs");
        let steps = include_str!("../../serve_steps.rs");
        let serve = &server[server
            .find("pub async fn serve(")
            .expect("serve must exist")..];
        let serve = &serve[..serve.find("\n}\n").expect("serve must be brace-terminated")];
        let tools_call = serve
            .find("register_startup_tools(")
            .expect("serve must register tools");
        let publish_call = serve
            .find("publish_mount_and_attach(")
            .expect("serve must publish the mount");
        assert!(
            tools_call < publish_call,
            "tool authorization is built before the grant-expiry notifier"
        );
        let step = |name: &str| {
            let marker = format!("fn {name}(");
            let start = steps.find(&marker).unwrap_or_else(|| panic!("{name}"));
            let rest = &steps[start..];
            rest[..rest
                .find("\n}\n")
                .expect("function must be brace-terminated")]
                .to_owned()
        };
        let code = |src: &str| {
            src.lines()
                .map(|line| line.split("//").next().unwrap_or(""))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let tools = code(&step("register_startup_tools"));
        let publish = code(&step("publish_mount_and_attach"));
        assert!(tools.contains("crate::module_tool_runtime::tool_authorization("));
        // The notifier asks the same cards (A5c-2b).
        assert!(publish.contains("cards.on_grants_protocol(module)"));
        assert!(
            !code(serve).contains("store_authorization("),
            "serve() must not call store_authorization directly"
        );
        assert!(
            !code(steps).contains("store_authorization("),
            "serve_steps must not call store_authorization directly"
        );
    }

    /// #1202 G6 (A5c-2b): a module is on the grants protocol when a tool of
    /// it takes grants; one whose tools do not, or with none, is not.
    #[tokio::test]
    async fn the_cards_say_which_modules_are_on_the_grants_protocol() {
        let dir = tempfile::tempdir().unwrap();
        let running = StdArc::new(std::sync::atomic::AtomicBool::new(true));
        let (state, _) = decision_state(&running, &dir.path().join("os.json")).await;
        let cards = state.module_tool_cards.unwrap();
        assert!(cards.on_grants_protocol("doc-module"));
        assert!(!cards.on_grants_protocol("old-module"));
        assert!(!cards.on_grants_protocol("absent"));
    }

    // ---- ME4-4.2.3b (design §6.3/v3.1 M-2, J19 variant 2a) -------------------

    /// J19's structural half: `stop_usage_writer` must be called from
    /// `serve()`'s MAIN shutdown path (the one every real shutdown takes)
    /// strictly AFTER `let _ = stopping.await;` — the point at which the
    /// out-of-process supervisors have finished stopping, so every in-flight
    /// call's outcome (including ones the cut-off cancelled) has already
    /// either reached the recorder's channel or never will. Calling it
    /// earlier would race the very cancellations it exists to wait out; not
    /// calling it at all is the bug this test exists to catch.
    ///
    /// Review, M1: two hardenings over the original version of this test —
    /// (a) `//`-comment lines are stripped from the scanned source FIRST, so
    /// commenting the call out (and `drop`-ping the values it would have
    /// consumed, to keep the function compiling) does not fool a plain
    /// substring search; (b) the match is the FULL call with its exact
    /// arguments, not just the bare function name, so swapping in a
    /// differently-shaped call (wrong argument, wrong order) also turns this
    /// red. Mutation verified: replacing the real call with
    /// `// crate::usage_recorder::stop_usage_writer(...)` plus
    /// `drop(usage_recorder); drop(usage_writer);` turns this red; reverting
    /// turns it green again.
    #[test]
    fn stop_usage_writer_is_called_after_the_supervisors_have_stopped() {
        let src = include_str!("../serve.rs");
        let start = src.find("pub async fn serve(").expect("serve must exist");
        let body = &src[start..];
        let end = body
            .find("\n}\n")
            .expect("function must be brace-terminated");
        let body = &body[..end];
        // Strip `//`-comments line by line (this scans ONLY `serve`'s own
        // source, which has no `//` inside a string literal near this area,
        // so a naive split is safe here) — a commented-out call must be
        // invisible to the search below.
        let code_only: String = body
            .lines()
            .map(|line| line.split("//").next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n");
        // Unique to the main shutdown path — the two early-return copies
        // (during startup) never call `shutdown.request()` right before
        // their own `stopping.await`.
        let stopping_at = code_only
            .find("shutdown.request();\n    let _ = stopping.await;")
            .expect("the main shutdown path must request, then wait for stopping");
        let after = &code_only[stopping_at..];
        // The exact rustfmt shape of the real call (verified against the
        // source at the time this test was written — a reformat that keeps
        // the same call would need this literal updated too, which is the
        // point: it is not just checking the function name).
        let exact_call = "crate::usage_recorder::stop_usage_writer(\n        usage_recorder,\n        \
                           usage_writer,\n        shutdown.deadlines().modules,\n    )\n    .await;";
        assert!(
            after.contains(exact_call),
            "serve()'s main shutdown path must call \
             crate::usage_recorder::stop_usage_writer(usage_recorder, usage_writer, \
             shutdown.deadlines().modules).await AFTER `let _ = stopping.await;` — design \
             docs/design/ME4-S2-model-callback.md §6.3/v3.1 M-2"
        );
    }

    // ---- ME-3a: the wiring itself, not just the scanner ---------------------

    /// T7b/ME-3e: a throwaway broker for `mount_all` tests that don't care
    /// about approval behavior — built on the SAME hub the test passes as
    /// `mount_all`'s `events`.
    async fn test_approval_broker(
        hub: &crate::events::EventsHub,
    ) -> std::sync::Arc<crate::module_approval_broker::ModuleApprovalBroker> {
        crate::module_approval_broker::ModuleApprovalBroker::new(
            agent24_store::Store::open_memory().await.unwrap(),
            hub.clone(),
        )
    }

    fn pkg(root: &std::path::Path, name: &str) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(agent24_os_packages::discovery::MANIFEST_FILE),
            format!(
                "name: {name}\nversion: \"0.1.0\"\nroute_namespace: /api/v1/{name}\n\
                 event_module: {name}\ndata_dir: ~/.agent24/os/{name}/\n\
                 kernel_capabilities: [events]\nimpl_kind: out_of_process_provider\n\
                 spawn:\n  command: bin/mod\n"
            ),
        )
        .unwrap();
        agent24_os_packages::test_support::seal_exit_stub(&dir, "bin/mod").unwrap();
    }

    /// Readiness and a shutdown request cannot both win: requested first, the
    /// daemon never says it is ready; ready first, the shutdown still happens
    /// (review of SUP-4, round 7).
    #[test]
    fn readiness_and_a_shutdown_request_decide_once() {
        let stopping_first = Shutdown::new(CancellationToken::new());
        stopping_first.token().cancel(); // no watchdog in a test process
        stopping_first
            .phase
            .store(STOPPING, std::sync::atomic::Ordering::SeqCst);
        assert!(
            !stopping_first.commit_ready(),
            "ready after a shutdown request"
        );

        let ready_first = Shutdown::new(CancellationToken::new());
        assert!(ready_first.commit_ready());
        assert!(!ready_first.commit_ready(), "ready twice");
        assert!(!ready_first.token().is_cancelled());
    }

    fn built_in(name: &str) -> crate::domain::Installed {
        crate::domain::Installed {
            name: name.to_owned(),
            version: "9.9.9".to_owned(),
            build: crate::domain::Build::InProcess(Box::new(|| {
                Err("built-in, not constructed in this test".to_owned())
            })),
        }
    }

    /// A scan thread that panics is a failed start, not an empty catalogue.
    /// An empty catalogue is what "no packages installed" looks like.
    #[tokio::test]
    async fn a_panicked_package_scan_is_a_startup_error() {
        let joined = tokio::task::spawn_blocking(|| {
            panic!("scan blew up");
        })
        .await;
        let err = super::catalogue_from_join(joined).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("discovering domain OS packages failed"),
            "{msg}"
        );
        assert!(msg.contains("panicked"), "{msg}");
    }

    /// A shutdown during discovery must not put the package on the catalogue.
    /// Passing `CancellationToken::new()` instead of the caller's token
    /// discovers `cos72` and turns this red.
    #[test]
    fn a_cancelled_discovery_does_not_catalogue_the_package() {
        let root = tempfile::tempdir().unwrap();
        pkg(root.path(), "cos72");
        let cancel = CancellationToken::new();
        cancel.cancel();
        let out = super::with_discovered(vec![built_in("sin90")], root.path(), &cancel);
        let names: Vec<&str> = out.catalogue.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["sin90"]);
    }

    #[test]
    fn a_package_placed_after_the_build_reaches_the_catalogue() {
        // The scanner having found it is not the same claim as the daemon having
        // USED it. This is the wiring, which was previously only assertable by
        // reading `serve`.
        let root = tempfile::tempdir().unwrap();
        pkg(root.path(), "cos72");

        let out = super::with_discovered(
            vec![built_in("sin90")],
            root.path(),
            &CancellationToken::new(),
        );
        let names: Vec<&str> = out.catalogue.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["sin90", "cos72"]);
    }

    /// FU-41's wiring. The library gained `ensure_packages_root`; a library
    /// function nobody calls closes nothing.
    ///
    /// The refusal must SKIP discovery and keep the built-ins — a daemon with
    /// only its compiled-in modules still works, while one that loaded a package
    /// from a directory anyone could write would be executing a program of their
    /// choosing (the manifest carries a `spawn` command as of ME-3b-3).
    #[test]
    fn a_world_writable_packages_root_is_not_read_at_all() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        pkg(root.path(), "cos72");
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o777)).unwrap();

        let out = super::with_discovered(
            vec![built_in("sin90")],
            root.path(),
            &CancellationToken::new(),
        );
        let names: Vec<&str> = out.catalogue.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["sin90"],
            "a package in an unsafe root was loaded"
        );

        // Control: the SAME root with the SAME package, tightened, is read. So
        // the skip above is about the mode — not about the package being
        // unreadable for some other reason, which would make this test pass for
        // a reason that has nothing to do with FU-41.
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let out = super::with_discovered(
            vec![built_in("sin90")],
            root.path(),
            &CancellationToken::new(),
        );
        let names: Vec<&str> = out.catalogue.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["sin90", "cos72"]);
    }

    #[test]
    fn a_disk_package_cannot_shadow_a_built_in_module() {
        // Order is the whole mechanism: `mount_all` claims names first-come,
        // first-served, so a disk package named `sin90` must land AFTER the
        // compiled-in one and lose. If discovery ever moved ahead of the built-ins
        // — or the mounter's claim became last-wins — a dropped-in directory could
        // take over a kernel module's namespace silently.
        let root = tempfile::tempdir().unwrap();
        pkg(root.path(), "sin90");

        let out = super::with_discovered(
            vec![built_in("sin90")],
            root.path(),
            &CancellationToken::new(),
        );
        assert_eq!(
            out.catalogue.len(),
            2,
            "both entries exist; the mounter decides"
        );
        assert_eq!(
            out.catalogue[0].version, "9.9.9",
            "the built-in must come FIRST"
        );
        assert_eq!(out.catalogue[1].version, "0.1.0");
    }

    #[test]
    fn discovery_failing_does_not_empty_the_built_in_catalogue() {
        // A missing or unreadable packages root must not cost the user the modules
        // that were compiled in. "No third-party packages" is the normal case.
        let out = super::with_discovered(
            vec![built_in("sin90")],
            std::path::Path::new("/nonexistent/agent24-packages"),
            &CancellationToken::new(),
        );
        assert_eq!(out.catalogue.len(), 1);
        assert_eq!(out.catalogue[0].name, "sin90");
    }

