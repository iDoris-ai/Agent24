//! ME4-S3 §3.2 — `Module`/`ModuleBuilder`: the connect/serve skeleton every
//! out-of-process module runs. `connect()` (production path) takes over the
//! kernel-bound listener FIRST (§2.3 step 0), then reads the environment,
//! parses the manifest for `name`/`route_namespace`/`kernel_capabilities`
//! (never hand-copied — §1.2 row 4), and dials. `with_env` (test-util only)
//! skips the listener takeover entirely, so a test can never consume the
//! one-per-process fd 3 by accident (§3.2).

use std::borrow::Cow;
use std::path::Path;
use std::sync::Arc;

use agent24_os_proto::initialize::Offer;
use agent24_os_proto::manifest::{self, ManifestFacts};
use agent24_os_proto::module::{
    ConnectError, Connection, EnvError, FatalHook, Hello, InheritedListener, ListenError, ModuleEnv,
};

use crate::clients::{ApprovalClient, EventsClient, MemoryClient, ModelClient, SchedulerClient};

/// Protocol range this SDK version implements (§8 Q7): exactly what it
/// speaks, not the widest range a future kernel might one day negotiate
/// down to. 2 (ME4-S2b): `CompleteRequest.temperature`; a module that sets it
/// asks for 2 in its manifest (`min_daemon_protocol: 2`).
pub const PROTOCOL_MIN: u32 = 1;
pub const PROTOCOL_MAX: u32 = 2;
// The SDK speaks no further than the kernel it is built with.
const _: () = assert!(PROTOCOL_MAX <= agent24_os_proto::version::KERNEL_PROTOCOL_MAX);

#[derive(Debug, thiserror::Error)]
pub enum SdkError {
    #[error("reading the module environment: {0:?}")]
    Env(EnvError),
    #[error("parsing the manifest: {0}")]
    Manifest(String),
    #[error("connecting to the kernel: {0}")]
    Connect(#[from] ConnectError),
    #[error("taking over the inherited listener: {0:?}")]
    Listen(ListenError),
    /// `serve()` was called on a `Module` built with `with_env` (test-util),
    /// which deliberately never takes over fd 3.
    #[error("this Module was built with `with_env`; it has no listener to serve on")]
    NoListener,
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
}

/// The default connection-lost behaviour (Sin90's `main.rs:138-145`): warn
/// and exit 70 so the supervisor starts the next generation. There is no
/// reconnect (one callback connection per generation, §2.3) — running on
/// past this point would mean answering HTTP requests over a connection
/// that will never deliver another kernel call.
///
/// `process::exit` skips Rust destructors. That is the Q10 contract: the
/// supervisor must see status 70, not `serve` returning `Ok` (module `main`
/// turns that into success). Advisory `flock`s are released by the kernel
/// when the process terminates, so a lock guard's `Drop` is not what
/// unlocks them.
fn default_fatal_hook() -> FatalHook {
    FatalHook::new(|| {
        // Best-effort log, then exit unconditionally. A fmt subscriber
        // reports a failed stderr write with `eprintln!`, and Rust's
        // `print_to` panics on that error (`stdio.rs`). This hook runs on
        // the callback reader's tokio task, which catches task panics — an
        // unwind here used to skip `exit` and leave the process holding
        // the storage lock (issue #1020).
        let _ = std::panic::catch_unwind(|| {
            tracing::warn!(
                "callback connection lost; exiting so the supervisor restarts this module"
            );
        });
        std::process::exit(70);
    })
}

pub struct ModuleBuilder {
    manifest_yaml: Cow<'static, str>,
    on_fatal: Option<FatalHook>,
    /// `Some` only via `with_env` (test-util): the production path always
    /// reads the real process environment and takes over fd 3 instead.
    env_override: Option<ModuleEnv>,
}

impl ModuleBuilder {
    /// Overrides the default `warn! + exit(70)` connection-lost behaviour.
    #[must_use]
    pub fn on_connection_lost(mut self, hook: FatalHook) -> Self {
        self.on_fatal = Some(hook);
        self
    }

    /// Tests only: connect with `env` instead of the process environment,
    /// and skip taking over the inherited listener entirely (`serve` then
    /// returns [`SdkError::NoListener`] — a test drives its router directly,
    /// e.g. with `tower::ServiceExt::oneshot`). `hook` is a required
    /// argument so a test can never fall back to the default `exit(70)`.
    #[cfg(feature = "test-util")]
    #[must_use]
    pub fn with_env(mut self, env: ModuleEnv, hook: FatalHook) -> Self {
        self.env_override = Some(env);
        self.on_fatal = Some(hook);
        self
    }

