//! Invoke the pinned setup contract without persisting credentials.
//!
//! Each attempt gets a fresh `instances/<id>/components/gen-<random>` directory.
//! Generations are never overwritten: no `--force` or default model selection.
//! v0.5.0 uses the IL-04 probe; v0.6.0 delegates discovery to setup --auto.
//! The caller serializes instance operations.
//!
//! Contract source: iDoris tag v0.5.0, crates/idoris-router/src/{cli,setup_omlx}.rs
//! and docs/interfaces/iDoris-Agent24-边界与接口规范.md (L2). The released
//! binary's `setup --help` prints the same unified usage as `cli.rs`.

use crate::{
    install::InstalledVersion,
    omlx_probe::{Endpoint, ProbeResult, ProbeStatus, ServiceIdentity},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Daily,
    Deep,
    Embed,
}
impl Role {
    fn as_str(self) -> &'static str {
        match self {
            Self::Daily => "daily",
            Self::Deep => "deep",
            Self::Embed => "embed",
        }
    }
}
impl TryFrom<&str> for Role {
    type Error = SetupError;
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "daily" => Ok(Self::Daily),
            "deep" => Ok(Self::Deep),
            "embed" => Ok(Self::Embed),
            _ => Err(SetupError::InvalidBindings),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupCode {
    EngineNotInstalled,
    PortInUse,
    ModelNotInstalled,
    Unreachable,
    AuthRequired,
    AuthFailed,
    NotOmlx,
    NotOpenaiCompatible,
    InvalidId,
    IdConflict,
    SetupFailed,
    ModelNotFound,
    InvalidEndpoint,
    InvalidRole,
    OutDirNotEmpty,
    ValidationFailed,
    WriteFailed,
    InvalidApiKeyInput,
}
impl SetupCode {
    pub fn code(self) -> &'static str {
        match self {
            Self::EngineNotInstalled => "engine_not_installed",
            Self::PortInUse => "port_in_use",
            Self::ModelNotInstalled => "model_not_installed",
            Self::Unreachable => "unreachable",
            Self::AuthRequired => "auth_required",
            Self::AuthFailed => "auth_failed",
            Self::NotOmlx => "not_omlx",
            Self::NotOpenaiCompatible => "not_openai_compatible",
            Self::InvalidId => "invalid_id",
            Self::IdConflict => "id_conflict",
            Self::SetupFailed => "setup_failed",
            Self::ModelNotFound => "model_not_found",
            Self::InvalidEndpoint => "invalid_endpoint",
            Self::InvalidRole => "invalid_role",
            Self::OutDirNotEmpty => "out_dir_not_empty",
            Self::ValidationFailed => "validation_failed",
            Self::WriteFailed => "write_failed",
            Self::InvalidApiKeyInput => "invalid_api_key_input",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SetupError {
    #[error("iDoris setup rejected: {}", .0.code())]
    Rejected(SetupCode),
    #[error("iDoris budget or explicit core reservation is unavailable")]
    BudgetUnavailable,
    #[error("oMLX authentication required at {endpoint}")]
    AuthRequired { endpoint: String },
    #[error("setup requires a ready oMLX probe and unique daily/deep bindings including daily")]
    InvalidBindings,
    #[error("setup path is outside the managed instance")]
    UnsafePath,
    #[error("setup response does not match the pinned contract")]
    InvalidResponse,
    #[error("setup output exceeds the size limit")]
    OutputTooLarge,
    #[error("setup timed out")]
    TimedOut,
    #[error("setup cancelled")]
    Cancelled,
    #[error("setup process or filesystem I/O failed")]
    Io,
}

/// Only constructed from a hardware report and an explicit core allocation.
#[derive(Clone, Copy, Debug)]
pub struct SetupBudget(u64);
impl SetupBudget {
    pub fn from_report(
        report: Option<&agent24_hw::HardwareReport>,
        core_reserved_bytes: Option<u64>,
    ) -> Result<Self, SetupError> {
        let report = report
            .filter(|report| report.profile.total_mem_bytes > 0)
            .ok_or(SetupError::BudgetUnavailable)?;
        let core = core_reserved_bytes.ok_or(SetupError::BudgetUnavailable)?;
        Ok(Self(report.idoris_budget_bytes(core)))
    }
    pub fn bytes(self) -> u64 {
        self.0
    }
}

#[derive(Debug)]
pub struct SetupOutcome {
    pub components_dir: PathBuf,
    pub bindings: BTreeMap<Role, String>,
    pub auth_required: bool,
    pub omlx: Option<OmlxSetup>,
}

/// The discovery result belongs to iDoris; Agent24 only displays it.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OmlxSetup {
    pub endpoint: String,
    pub mode: OmlxMode,
    pub engine: Engine,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OmlxMode {
    Adopt,
    Managed,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Engine {
    pub engine_kind: String,
    pub installed: bool,
    pub executable: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// Create a fresh single-card OpenAI-compatible component generation. Values
/// are validated by the host before reaching this writer.
// Remove this v0.5.0 writer when the embedded lock is bumped to v0.6.0.
pub fn write_openai_compat_card(
    instance_root: &Path,
    endpoint: &str,
    daily: &str,
    deep: &str,
    embed: Option<&str>,
) -> Result<SetupOutcome, SetupError> {
    let components = components_root(instance_root)?;
    let generation = tempfile::Builder::new()
        .prefix("gen-")
        .tempdir_in(&components)
        .map_err(|_| SetupError::Io)?;
    let capabilities = if embed.is_some() {
        "chat, embedding"
    } else {
        "chat"
    };
    let embed_line = embed
        .map(|id| format!("    embed: \"{id}\"\n"))
        .unwrap_or_default();
    let card = format!(
        "provider:\n  id: lmstudio\n  family: local\n  tier: local\n  capabilities: [{capabilities}]\n  privacy_class: local_only\n  cost: {{ input_per_m: 0, output_per_m: 0 }}\n  locality: loopback\nform: http_service\nendpoint: \"{endpoint}\"\nversion_pin: \"lmstudio@0\"\nprivacy_class: local_only\nallowed_egress: [loopback]\nfallback_policy: fail_closed\nfail_closed: true\nload_policy: {{ mode: resident, keepalive: {{ pinned: true }}, admission: coexist }}\nextensions:\n  _degradation: \"Removing these bindings disables role aliases on this backend.\"\n  idoris_role_models:\n    daily: \"{daily}\"\n    deep: \"{deep}\"\n"
    );
    std::fs::write(generation.path().join("lmstudio.yaml"), card + &embed_line)
        .map_err(|_| SetupError::Io)?;
    let path = generation.keep();
    Ok(SetupOutcome {
        components_dir: path,
        bindings: [
            (Role::Daily, daily.to_owned()),
            (Role::Deep, deep.to_owned()),
        ]
        .into_iter()
        .chain(embed.map(|id| (Role::Embed, id.to_owned())))
        .collect(),
        auth_required: false,
        omlx: None,
    })
}

/// Shared gate for setup features introduced in iDoris v0.6.0.
pub(crate) fn upstream_setup_enabled(lock: &crate::IdorisLock) -> bool {
    semver::Version::parse(lock.tag.trim_start_matches('v'))
        .is_ok_and(|version| version >= semver::Version::new(0, 6, 0))
        || (cfg!(debug_assertions) && std::env::var("AGENT24_DEV_IDORIS_V6").as_deref() == Ok("1"))
}

/// Generate a loopback OpenAI-compatible card using iDoris's upstream contract.
pub async fn run_setup_upstream(
    installed: &InstalledVersion,
    instance_root: &Path,
    endpoint: &str,
    bindings: &[(Role, String)],
    id: &str,
    cancel: &CancellationToken,
) -> Result<SetupOutcome, SetupError> {
    run_setup_upstream_with_timeout(
        installed,
        instance_root,
        endpoint,
        bindings,
        id,
        cancel,
        Duration::from_secs(30),
    )
    .await
}

async fn run_setup_upstream_with_timeout(
    installed: &InstalledVersion,
    instance_root: &Path,
    endpoint: &str,
    bindings: &[(Role, String)],
    id: &str,
    cancel: &CancellationToken,
    timeout: Duration,
) -> Result<SetupOutcome, SetupError> {
    use std::process::Stdio;
    use tokio::process::Command;

    if cancel.is_cancelled() {
        return Err(SetupError::Cancelled);
    }
    let mut url =
        url::Url::parse(endpoint).map_err(|_| SetupError::Rejected(SetupCode::InvalidEndpoint))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(SetupError::Rejected(SetupCode::InvalidEndpoint));
    }
    url.set_path("");
    url.set_query(None);
    url.set_fragment(None);
    // Validate the origin after dropping OpenAI API suffixes such as /v1.
    let origin = url.origin().ascii_serialization();
    let origin = if url.host_str() == Some("localhost") {
        origin.replacen("://localhost", "://127.0.0.1", 1)
    } else {
        origin
    };
    Endpoint::parse(&origin).map_err(|_| SetupError::Rejected(SetupCode::InvalidEndpoint))?;
    let expected: BTreeMap<_, _> = bindings.iter().cloned().collect();
    if expected.len() != bindings.len()
        || !expected.contains_key(&Role::Daily)
        || !expected.contains_key(&Role::Deep)
        || bindings
            .iter()
            .any(|(_, model)| model.is_empty() || model.chars().any(char::is_control))
    {
        return Err(SetupError::InvalidBindings);
    }
    let components = components_root(instance_root)?;
    let generation = tempfile::Builder::new()
        .prefix("gen-")
        .tempdir_in(&components)
        .map_err(|_| SetupError::Io)?;
    let out_dir = generation
        .path()
        .canonicalize()
        .map_err(|_| SetupError::Io)?;
    let mut cmd = Command::new(
        installed
            .binary()
            .canonicalize()
            .map_err(|_| SetupError::Io)?,
    );
    cmd.env_clear()
        .env("PATH", "/usr/bin:/bin")
        .current_dir(&components)
        .args([
            "setup",
            "upstream",
            "--kind",
            "openai_compat",
            "--endpoint",
            &origin,
            "--out-dir",
        ])
        .arg(&out_dir);
    for (role, model) in &expected {
        cmd.arg("--bind").arg(format!("{}={model}", role.as_str()));
    }
    cmd.args(["--id", id])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|_| SetupError::Io)?;
    let process_group = child
        .id()
        .and_then(|pid| i32::try_from(pid).ok())
        .and_then(rustix::process::Pid::from_raw);
    let stdout = child.stdout.take().ok_or(SetupError::Io)?;
    let stderr = child.stderr.take().ok_or(SetupError::Io)?;
    let operation = async {
        let wait = async { child.wait().await.map_err(|_| SetupError::Io) };
        let (stdout, _, status) =
            tokio::try_join!(read_bounded(stdout), read_bounded(stderr), wait)?;
        Ok::<_, SetupError>((stdout, status))
    };
    let result = tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(SetupError::Cancelled),
        result = tokio::time::timeout(timeout, operation) => result.unwrap_or(Err(SetupError::TimedOut)),
    };
    if result.is_err() {
        if let Some(pid) = process_group {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    let (stdout, status) = result?;
    if stdout
        .strip_suffix(b"\n")
        .unwrap_or(&stdout)
        .contains(&b'\n')
    {
        return Err(SetupError::InvalidResponse);
    }
    let reply: UpstreamReply =
        serde_json::from_slice(&stdout).map_err(|_| SetupError::InvalidResponse)?;
    match reply {
        UpstreamReply::Error { code } if status.code() == Some(1) => {
            let code = serde_json::from_value(serde_json::Value::String(code))
                .unwrap_or(SetupCode::SetupFailed);
            Err(SetupError::Rejected(code))
        }
        UpstreamReply::Ok {
            card_file,
            models,
            bindings,
            id: returned_id,
        } if status.success() => {
            if returned_id != id
                || bindings != expected
                || bindings.values().any(|model| !models.contains(model))
            {
                return Err(SetupError::InvalidResponse);
            }
            let card = out_dir
                .join(card_file)
                .canonicalize()
                .map_err(|_| SetupError::UnsafePath)?;
            if !card.starts_with(&out_dir) || !card.is_file() {
                return Err(SetupError::UnsafePath);
            }
            let _ = generation.keep();
            Ok(SetupOutcome {
                components_dir: out_dir,
                bindings,
                auth_required: false,
                omlx: None,
            })
        }
        _ => Err(SetupError::InvalidResponse),
    }
}

#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum UpstreamReply {
    Ok {
        card_file: PathBuf,
        models: Vec<String>,
        bindings: BTreeMap<Role, String>,
        id: String,
    },
    Error {
        code: String,
    },
}

/// A daily binding must be selected from the probe; deep is opt-in. Missing
/// budget means disconnected, while a computed zero is passed through intact.
/// `instance_root` is an absolute `<managed-root>/instances/<instance-id>` path;
/// the managed root must already exist. Only its components generations are
/// handed to setup. Successful generations stay in place for caller activation.
#[allow(clippy::too_many_arguments)]
pub async fn run_setup(
    installed_version: &InstalledVersion,
    instance_root: &Path,
    omlx_endpoint: &ProbeResult,
    omlx_key: Option<Zeroizing<String>>,
    bindings: &[(Role, String)],
    budget: Option<SetupBudget>,
    cancel: &CancellationToken,
) -> Result<SetupOutcome, SetupError> {
    run_setup_with_timeout(
        installed_version,
        instance_root,
        omlx_endpoint,
        omlx_key,
        bindings,
        budget,
        cancel,
        Duration::from_secs(30),
    )
    .await
}

const MAX_OUTPUT_BYTES: u64 = 1024 * 1024;

#[allow(clippy::too_many_arguments)]
async fn run_setup_with_timeout(
    installed: &InstalledVersion,
    instance: &Path,
    probe: &ProbeResult,
    key: Option<Zeroizing<String>>,
    bindings: &[(Role, String)],
    budget: Option<SetupBudget>,
    cancel: &CancellationToken,
    timeout: Duration,
) -> Result<SetupOutcome, SetupError> {
    run_omlx_setup(
        installed,
        instance,
        Some(probe),
        None,
        key,
        bindings,
        budget,
        cancel,
        timeout,
    )
    .await
}

/// v0.6.0 delegates endpoint discovery and model validation to iDoris.
pub async fn run_setup_auto(
    installed: &InstalledVersion,
    instance: &Path,
    key: Option<Zeroizing<String>>,
    bindings: &[(Role, String)],
    budget: Option<SetupBudget>,
    cancel: &CancellationToken,
) -> Result<SetupOutcome, SetupError> {
    let first = run_omlx_setup(
        installed,
        instance,
        None,
        None,
        None,
        bindings,
        budget,
        cancel,
        Duration::from_secs(30),
    )
    .await;
    match first {
        Err(SetupError::AuthRequired { endpoint }) if key.is_some() => {
            run_omlx_setup(
                installed,
                instance,
                None,
                Some(&endpoint),
                key,
                bindings,
                budget,
                cancel,
                Duration::from_secs(30),
            )
            .await
        }
        result => result,
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_omlx_setup(
    installed: &InstalledVersion,
    instance: &Path,
    probe: Option<&ProbeResult>,
    discovered_endpoint: Option<&str>,
    key: Option<Zeroizing<String>>,
    bindings: &[(Role, String)],
    budget: Option<SetupBudget>,
    cancel: &CancellationToken,
    timeout: Duration,
) -> Result<SetupOutcome, SetupError> {
    use std::process::Stdio;
    use tokio::{io::AsyncWriteExt, process::Command};

    let budget = budget.ok_or(SetupError::BudgetUnavailable)?;
    if cancel.is_cancelled() {
        return Err(SetupError::Cancelled);
    }
    let expected: BTreeMap<_, _> = bindings.iter().cloned().collect();
    if !expected.contains_key(&Role::Daily)
        || expected.len() != bindings.len()
        || bindings
            .iter()
            .any(|(_, model)| model.is_empty() || model.chars().any(char::is_control))
    {
        return Err(SetupError::InvalidBindings);
    }
    if let Some(probe) = probe {
        let ProbeStatus::Ready { models } = &probe.status else {
            return Err(SetupError::InvalidBindings);
        };
        if probe.identity != ServiceIdentity::Omlx
            || bindings.iter().any(|(_, model)| !models.contains(model))
        {
            return Err(SetupError::InvalidBindings);
        }
    }
    if key.as_ref().is_some_and(|key| {
        key.is_empty()
            || key.len() > 4095
            || key.chars().any(|c| c.is_whitespace() || c.is_control())
    }) {
        return Err(SetupError::Rejected(SetupCode::InvalidApiKeyInput));
    }
    let components = components_root(instance)?;
    let generation = tempfile::Builder::new()
        .prefix("gen-")
        .tempdir_in(&components)
        .map_err(|_| SetupError::Io)?;
    let out_dir = generation
        .path()
        .canonicalize()
        .map_err(|_| SetupError::Io)?;
    let mut cmd = Command::new(
        installed
            .binary()
            .canonicalize()
            .map_err(|_| SetupError::Io)?,
    );
    cmd.env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("IDORIS_ALLOCATED_BUDGET_BYTES", budget.bytes().to_string())
        .current_dir(&components)
        .args(["setup", "omlx"]);
    if let Some(probe) = probe {
        cmd.args(["--endpoint", probe.endpoint.as_str(), "--out-dir"])
            .arg(&out_dir)
            .args(["--load-mode", "resident"]);
    } else {
        cmd.arg("--out-dir").arg(&out_dir);
        if let Some(endpoint) = discovered_endpoint {
            cmd.args(["--endpoint", endpoint]);
        } else {
            cmd.arg("--auto");
        }
        // iDoris reads the local model directory and discovers the installed engine.
        if let Some(home) = std::env::var_os("HOME") {
            cmd.env("HOME", home);
        }
        cmd.env(
            "PATH",
            std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into()),
        );
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true);
    for (role, model) in bindings {
        cmd.arg("--bind").arg(format!("{}={model}", role.as_str()));
    }
    if key.is_some() {
        cmd.arg("--api-key-stdin");
    }
    let mut child = cmd.spawn().map_err(|_| SetupError::Io)?;
    let process_group = child
        .id()
        .and_then(|pid| i32::try_from(pid).ok())
        .and_then(rustix::process::Pid::from_raw);
    let mut stdin = child.stdin.take().ok_or(SetupError::Io)?;
    let stdout = child.stdout.take().ok_or(SetupError::Io)?;
    let stderr = child.stderr.take().ok_or(SetupError::Io)?;
    let operation = async {
        let write = async {
            if let Some(key) = &key {
                stdin
                    .write_all(key.as_bytes())
                    .await
                    .map_err(|_| SetupError::Io)?;
                stdin.write_all(b"\n").await.map_err(|_| SetupError::Io)?;
            }
            // EOF is part of the protocol, including the unauthenticated case.
            drop(stdin);
            Ok::<_, SetupError>(())
        };
        let wait = async { child.wait().await.map_err(|_| SetupError::Io) };
        let (_, stdout, _, status) =
            tokio::try_join!(write, read_bounded(stdout), read_bounded(stderr), wait)?;
        Ok::<_, SetupError>((stdout, status))
    };
    let result = tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(SetupError::Cancelled),
        result = tokio::time::timeout(timeout, operation) => result.unwrap_or(Err(SetupError::TimedOut)),
    };
    if result.is_err() {
        // Kill the group as well as the direct child: a pipe-owning descendant
        // must not survive a cancelled or oversized setup attempt.
        if let Some(pid) = process_group {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    let (stdout, status) = result?;
    // Raw output and upstream messages never become diagnostics or log text.
    let reply: Reply = serde_json::from_slice(&stdout).map_err(|_| SetupError::InvalidResponse)?;
    match reply {
        Reply::Error {
            code: SetupCode::AuthRequired,
            endpoint: Some(endpoint),
        } if !status.success() && probe.is_none() => {
            Endpoint::parse(&endpoint).map_err(|_| SetupError::InvalidResponse)?;
            Err(SetupError::AuthRequired { endpoint })
        }
        Reply::Error { code, .. } if !status.success() => Err(SetupError::Rejected(code)),
        Reply::Ok {
            endpoint,
            auth,
            models,
            bindings,
            components_dir,
            next,
            mode,
            engine,
        } if status.success() => {
            if probe.is_some_and(|p| Endpoint::parse(&endpoint).ok().as_ref() != Some(&p.endpoint))
                || bindings != expected
                || bindings.values().any(|model| !models.contains(model))
                || !matches!(auth.as_str(), "none" | "required")
                || next.env.get("IDORIS_COMPONENTS_DIR") != Some(&components_dir)
            {
                return Err(SetupError::InvalidResponse);
            }
            let omlx = if probe.is_none() {
                let mode = mode.ok_or(SetupError::InvalidResponse)?;
                let engine = engine
                    .filter(|e| e.engine_kind == "omlx")
                    .ok_or(SetupError::InvalidResponse)?;
                Endpoint::parse(&endpoint).map_err(|_| SetupError::InvalidResponse)?;
                Some(OmlxSetup {
                    endpoint,
                    mode,
                    engine: *engine,
                })
            } else {
                None
            };
            let returned = Path::new(&components_dir)
                .canonicalize()
                .map_err(|_| SetupError::UnsafePath)?;
            if returned != out_dir
                || returned.parent() != Some(components.as_path())
                || generation
                    .path()
                    .symlink_metadata()
                    .map_err(|_| SetupError::UnsafePath)?
                    .file_type()
                    .is_symlink()
                || components
                    .canonicalize()
                    .map_err(|_| SetupError::UnsafePath)?
                    != components
            {
                return Err(SetupError::UnsafePath);
            }
            for name in ["omlx.yaml", "subscription.yaml"] {
                let path = returned.join(name);
                let meta = path
                    .symlink_metadata()
                    .map_err(|_| SetupError::UnsafePath)?;
                if !meta.is_file()
                    || meta.file_type().is_symlink()
                    || path
                        .canonicalize()
                        .map_err(|_| SetupError::UnsafePath)?
                        .parent()
                        != Some(returned.as_path())
                {
                    return Err(SetupError::UnsafePath);
                }
            }
            let _ = generation.keep();
            Ok(SetupOutcome {
                components_dir: returned,
                bindings,
                auth_required: auth == "required",
                omlx,
            })
        }
        _ => Err(SetupError::InvalidResponse),
    }
}

async fn read_bounded(
    reader: impl tokio::io::AsyncRead + Unpin,
) -> Result<Zeroizing<Vec<u8>>, SetupError> {
    use tokio::io::AsyncReadExt;
    let mut bytes = Zeroizing::new(Vec::new());
    reader
        .take(MAX_OUTPUT_BYTES + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| SetupError::Io)?;
    if bytes.len() as u64 > MAX_OUTPUT_BYTES {
        return Err(SetupError::OutputTooLarge);
    }
    Ok(bytes)
}

fn components_root(instance: &Path) -> Result<PathBuf, SetupError> {
    use std::{fs, path::Component};
    if !instance.is_absolute()
        || instance
            .components()
            .any(|c| !matches!(c, Component::RootDir | Component::Normal(_)))
    {
        return Err(SetupError::UnsafePath);
    }
    let instances = instance
        .parent()
        .filter(|p| p.file_name().is_some_and(|n| n == "instances"))
        .ok_or(SetupError::UnsafePath)?;
    let root = instances
        .parent()
        .ok_or(SetupError::UnsafePath)?
        .canonicalize()
        .map_err(|_| SetupError::UnsafePath)?;
    let mut current = root;
    for name in [
        std::ffi::OsStr::new("instances"),
        instance.file_name().ok_or(SetupError::UnsafePath)?,
        std::ffi::OsStr::new("components"),
    ] {
        current.push(name);
        match fs::create_dir(&current) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(SetupError::Io),
        }
        let meta = current
            .symlink_metadata()
            .map_err(|_| SetupError::UnsafePath)?;
        if !meta.is_dir()
            || meta.file_type().is_symlink()
            || current.canonicalize().map_err(|_| SetupError::UnsafePath)? != current
        {
            return Err(SetupError::UnsafePath);
        }
    }
    Ok(current)
}

// v0.5.0 has no closed JSON schema. Ignore additive fields but require the
// consumed success fields and the frozen status/code discriminants. Error
// messages and unused `next` fields are never retained in public results.
#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum Reply {
    Ok {
        endpoint: String,
        auth: String,
        models: Vec<String>,
        bindings: BTreeMap<Role, String>,
        components_dir: String,
        next: Next,
        #[serde(default)]
        mode: Option<OmlxMode>,
        #[serde(default)]
        engine: Option<Box<Engine>>,
    },
    Error {
        code: SetupCode,
        #[serde(default)]
        endpoint: Option<String>,
    },
}
#[derive(Deserialize)]
struct Next {
    env: BTreeMap<String, String>,
}

#[cfg(test)]
mod tests;
