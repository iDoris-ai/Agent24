    // ---- FU-92 follow-up: secure_fallback_dir / callback_root hardening ----
    // The fallback directory `callback_root` may pick lives under the shared,
    // world-writable `/tmp` with a PREDICTABLE name (a hash of `root`) — so
    // another local user could race to plant something at that name before
    // this daemon starts. These tests are for `secure_fallback_dir` itself
    // (the pure filesystem check) and for its wiring into `callback_root`
    // (so a mutation that drops the call is caught, not just the helper in
    // isolation). The "normal case" — nothing planted, a real mount — is
    // covered end to end by `daemon_modules.rs`'s long-HOME blackbox test.

    /// Mirrors `callback_root`'s own hash formula, only so tests can find (and
    /// clean up before/after) the exact path it will pick for a given `root`
    /// — never asserted on for its OWN sake.
    fn hashed_fallback_path(root: &std::path::Path) -> std::path::PathBuf {
        std::path::PathBuf::from("/tmp").join(format!("a24-run-{:016x}", fallback_dir_hash(root)))
    }

    #[tokio::test]
    async fn callback_root_accounts_for_symlink_expansion_before_choosing_a_socket_path() {
        let base = tempfile::tempdir_in("/tmp").unwrap();
        let real = base.path().join("x".repeat(100));
        std::fs::create_dir(&real).unwrap();
        let alias = base.path().join("short");
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        let fallback = hashed_fallback_path(&alias);

        // Exercise the real directory creation and bind, not only the length check.
        let host = process_host(&alias, Duration::from_secs(1), CancellationToken::new()).unwrap();
        let listener = host.callback_dir.listen_next().unwrap();
        assert!(listener.path().as_os_str().len() <= agent24_os_proto::endpoint::MAX_SOCKET_PATH);
        drop(listener);
        drop(host);
        std::fs::remove_dir_all(fallback).unwrap();
    }

    /// Codex ME4-CODEX-DEBT-10 (#528 High): golden value locking
    /// [`fallback_dir_hash`] to sha256, not `std::collections::hash_map::DefaultHasher`
    /// (whose own doc does not promise this algorithm across Rust versions).
    /// A fixed input's fallback directory name must never silently change
    /// again — if this test ever needs updating, that is a deliberate,
    /// visible break, not a toolchain upgrade nobody noticed.
    #[test]
    fn fallback_dir_hash_is_a_stable_golden_value() {
        let root = std::path::Path::new("/home/test/very/long/path/for/golden/hash/test");
        assert_eq!(
            format!("{:016x}", fallback_dir_hash(root)),
            "2ffb6c3d38a488a3",
            "fallback_dir_hash's output for a fixed input must never change — if it did, \
             every existing user's fallback directory name would silently change with it"
        );
    }

    /// Nothing there yet: created fresh, exactly `0700`.
    #[test]
    fn secure_fallback_dir_creates_a_fresh_directory_0700() {
        use std::os::unix::fs::PermissionsExt;
        let base = tempfile::tempdir().unwrap();
        let path = base.path().join("fallback");
        secure_fallback_dir(&path).expect("a fresh directory is fine");
        let mode = std::fs::symlink_metadata(&path)
            .unwrap()
            .permissions()
            .mode()
            & 0o7777;
        assert_eq!(mode, 0o700, "{mode:o}");
    }

    /// A restart re-checking its OWN directory from a previous run (still
    /// `0700`, still ours) must not be refused — only an unsafe entry is.
    #[test]
    fn secure_fallback_dir_accepts_its_own_directory_again() {
        let base = tempfile::tempdir().unwrap();
        let path = base.path().join("fallback");
        secure_fallback_dir(&path).unwrap();
        secure_fallback_dir(&path).expect("our own 0700 directory is fine the second time");
    }

    /// A pre-existing directory at that name with looser permissions — as
    /// another local user racing the predictable name could leave — is
    /// refused, not silently tightened in place (unlike `agent24-os-proto`'s
    /// `private_dir`, which DOES tighten a loose directory of ours: on a
    /// shared `/tmp`, "ours" cannot be trusted here without the check first).
    #[test]
    fn secure_fallback_dir_refuses_a_preexisting_loose_directory() {
        use std::os::unix::fs::PermissionsExt;
        let base = tempfile::tempdir().unwrap();
        let path = base.path().join("fallback");
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let err = secure_fallback_dir(&path).expect_err("a 0755 directory must be refused");
        assert!(err.contains("0700"), "{err}");
    }

    /// A pre-existing symlink at that name — pointing anywhere, even nowhere
    /// that exists — is refused, never followed (which is exactly what
    /// `DirBuilder::create(..).recursive(true)` — used one layer down by
    /// `CallbackDir::create` — would otherwise walk straight through).
    #[test]
    fn secure_fallback_dir_refuses_a_preexisting_symlink() {
        let base = tempfile::tempdir().unwrap();
        let path = base.path().join("fallback");
        let elsewhere = base.path().join("elsewhere");
        std::os::unix::fs::symlink(&elsewhere, &path).unwrap();
        let err = secure_fallback_dir(&path).expect_err("a symlink must be refused");
        assert!(err.contains("symlink"), "{err}");
        assert!(
            !elsewhere.exists(),
            "the symlink target must never be created"
        );
    }

    /// Wiring check: `callback_root` itself must refuse an unsafe pre-existing
    /// fallback directory rather than silently reusing it or trying some
    /// other path (mutation check: dropping the `secure_fallback_dir` call
    /// from `callback_root` turns this green when it must be red).
    #[test]
    fn callback_root_refuses_an_unsafe_preexisting_fallback_directory() {
        // A root that does not exist on disk — `callback_root` never touches
        // `root` itself in the too-long branch, only hashes it — but long
        // enough that `<root>/run/<pid>/<u64::MAX>.sock` cannot fit, so the
        // `/tmp` fallback branch is the one under test. Unique to this test
        // so it cannot collide with the other `callback_root`/blackbox tests.
        let long_root = std::path::PathBuf::from(format!(
            "/callback-root-hardening-test-{}-{}",
            std::process::id(),
            "x".repeat(150)
        ));
        let fallback = hashed_fallback_path(&long_root);
        let _ = std::fs::remove_dir_all(&fallback);
        let _ = std::fs::remove_file(&fallback);

        let got = callback_root(&long_root).expect("a fresh fallback directory is fine");
        assert_eq!(got, fallback);

        // Replace the now-created directory with a symlink, as a racing local
        // user could have done between two runs of this daemon.
        std::fs::remove_dir_all(&fallback).unwrap();
        std::os::unix::fs::symlink("/nonexistent-elsewhere-for-this-test", &fallback).unwrap();
        let err = callback_root(&long_root).expect_err("a symlinked fallback must be refused");
        assert!(err.contains("symlink"), "{err}");

        let _ = std::fs::remove_file(&fallback);
    }

    // ── Review (Codex A3 follow-up, §5.5): the `attach_registry_cell` race ──
    //
    // `serve`'s `stopping` task checks `attach_registry_cell` (a
    // `OnceLock<Arc<AttachRegistry>>`) exactly ONCE, the instant shutdown is
    // requested, and only calls `revoke_all()` if it finds the cell already
    // filled. `serve` itself fills the cell and re-checks `cancel` right
    // after via [`fill_attach_registry_and_recheck`] — both tests below call
    // THAT function directly (not a re-implementation of it) against a REAL
    // `AttachRegistry` (via `state()`), driving it through both possible
    // interleavings with `stopping`'s own one-shot check. Driving the entire
    // `serve()` startup through this precise interleaving would need to win
    // an actual OS-scheduling race, which cannot be made deterministic for
    // CI; calling the extracted function directly is what makes a reverse
    // mutation of it (deleting the re-check) turn these tests red — see
    // each test's own doc comment.

    /// The bug: shutdown requested before the registry is ever filled into
    /// the cell. Without the fix, `registry.is_closed()` would stay `false`
    /// forever — the attach listener would keep accepting and handshaking
    /// connections for a daemon that is meant to be going away. Confirmed
    /// red under the reverse mutation (deleting the `if cancel.is_cancelled()
    /// { registry.revoke_all(); }` re-check inside
    /// `fill_attach_registry_and_recheck`): `registry.is_closed()` stayed
    /// `false` at the final assertion.
    #[tokio::test]
    async fn a_shutdown_that_races_ahead_of_the_registry_fill_is_still_revoked() {
        let state = state().await;
        let registry = Arc::clone(&state.attach_registry);
        let cancel = CancellationToken::new();
        let cell: std::sync::OnceLock<Arc<agent24_attach::attach_registry::AttachRegistry>> =
            std::sync::OnceLock::new();

        // `stopping`'s own one-shot check, landing before the fill: shutdown
        // is already requested, but the cell is still empty.
        cancel.cancel();
        if let Some(r) = cell.get() {
            r.revoke_all();
        }
        assert!(
            !registry.is_closed(),
            "sanity check on the test itself: the premature check must find nothing to revoke"
        );

        // The startup fill, immediately followed by the FIXED re-check —
        // the actual production function, not a copy of its body.
        fill_attach_registry_and_recheck(&cell, &registry, &cancel);

        assert!(
            registry.is_closed(),
            "a shutdown that raced ahead of the registry fill must still end up revoked"
        );
    }

    /// The ordinary ordering, as a control: the fill happens first, well
    /// before any shutdown, and `stopping`'s own check (not the re-check
    /// inside `fill_attach_registry_and_recheck`) is what revokes it. That
    /// re-check is then a harmless no-op — `revoke_all` is idempotent, and
    /// `cancel` is not yet cancelled at the point the re-check runs.
    #[tokio::test]
    async fn the_ordinary_ordering_the_stopping_tasks_own_check_still_revokes() {
        let state = state().await;
        let registry = Arc::clone(&state.attach_registry);
        let cancel = CancellationToken::new();
        let cell: std::sync::OnceLock<Arc<agent24_attach::attach_registry::AttachRegistry>> =
            std::sync::OnceLock::new();

        assert!(!cancel.is_cancelled());
        fill_attach_registry_and_recheck(&cell, &registry, &cancel);
        assert!(
            !registry.is_closed(),
            "no shutdown requested yet — the re-check must not revoke early"
        );

        cancel.cancel();
        if let Some(r) = cell.get() {
            r.revoke_all();
        }
        assert!(registry.is_closed());
    }