    /// Production path: take over the inherited listener (fd 3), read the
    /// environment, parse the manifest, dial and run `initialize`. Once per
    /// process — see `agent24_os_proto::module::take_listener`'s doc comment
    /// for why (`cargo test` runs a whole test binary in one process).
    ///
    /// # Errors
    /// See [`SdkError`].
    pub async fn connect(self) -> Result<Module, SdkError> {
        let listener = match &self.env_override {
            Some(_) => None,
            None => Some(agent24_os_proto::module::take_listener().map_err(SdkError::Listen)?),
        };
        let env = match self.env_override {
            Some(env) => env,
            None => ModuleEnv::from_env().map_err(SdkError::Env)?,
        };
        let facts = manifest::facts_from_yaml(&self.manifest_yaml).map_err(SdkError::Manifest)?;
        let capabilities: Vec<&str> = facts
            .kernel_capabilities
            .iter()
            .map(String::as_str)
            .collect();
        let hello = Hello {
            module: &facts.name,
            manifest_bytes: self.manifest_yaml.as_bytes(),
            capabilities: &capabilities,
            protocol_min: PROTOCOL_MIN,
            protocol_max: PROTOCOL_MAX,
        };
        let hook = self.on_fatal.unwrap_or_else(default_fatal_hook);
        let conn = Connection::connect_from_env(&env, &hello, hook).await?;
        Ok(Module {
            env,
            facts,
            conn: Arc::new(conn),
            listener,
        })
    }
}

/// A connected out-of-process module: one live callback connection, plus
/// (in production) the kernel-bound listener to serve HTTP on.
pub struct Module {
    env: ModuleEnv,
    facts: ManifestFacts,
    conn: Arc<Connection>,
    listener: Option<InheritedListener>,
}

impl Module {
    /// `manifest_yaml` must be the exact bytes the kernel digests (typically
    /// `include_str!("../domain-os.yml")`). `Cow` so a test can build a
    /// manifest at runtime instead of leaking a `String` to get a
    /// `&'static str`.
    #[must_use]
    pub fn builder(manifest_yaml: impl Into<Cow<'static, str>>) -> ModuleBuilder {
        ModuleBuilder {
            manifest_yaml: manifest_yaml.into(),
            on_fatal: None,
            env_override: None,
        }
    }

    #[must_use]
    pub fn data_dir(&self) -> &Path {
        self.env.data_dir()
    }

    #[must_use]
    pub fn offer(&self) -> &Offer {
        self.conn.offer()
    }

    #[must_use]
    pub fn is_alive(&self) -> bool {
        self.conn.is_alive()
    }

    #[must_use]
    pub fn events(&self) -> Option<EventsClient> {
        EventsClient::new(&self.conn)
    }

    #[must_use]
    pub fn memory(&self) -> Option<MemoryClient> {
        MemoryClient::new(&self.conn)
    }

    #[must_use]
    pub fn approval(&self) -> Option<ApprovalClient> {
        ApprovalClient::new(&self.conn)
    }

    #[must_use]
    pub fn scheduler(&self) -> Option<SchedulerClient> {
        SchedulerClient::new(&self.conn)
    }

    #[must_use]
    pub fn model(&self) -> Option<ModelClient> {
        ModelClient::new(&self.conn)
    }

    /// Nest `router` under the manifest's `route_namespace` and serve it on
    /// the listener taken in `connect()`. Returns when the server stops.
    ///
    /// # Errors
    /// [`SdkError::NoListener`] if this `Module` was built with `with_env`;
    /// otherwise an i/o error from the underlying `axum::serve`.
    pub async fn serve(self, router: axum::Router) -> Result<(), SdkError> {
        let listener = self.listener.ok_or(SdkError::NoListener)?;
        let app = axum::Router::new().nest(&self.facts.route_namespace, router);
        axum::serve(listener.into_tokio(), app).await?;
        Ok(())
    }
}

#[cfg(all(test, feature = "test-util"))]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use serde_json::json;

    use super::*;
    use crate::testing;

