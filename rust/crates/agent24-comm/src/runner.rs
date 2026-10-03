//! `HyphaeRunner`: spawns a single, hash-verified Hyphae CLI invocation,
//! writes an optional password to its stdin, and parses the resulting
//! envelope (COMM-HYPHAE.md §3).
//!
//! The child's environment is `env_clear()`-ed and rebuilt from
//! [`ENV_ALLOW`] only; `HOME` and `HYPHAE_OUTPUT=json` are always set by the
//! runner itself (never inherited), and `cwd` is pinned to the same home
//! directory. The password, when present, goes only to a dedicated stdin
//! pipe that is closed immediately after the write — never into argv, never
//! into a log line.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use rustix::process::{Pid, Signal, kill_process_group};
use serde_json::Value;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::binary::VerifiedBinary;
use crate::keystore_lock::KeystoreWriteLock;
use crate::password::Password;

/// Cap on stdout/stderr size, applied independently to each stream.
pub const OUTPUT_CAP: usize = 4 * 1024 * 1024;

/// Variables let through `env_clear()`. Everything else — including the
/// parent agent24d process's own environment — is dropped. `HOME` is
/// deliberately absent: the runner always forces it to the Hyphae home
/// directory rather than ever passing one through.
pub const ENV_ALLOW: &[&str] = &[
    "PATH",
    "LANG",
    "LC_ALL",
    "TZ",
    "TMPDIR",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "HTTPS_PROXY",
    "https_proxy",
    "HTTP_PROXY",
    "http_proxy",
    "ALL_PROXY",
    "all_proxy",
    "NO_PROXY",
    "no_proxy",
];

/// One invocation of the Hyphae CLI. `args` must not include `--json` (the
/// runner sets `HYPHAE_OUTPUT=json` instead) or `--password-stdin` (the
/// runner appends it iff `password` is `Some`).
pub struct Invocation {
    pub args: Vec<OsString>,
    pub password: Option<Password>,
    pub timeout: Option<Duration>,
}

/// Hyphae's exit-code contract: 0 is success, 1-5 are the closed set of
/// failure classes it distinguishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitClass {
    Ok = 0,
    UserError = 1,
    NetworkError = 2,
    AuthError = 3,
    OtherError = 4,
    WriteConflict = 5,
}

impl ExitClass {
    fn from_code(code: i32) -> Option<Self> {
        match code {
            0 => Some(Self::Ok),
            1 => Some(Self::UserError),
            2 => Some(Self::NetworkError),
            3 => Some(Self::AuthError),
            4 => Some(Self::OtherError),
            5 => Some(Self::WriteConflict),
            _ => None,
        }
    }
}