    /// A drop guard around a manually-created temp dir. `tempfile::TempDir`
    /// appends its own random suffix on top of our prefix, and these
    /// particular temp dirs hold a `cb.sock` AF_UNIX path — that extra
    /// suffix was enough to blow macOS's ~104-byte `sun_path` limit ("path
    /// must be shorter than SUN_LEN"). Building the exact, length-budgeted
    /// name ourselves and only borrowing `tempfile`'s idea (a guard that
    /// removes the dir on drop) keeps both properties.
    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// B1 (external review of #516, same root cause as #515): plain
    /// `SystemTime::now()` nanoseconds collided under parallel `cargo test`
    /// on macOS (clock resolution), producing `AddrInUse`/`EEXIST` for the
    /// socket path built on top of this dir. A process-local counter makes
    /// each call unique regardless of clock resolution; the returned guard
    /// additionally cleans the directory up on drop instead of leaking it
    /// into the temp dir on every test run.
    fn tempdir() -> TempDir {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "a24sdk-mod-{}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    const MANIFEST: &str =
        "name: minimal\nroute_namespace: /api/v1/minimal\nkernel_capabilities: [events]\n";

    // J-S11: the handshake carries the manifest-derived name/capabilities and
    // the declared protocol range is exactly what this SDK speaks, {1,2} since
    // ME4-S2b (§8 Q7) — never the 1..=1000 Sin90's pre-migration code used,
    // which would let a future v3 kernel "successfully" negotiate a version
    // this SDK does not speak.
    #[tokio::test]
    async fn connect_derives_hello_from_the_manifest_and_declares_what_it_speaks() {
        let dir = tempdir();
        let (env, endpoint) = testing::FakeEndpoint::bind(dir.path());
        let accept = endpoint.accept_initialize(json!({
            "protocol_version": 1,
            "offer": {"provides": ["_a24/events/"]},
        }));
        let (hook, _count) = testing::recording_hook();
        let connect = Module::builder(MANIFEST).with_env(env, hook).connect();
        let ((params, _peer), module_result) = tokio::join!(accept, connect);
        let module = module_result.expect("handshake must succeed");
        assert_eq!(params["module"], "minimal");
        assert_eq!(params["capabilities"], json!(["events"]));
        assert_eq!(params["protocol_versions"], json!({"min": 1, "max": 2}));
        assert!(module.events().is_some());
        assert!(module.memory().is_none(), "not granted; offer excludes it");
    }

    #[tokio::test]
    async fn with_env_never_takes_the_listener_so_serve_reports_no_listener() {
        let dir = tempdir();
        let (env, endpoint) = testing::FakeEndpoint::bind(dir.path());
        let accept = endpoint.accept_initialize(json!({
            "protocol_version": 1,
            "offer": {"provides": []},
        }));
        let (hook, _count) = testing::recording_hook();
        let connect = Module::builder(MANIFEST).with_env(env, hook).connect();
        let (_accepted, module_result) = tokio::join!(accept, connect);
        let module = module_result.unwrap();
        let err = module.serve(axum::Router::new()).await.unwrap_err();
        assert!(matches!(err, SdkError::NoListener));
    }

    /// Issue #1020: the default hook's `tracing::warn!` goes through a fmt
    /// subscriber. On a broken stderr that subscriber's `eprintln!` fallback
    /// panics (`print_to`), and the reader's tokio task would swallow the
    /// panic before `process::exit(70)`. The child installs the same
    /// subscriber shape documents uses, waits until stderr writes fail, and
    /// fires the real hook from a tokio task.
    ///
    /// `--nocapture` is required. libtest's capture buffer absorbs
    /// `eprintln!`, so a captured child would exit 70 without ever hitting
    /// the panic this test exists to survive.
    #[test]
    fn default_fatal_hook_exits_70_when_stderr_write_fails() {
        if std::env::var_os("A24_DEATH_HOOK_CHILD").is_some() {
            death_hook_child();
        }
        death_hook_parent();
    }

    fn death_hook_child() -> ! {
        wait_until_stderr_write_fails();
        std::panic::set_hook(Box::new(|info| {
            let payload = info.payload();
            let msg = payload
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
                .unwrap_or("non-string panic");
            // `writeln!` to the handle returns the error. `eprintln!` would
            // panic again and abort before the parent can see this line.
            let mut out = std::io::stdout().lock();
            let _ = std::io::Write::write_fmt(&mut out, format_args!("PANIC {msg}\n"));
            let _ = std::io::Write::flush(&mut out);
        }));
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .with_writer(std::io::stderr)
            .init();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        rt.block_on(async {
            tokio::spawn(async {
                default_fatal_hook().fire();
            });
            // If the hook's panic escapes, tokio keeps the runtime alive
            // until this sleep ends. 101 means exit was skipped.
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        });
        std::process::exit(101);
    }

    fn wait_until_stderr_write_fails() {
        let start = std::time::Instant::now();
        loop {
            // `Write::write_all` returns `Err` on `EPIPE`. The print macros
            // panic instead, which is the bug; this probe must not.
            match std::io::Write::write_all(&mut std::io::stderr(), b"x") {
                Ok(()) => {
                    if start.elapsed() > std::time::Duration::from_secs(5) {
                        std::process::exit(102);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(_) => return,
            }
        }
    }

    fn death_hook_parent() {
        let exe = std::env::current_exe().unwrap();
        let mut child = std::process::Command::new(exe)
            .args([
                "--exact",
                "module::tests::default_fatal_hook_exits_70_when_stderr_write_fails",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("A24_DEATH_HOOK_CHILD", "1")
            .env("RUST_TEST_NOCAPTURE", "1")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        // Close the read end before the child logs. Its next stderr write
        // is `EPIPE` instead of a buffered success.
        drop(child.stderr.take());
        let mut stdout = child.stdout.take().unwrap();

        let start = std::time::Instant::now();
        let status = loop {
            match child.try_wait().unwrap() {
                Some(status) => break status,
                None if start.elapsed() > std::time::Duration::from_secs(8) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let mut out = String::new();
                    let _ = std::io::Read::read_to_string(&mut stdout, &mut out);
                    panic!("death hook still running after 8s; stdout={out:?}");
                }
                None => std::thread::sleep(std::time::Duration::from_millis(20)),
            }
        };
        let mut out = String::new();
        std::io::Read::read_to_string(&mut stdout, &mut out).unwrap();
        assert!(
            out.contains("failed printing to stderr"),
            "stderr write did not panic inside the hook; stdout={out:?} status={status:?}"
        );
        assert_eq!(
            status.code(),
            Some(70),
            "hook must exit 70 after the log panic; stdout={out:?} status={status:?}"
        );
    }
}