/// The parsed `{ok:true,data}` / `{ok:false,error,message,data?}` envelope.
/// `Failed.data` is kept even on failure: a partially-successful `send` or
/// `outbox retry` still carries an `event_id` callers need (COMM-HYPHAE.md
/// §5.3).
#[derive(Debug, Clone)]
pub enum Envelope {
    Ok {
        data: Value,
    },
    Failed {
        exit: ExitClass,
        error: String,
        message: String,
        data: Option<Value>,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    #[error("password length {0} is out of the allowed 1..=4096 range")]
    PasswordLength(usize),
    #[error("failed to spawn hyphae: {0}")]
    Spawn(#[source] io::Error),
    #[error("hyphae timed out after {0:?}")]
    Timeout(Duration),
    #[error("hyphae was terminated by a signal")]
    Signaled,
    #[error("hyphae exited with unexpected code {0}")]
    UnknownExit(i32),
    #[error("bad envelope at exit {exit}: {reason}")]
    BadEnvelope { exit: i32, reason: &'static str },
    #[error("hyphae output exceeded the size cap")]
    OutputTooLarge,
    #[error("io error: {0}")]
    Io(#[source] io::Error),
}

/// Pure function: parses Hyphae's envelope out of `(exit code, stdout,
/// stderr)`. Exit 0 must carry `{ok:true,...}` on stdout; exit 1..=5 must
/// carry `{ok:false,error,message,...}` on stderr. An `ok` field that
/// contradicts the exit code, a stream that isn't exactly one JSON object,
/// or an exit code outside 0..=5 are all reported as errors rather than
/// guessed at.
pub fn parse_envelope(code: i32, stdout: &[u8], stderr: &[u8]) -> Result<Envelope, RunnerError> {
    let class = ExitClass::from_code(code).ok_or(RunnerError::UnknownExit(code))?;

    if matches!(class, ExitClass::Ok) {
        let object = parse_json_object(stdout, code, "stdout is not a single json object")?;
        let ok = object
            .get("ok")
            .and_then(Value::as_bool)
            .ok_or(RunnerError::BadEnvelope {
                exit: code,
                reason: "stdout is missing a boolean ok field",
            })?;
        if !ok {
            return Err(RunnerError::BadEnvelope {
                exit: code,
                reason: "ok:false contradicts a zero exit code",
            });
        }
        let data = object.get("data").cloned().unwrap_or(Value::Null);
        Ok(Envelope::Ok { data })
    } else {
        let object = parse_json_object(stderr, code, "stderr is not a single json object")?;
        let ok = object
            .get("ok")
            .and_then(Value::as_bool)
            .ok_or(RunnerError::BadEnvelope {
                exit: code,
                reason: "stderr is missing a boolean ok field",
            })?;
        if ok {
            return Err(RunnerError::BadEnvelope {
                exit: code,
                reason: "ok:true contradicts a non-zero exit code",
            });
        }
        let error = object
            .get("error")
            .and_then(Value::as_str)
            .ok_or(RunnerError::BadEnvelope {
                exit: code,
                reason: "stderr is missing a string error field",
            })?
            .to_string();
        let message = object
            .get("message")
            .and_then(Value::as_str)
            .ok_or(RunnerError::BadEnvelope {
                exit: code,
                reason: "stderr is missing a string message field",
            })?
            .to_string();
        let data = object.get("data").cloned();
        Ok(Envelope::Failed {
            exit: class,
            error,
            message,
            data,
        })
    }
}

fn parse_json_object(
    bytes: &[u8],
    code: i32,
    reason: &'static str,
) -> Result<serde_json::Map<String, Value>, RunnerError> {
    // `serde_json::from_slice` fails on trailing non-whitespace bytes, so two
    // concatenated JSON values are rejected the same way invalid JSON is.
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|_| RunnerError::BadEnvelope { exit: code, reason })?;
    match value {
        Value::Object(map) => Ok(map),
        _ => Err(RunnerError::BadEnvelope { exit: code, reason }),
    }
}

/// Filters a parent environment down to [`ENV_ALLOW`]. A free function
/// (rather than reading `std::env::vars()` directly) so it can be unit
/// tested against a synthetic environment without ever mutating the real
/// process environment — `std::env::set_var` is `unsafe` and this crate
/// forbids unsafe code everywhere, including in tests.
pub(crate) fn filtered_env<I>(source: I) -> Vec<(String, String)>
where
    I: IntoIterator<Item = (String, String)>,
{
    source
        .into_iter()
        .filter(|(key, _)| ENV_ALLOW.contains(&key.as_str()))
        .collect()
}

fn kill_process_group_best_effort(pid: u32) {
    let Ok(raw) = i32::try_from(pid) else {
        return;
    };
    if let Some(pid) = Pid::from_raw(raw) {
        let _ = kill_process_group(pid, Signal::Kill);
    }
}

#[cfg(test)]
pub(crate) mod timeout_test_hook {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

    use tokio::sync::Notify;

    struct Barrier {
        reached: Arc<Notify>,
        resume: Arc<Notify>,
    }

    fn barriers() -> &'static Mutex<HashMap<PathBuf, Barrier>> {
        static BARRIERS: OnceLock<Mutex<HashMap<PathBuf, Barrier>>> = OnceLock::new();
        BARRIERS.get_or_init(|| Mutex::new(HashMap::new()))
    }

    fn locked_barriers() -> MutexGuard<'static, HashMap<PathBuf, Barrier>> {
        match barriers().lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    pub(crate) fn install(binary: &Path) -> (Arc<Notify>, Arc<Notify>) {
        let reached = Arc::new(Notify::new());
        let resume = Arc::new(Notify::new());
        let previous = locked_barriers().insert(
            binary.to_path_buf(),
            Barrier {
                reached: Arc::clone(&reached),
                resume: Arc::clone(&resume),
            },
        );
        assert!(previous.is_none(), "duplicate runner timeout barrier");
        (reached, resume)
    }

    pub(super) async fn pause_before_timeout(binary: &Path) {
        let barrier = locked_barriers().remove(binary);
        let Some(barrier) = barrier else {
            return;
        };
        barrier.reached.notify_one();
        barrier.resume.notified().await;
    }
}

pub struct HyphaeRunner {
    bin: VerifiedBinary,
    home: PathBuf,
    default_timeout: Duration,
    keystore_lock: KeystoreWriteLock,
}

impl HyphaeRunner {
    pub fn new(bin: VerifiedBinary, home: PathBuf, default_timeout: Duration) -> Self {
        Self {
            bin,
            home,
            default_timeout,
            keystore_lock: KeystoreWriteLock::default(),
        }
    }

    /// The lock every keystore-writing Hyphae invocation must hold for its
    /// full lifetime (COMM-HYPHAE.md §3, H3/G8; see `keystore_lock.rs`).
    /// Exposed so a multi-step flow like `import` (COMM-HYPHAE.md §4.1) can
    /// hold it across several invocations, not just one.
    pub fn keystore_lock(&self) -> &KeystoreWriteLock {
        &self.keystore_lock
    }

    /// A new runner over the *same verified binary* (no re-verification,
    /// no re-copy) but a different `home` — and therefore a different
    /// `HOME`/cwd for every command it runs. COMM-2b's `import` flow
    /// (`crate::import`) uses this to run `identity list` / `contact list` /
    /// `storage outbox list` / `history inbox` against the staging HOME
    /// before committing, and `identity create --nickname __verify` against
    /// a one-off verify HOME (COMM-HYPHAE.md §4.1 steps 4–5) — all without
    /// ever pointing the *real* runner (the one callers hold a shared
    /// `Arc<HyphaeRunner>` to) at anything but the real home. The returned
    /// runner gets its own, independent `KeystoreWriteLock`: it is only ever
    /// used against a throwaway staging/verify HOME that nothing else can
    /// reach, so there is no writer to serialize against.
    pub(crate) fn with_home(&self, home: PathBuf) -> Self {
        Self {
            bin: self.bin.clone(),
            home,
            default_timeout: self.default_timeout,
            keystore_lock: KeystoreWriteLock::default(),
        }
    }

    /// The verified Hyphae binary's path. COMM-4a's daemon supervisor
    /// spawns a long-running `hyphae daemon` child itself (not via
    /// [`Self::run`], which is built around a single invocation whose
    /// stdout/stderr are captured in full for envelope parsing) but must
    /// exec the exact same verified copy.
    pub fn bin_path(&self) -> &Path {
        self.bin.path()
    }

    /// The verified binary's sha256, hex-encoded — recorded in the daemon
    /// pid file (COMM-HYPHAE.md §2) so a future `agent24d` can at least log
    /// which binary an orphaned daemon was running.
    pub fn bin_sha256_hex(&self) -> String {
        self.bin.sha256().to_hex()
    }

    /// Builds (but does not spawn) the child command for `inv`: absolute
    /// binary path, `env_clear()` + [`ENV_ALLOW`] + forced `HOME` and
    /// `HYPHAE_OUTPUT=json`, cwd pinned to `home`, stdin `null` unless a
    /// password is supplied, its own process group, and `kill_on_drop`.
    pub fn command(&self, inv: &Invocation) -> Command {
        self.build_command(inv, std::env::vars())
    }

    fn build_command(
        &self,
        inv: &Invocation,
        parent_env: impl IntoIterator<Item = (String, String)>,
    ) -> Command {
        let mut cmd = Command::new(self.bin.path());
        cmd.args(&inv.args);
        if inv.password.is_some() {
            cmd.arg("--password-stdin");
        }
        cmd.env_clear();
        for (key, value) in filtered_env(parent_env) {
            cmd.env(key, value);
        }
        cmd.env("HOME", &self.home);
        cmd.env("HYPHAE_OUTPUT", "json");
        cmd.current_dir(&self.home);
        cmd.stdin(if inv.password.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.kill_on_drop(true);
        cmd.process_group(0);
        cmd
    }

    /// Test-only seam: lets tests supply a synthetic parent environment
    /// (e.g. a sentinel variable that must not leak) instead of the real
    /// process environment, without touching `std::env::set_var`.
    #[cfg(test)]
    pub(crate) fn command_with_parent_env(
        &self,
        inv: &Invocation,
        parent_env: Vec<(String, String)>,
    ) -> Command {
        self.build_command(inv, parent_env)
    }

    /// Spawns `inv`, writes and closes the password pipe if present, waits
    /// up to `inv.timeout` (or the runner's default), and parses the
    /// envelope. On timeout the process group is killed and the result is
    /// reported as [`RunnerError::Timeout`] — the underlying command may
    /// already have taken effect (COMM-HYPHAE.md §3), so callers must not
    /// treat a timeout as "nothing happened".
    pub async fn run(&self, inv: Invocation) -> Result<Envelope, RunnerError> {
        let timeout = inv.timeout.unwrap_or(self.default_timeout);
        let subcommand = inv
            .args
            .first()
            .map(|arg| arg.to_string_lossy().into_owned())
            .unwrap_or_default();

        let mut cmd = self.command(&inv);
        let mut child = cmd.spawn().map_err(RunnerError::Spawn)?;
        let pid = child.id();
        #[cfg(test)]
        timeout_test_hook::pause_before_timeout(self.bin.path()).await;
        let password = inv.password;

        let wait = async move {
            if let Some(password) = password {
                let mut stdin = child.stdin.take().ok_or_else(|| {
                    RunnerError::Spawn(io::Error::other("hyphae stdin unavailable"))
                })?;
                stdin
                    .write_all(password.as_bytes())
                    .await
                    .map_err(RunnerError::Spawn)?;
                stdin.shutdown().await.map_err(RunnerError::Spawn)?;
                drop(stdin);
            }
            child.wait_with_output().await.map_err(RunnerError::Spawn)
        };

        let output = match tokio::time::timeout(timeout, wait).await {
            Ok(result) => result?,
            Err(_elapsed) => {
                if let Some(pid) = pid {
                    kill_process_group_best_effort(pid);
                }
                return Err(RunnerError::Timeout(timeout));
            }
        };

        if output.stdout.len() > OUTPUT_CAP || output.stderr.len() > OUTPUT_CAP {
            return Err(RunnerError::OutputTooLarge);
        }

        let Some(code) = output.status.code() else {
            return Err(RunnerError::Signaled);
        };

        tracing::debug!(subcommand = %subcommand, exit = code, "hyphae run finished");

        parse_envelope(code, &output.stdout, &output.stderr)
    }

    /// Like [`Self::run`], but holds [`Self::keystore_lock`] for the entire
    /// invocation — spawn through exit — so no second keystore-writing
    /// invocation can start (and race `SaveKeyStore`'s unsynchronized
    /// read-modify-write) until this one has fully finished and its process
    /// has exited. Callers must use this, never `run`, for any Hyphae
    /// subcommand that writes `keystore.json`: `identity create`,
    /// `identity use`, `contact add`, and every step of `import`
    /// (COMM-HYPHAE.md §3).
    pub async fn run_keystore_write(&self, inv: Invocation) -> Result<Envelope, RunnerError> {
        let _guard = self.keystore_lock.acquire().await;
        self.run(inv).await
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    use super::*;
    use crate::binary::sha256_of;

    // ---- parse_envelope -----------------------------------------------

    #[test]
    fn ok_envelope_extracts_data() {
        let stdout = br#"{"ok":true,"data":{"foo":1}}"#;
        let envelope = parse_envelope(0, stdout, b"").unwrap();
        match envelope {
            Envelope::Ok { data } => assert_eq!(data.get("foo").and_then(Value::as_i64), Some(1)),
            Envelope::Failed { .. } => panic!("expected Ok envelope"),
        }
    }

    #[test]
    fn ok_envelope_defaults_missing_data_to_null() {
        let stdout = br#"{"ok":true}"#;
        let envelope = parse_envelope(0, stdout, b"").unwrap();
        match envelope {
            Envelope::Ok { data } => assert!(data.is_null()),
            Envelope::Failed { .. } => panic!("expected Ok envelope"),
        }
    }

    #[test]
    fn failed_envelope_covers_every_exit_class() {
        let cases = [
            (1, ExitClass::UserError),
            (2, ExitClass::NetworkError),
            (3, ExitClass::AuthError),
            (4, ExitClass::OtherError),
            (5, ExitClass::WriteConflict),
        ];
        for (code, class) in cases {
            let stderr = br#"{"ok":false,"error":"e","message":"m"}"#;
            let envelope = parse_envelope(code, b"", stderr).unwrap();
            match envelope {
                Envelope::Failed {
                    exit,
                    error,
                    message,
                    data,
                } => {
                    assert_eq!(exit, class, "exit class for code {code}");
                    assert_eq!(error, "e");
                    assert_eq!(message, "m");
                    assert!(data.is_none());
                }
                Envelope::Ok { .. } => panic!("expected Failed envelope for code {code}"),
            }
        }
    }

    #[test]
    fn unknown_exit_code_is_rejected() {
        let err = parse_envelope(7, b"", b"").unwrap_err();
        assert!(matches!(err, RunnerError::UnknownExit(7)));
    }

    #[test]
    fn ok_field_false_on_zero_exit_is_bad_envelope() {
        let stdout = br#"{"ok":false,"data":{}}"#;
        let err = parse_envelope(0, stdout, b"").unwrap_err();
        assert!(matches!(err, RunnerError::BadEnvelope { exit: 0, .. }));
    }

    #[test]
    fn ok_field_true_on_nonzero_exit_is_bad_envelope() {
        let stderr = br#"{"ok":true,"error":"e","message":"m"}"#;
        let err = parse_envelope(3, b"", stderr).unwrap_err();
        assert!(matches!(err, RunnerError::BadEnvelope { exit: 3, .. }));
    }

    #[test]
    fn non_json_output_is_bad_envelope() {
        let err = parse_envelope(0, b"not json at all", b"").unwrap_err();
        assert!(matches!(err, RunnerError::BadEnvelope { exit: 0, .. }));
    }

    #[test]
    fn two_concatenated_json_objects_is_bad_envelope() {
        let stdout = br#"{"ok":true,"data":{}}{"ok":true,"data":{}}"#;
        let err = parse_envelope(0, stdout, b"").unwrap_err();
        assert!(matches!(err, RunnerError::BadEnvelope { exit: 0, .. }));
    }

    #[test]
    fn partial_success_keeps_event_id_in_data() {
        let stderr =
            br#"{"ok":false,"error":"network_error","message":"m","data":{"event_id":"abc123","published_to":0}}"#;
        let envelope = parse_envelope(2, b"", stderr).unwrap();
        match envelope {
            Envelope::Failed {
                data: Some(data), ..
            } => {
                assert_eq!(data.get("event_id").and_then(Value::as_str), Some("abc123"));
            }
            other => panic!("expected Failed with data, got {other:?}"),
        }
    }

    // ---- filtered_env ----------------------------------------------------

    #[test]
    fn filtered_env_drops_unlisted_variables() {
        let source = vec![
            ("A24_SENTINEL".to_string(), "leak".to_string()),
            ("PATH".to_string(), "/usr/bin".to_string()),
            (
                "HTTPS_PROXY".to_string(),
                "http://proxy.example".to_string(),
            ),
        ];
        let filtered = filtered_env(source);
        assert!(filtered.iter().any(|(k, v)| k == "PATH" && v == "/usr/bin"));
        assert!(filtered.iter().any(|(k, _)| k == "HTTPS_PROXY"));
        assert!(!filtered.iter().any(|(k, _)| k == "A24_SENTINEL"));
    }

    // ---- end-to-end against fake "hyphae" shell scripts -------------------

    async fn install_fixture(
        source_dir: &Path,
        script_name: &str,
        script: &str,
        install_dir: &Path,
    ) -> VerifiedBinary {
        let source = source_dir.join(script_name);
        tokio::fs::write(&source, script).await.unwrap();
        let bytes = tokio::fs::read(&source).await.unwrap();
        let expected = sha256_of(&bytes);
        VerifiedBinary::install(&source, expected, install_dir)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn environment_is_scrubbed_and_home_cwd_are_forced() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("hyphae-home");
        tokio::fs::create_dir_all(&home).await.unwrap();
        let script = "#!/bin/sh\n\
            echo \"{\\\"ok\\\":true,\\\"data\\\":{\\\"home\\\":\\\"$HOME\\\",\\\"cwd\\\":\\\"$(pwd)\\\",\\\"sentinel\\\":\\\"${A24_SENTINEL:-}\\\",\\\"path_present\\\":\\\"${PATH:+yes}\\\"}}\"\n";
        let bin = install_fixture(tmp.path(), "echo-env.sh", script, &tmp.path().join("bin")).await;
        let runner = HyphaeRunner::new(bin, home.clone(), Duration::from_secs(5));

        let inv = Invocation {
            args: vec![],
            password: None,
            timeout: None,
        };
        let parent_env = vec![
            ("A24_SENTINEL".to_string(), "leak-if-present".to_string()),
            (
                "PATH".to_string(),
                std::env::var("PATH").unwrap_or_default(),
            ),
        ];
        let mut cmd = runner.command_with_parent_env(&inv, parent_env);
        let output = cmd.output().await.unwrap();
        let code = output.status.code().unwrap();
        let envelope = parse_envelope(code, &output.stdout, &output.stderr).unwrap();
        let Envelope::Ok { data } = envelope else {
            panic!("expected Ok envelope, stderr={:?}", output.stderr)
        };

        let expected_home = home.canonicalize().unwrap_or(home.clone());
        let seen_home = PathBuf::from(data.get("home").and_then(Value::as_str).unwrap());
        let seen_home = seen_home.canonicalize().unwrap_or(seen_home);
        assert_eq!(seen_home, expected_home);
        assert_eq!(data.get("sentinel").and_then(Value::as_str), Some(""));
        assert_eq!(
            data.get("path_present").and_then(Value::as_str),
            Some("yes")
        );
    }

    #[tokio::test]
    async fn password_is_written_byte_exact_and_never_in_argv() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("hyphae-home");
        tokio::fs::create_dir_all(&home).await.unwrap();
        let script = "#!/bin/sh\n\
            n=$(wc -c < /dev/stdin | tr -d ' ')\n\
            echo \"{\\\"ok\\\":true,\\\"data\\\":{\\\"stdin_len\\\":$n,\\\"argv\\\":\\\"$*\\\"}}\"\n";
        let bin =
            install_fixture(tmp.path(), "echo-stdin.sh", script, &tmp.path().join("bin")).await;
        let runner = HyphaeRunner::new(bin, home, Duration::from_secs(5));

        let secret = "s3cr3t-password-value-not-in-argv";
        let inv = Invocation {
            args: vec![OsString::from("identity"), OsString::from("create")],
            password: Some(Password::new(secret.as_bytes().to_vec()).unwrap()),
            timeout: None,
        };
        let envelope = runner.run(inv).await.unwrap();
        let Envelope::Ok { data } = envelope else {
            panic!("expected Ok envelope")
        };
        assert_eq!(
            data.get("stdin_len").and_then(Value::as_u64),
            Some(secret.len() as u64)
        );
        let argv = data.get("argv").and_then(Value::as_str).unwrap();
        assert!(argv.contains("--password-stdin"));
        assert!(!argv.contains(secret));
    }

    #[tokio::test]
    async fn timeout_kills_the_whole_process_group() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("hyphae-home");
        tokio::fs::create_dir_all(&home).await.unwrap();
        let marker = tmp.path().join("marker");
        // The direct child ("sh") must itself block past the timeout (`sleep 5`
        // in the foreground) so the command doesn't just exit normally before
        // the timeout fires. Separately, a BACKGROUNDED grandchild
        // (`( sleep 1; touch ... ) &`) is what actually exercises group-wide
        // killing: it is not sh's foreground child, so killing only the direct
        // `sh` process (e.g. via `kill_on_drop`'s default single-pid SIGKILL)
        // leaves it running free to finish its own `sleep 1` and touch the
        // marker — only killing the whole process group reaches it too. A
        // foreground-only `sleep; touch` (the previous fixture) can't tell the
        // two apart: the script interpreter itself dies either way, so `touch`
        // never runs regardless of whether the kill was group-wide.
        //
        // Verified by mutation: temporarily removing both `cmd.process_group(0)`
        // (runner.rs) and the `kill_process_group_best_effort` call in the
        // timeout branch makes this test fail with `marker` present (the
        // grandchild's `touch` ran); restoring either one makes it pass again.
        let script = format!(
            "#!/bin/sh\n( sleep 1; touch \"{}\" ) &\nsleep 5\n",
            marker.display()
        );
        let bin = install_fixture(tmp.path(), "slow.sh", &script, &tmp.path().join("bin")).await;
        let runner = HyphaeRunner::new(bin, home, Duration::from_secs(5));

        let inv = Invocation {
            args: vec![],
            password: None,
            timeout: Some(Duration::from_millis(500)),
        };
        let start = std::time::Instant::now();
        let err = runner.run(inv).await.unwrap_err();
        assert!(matches!(err, RunnerError::Timeout(_)));
        assert!(start.elapsed() < Duration::from_secs(2));

        // Give the grandchild's own `sleep 1` plenty of room (well past its own
        // 1s sleep, with slack for scheduling jitter) to have finished and run
        // `touch` if it (and not just its parent `sh`) hadn't actually been
        // killed.
        tokio::time::sleep(Duration::from_millis(3000)).await;
        assert!(
            !marker.exists(),
            "process group was not killed before the backgrounded grandchild completed"
        );
    }

    #[tokio::test]
    async fn no_password_means_null_stdin_and_no_password_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("hyphae-home");
        tokio::fs::create_dir_all(&home).await.unwrap();
        let script =
            "#!/bin/sh\necho \"{\\\"ok\\\":true,\\\"data\\\":{\\\"argv\\\":\\\"$*\\\"}}\"\n";
        let bin =
            install_fixture(tmp.path(), "echo-argv.sh", script, &tmp.path().join("bin")).await;
        let runner = HyphaeRunner::new(bin, home, Duration::from_secs(5));

        let inv = Invocation {
            args: vec![OsString::from("identity"), OsString::from("list")],
            password: None,
            timeout: None,
        };
        let envelope = runner.run(inv).await.unwrap();
        let Envelope::Ok { data } = envelope else {
            panic!("expected Ok envelope")
        };
        let argv = data.get("argv").and_then(Value::as_str).unwrap();
        assert!(!argv.contains("--password-stdin"));
    }

    #[tokio::test]
    async fn installed_copy_is_mode_0500() {
        let tmp = tempfile::tempdir().unwrap();
        let script = "#!/bin/sh\necho '{\"ok\":true,\"data\":{}}'\n";
        let bin =
            install_fixture(tmp.path(), "mode-check.sh", script, &tmp.path().join("bin")).await;
        let perms = tokio::fs::metadata(bin.path()).await.unwrap().permissions();
        assert_eq!(perms.mode() & 0o777, 0o500);
    }
}
