//! One owner for managed installation metadata, supervisor and gateway publication.
use crate::managed_idoris::{
    ManagedConfig, ManagedIdoris, SecretError, SecretSource, SupervisorReady,
};
use agent24_idoris::{
    IdorisLock,
    download::VerifiedArchive,
    install::{Codesign, InstalledVersion, SignatureVerifier, VerifyMode},
    keychain::{Keychain, KeychainError, SystemKeychain},
    omlx_probe::{ApiKey, Endpoint, OmlxProbe, ProbeStatus},
    setup::Role,
    supervisor::{
        Budget, ComponentsDir, DataPaths, LaunchInputs, Ports, StartConfig, Supervisor,
        SupervisorError,
    },
};
use agent24_models::{IdorisGateway, ModelError, router::ModelRouter};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex as SyncMutex, Weak},
    time::Duration,
};
use tokio::sync::{Mutex, Notify, OnceCell, watch};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

mod provision;
pub use provision::{
    Candidates, ConnectConsent, ConnectRequest, InstallRequest, Provisioner, SystemProvisioner,
};

/// Drain (30s), TERM (17s), KILL/reap and scheduling margin.
pub const SHUTDOWN_BUDGET: Duration = Duration::from_secs(50);

/// Local, non-sensitive receipt written by the install/connect coordinator (IL-11).
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Installation {
    pub version: String,
    pub release_dir: PathBuf,
    pub archive_path: PathBuf,
    pub components_dir: Option<PathBuf>,
    pub key_id: Option<String>,
    pub data_port: u16,
    pub admin_port: u16,
    pub roles: Vec<Role>,
    pub core_reserved_bytes: Option<u64>,
    pub omlx_url: Option<String>,
    pub omlx_key_id: Option<String>,
    pub desired_running: bool,
    #[serde(default)]
    pub verification: Option<VerificationReceipt>,
    /// Managed root (canonical) the release was installed under; absent in receipts
    /// written before ADR-038 D1.
    #[serde(default)]
    pub install_root: Option<PathBuf>,
    /// When an explicit Install last verified notarization online (RFC 3339). Start trusts
    /// this together with the digests and the offline codesign checks; without it the next
    /// explicit Install redoes the check, and Start is never blocked for that reason.
    #[serde(default)]
    pub notarized_verified_at: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationReceipt {
    pub model_id: String,
    pub record_id: String,
    pub locality: String,
    pub verified_at: String,
}

impl Installation {
    fn validate(&self) -> Result<(), Code> {
        let reference = |id: &str| {
            !id.is_empty()
                && id.len() <= 128
                && !id.starts_with("idk_")
                && !id.starts_with("sk-")
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        };
        if self.version != IdorisLock::embedded().tag.trim_start_matches('v')
            || !self.release_dir.is_absolute()
            || !self.archive_path.is_absolute()
            || self
                .components_dir
                .as_ref()
                .is_some_and(|p| !p.is_absolute())
            || self.data_port == 0
            || self.admin_port == 0
            || self.data_port == self.admin_port
            || self
                .key_id
                .as_deref()
                .is_some_and(|id| !id.starts_with("vk_") || !reference(id))
            || self.omlx_key_id.as_deref().is_some_and(|id| !reference(id))
            || self
                .omlx_url
                .as_deref()
                .is_some_and(|url| Endpoint::parse(url).is_err())
        {
            return Err(Code::InvalidRequest);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    NotInstalled,
    NotConfigured,
    Stopped,
    Starting,
    Running,
    Failed,
    InstanceConflict,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Code {
    NotInstalled,
    NotConfigured,
    InstanceConflict,
    KeychainLocked,
    KeyUnavailable,
    ContractMismatch,
    StartFailed,
    StopTimeout,
    Busy,
    Cancelled,
    NotFound,
    NotCancellable,
    ShuttingDown,
    InvalidRequest,
    PersistenceFailed,
    OmlxUnreachable,
    OmlxAuthRequired,
    OmlxAuthRejected,
    UnknownService,
    ModelNotFound,
    ConsentRequired,
    SetupFailed,
    VerifyFailed,
    ReplaceRequired,
}
impl Code {
    pub fn http_status(self) -> axum::http::StatusCode {
        use axum::http::StatusCode as H;
        match self {
            Self::NotFound => H::NOT_FOUND,
            Self::ConsentRequired
            | Self::ModelNotFound
            | Self::UnknownService
            | Self::NotInstalled
            | Self::NotConfigured
            | Self::ContractMismatch => H::UNPROCESSABLE_ENTITY,
            Self::ReplaceRequired
            | Self::InstanceConflict
            | Self::Busy
            | Self::NotCancellable
            | Self::Cancelled => H::CONFLICT,
            Self::OmlxUnreachable
            | Self::OmlxAuthRequired
            | Self::OmlxAuthRejected
            | Self::KeychainLocked
            | Self::KeyUnavailable
            | Self::ShuttingDown => H::SERVICE_UNAVAILABLE,
            Self::SetupFailed | Self::VerifyFailed | Self::StartFailed => H::BAD_GATEWAY,
            Self::StopTimeout => H::GATEWAY_TIMEOUT,
            Self::InvalidRequest => H::BAD_REQUEST,
            Self::PersistenceFailed => H::INTERNAL_SERVER_ERROR,
        }
    }
    pub fn message(self) -> &'static str {
        match self {
            Self::OmlxUnreachable => "无法连接 oMLX",
            Self::OmlxAuthRequired => "oMLX 需要凭据",
            Self::OmlxAuthRejected => "oMLX 拒绝了凭据",
            Self::UnknownService => "端点不是已确认的 oMLX 服务",
            Self::ModelNotFound => "请选择 oMLX 中存在的模型",
            Self::ConsentRequired => "请确认凭据交接与选定模型",
            Self::SetupFailed => "iDoris 配置失败",
            Self::VerifyFailed => "iDoris 对话验证失败",
            Self::ReplaceRequired => "已有连接，请显式确认替换",
            Self::NotInstalled => "尚未安装 iDoris",
            Self::NotConfigured => "请先完成 iDoris 配置",
            Self::InstanceConflict => "同一数据库已有实例或遗留进程身份无法确认",
            Self::KeychainLocked => "请解锁钥匙串并允许 Agent24 访问",
            Self::KeyUnavailable => "钥匙串中缺少凭据",
            Self::ContractMismatch => "iDoris 就绪契约不匹配",
            Self::StartFailed => "iDoris 启动失败",
            Self::StopTimeout => "iDoris 停止超时，已执行强制回收",
            Self::Busy => "已有生命周期操作正在执行",
            Self::Cancelled => "操作已取消",
            Self::NotFound => "操作不存在",
            Self::NotCancellable => "停止回收阶段不可取消",
            Self::ShuttingDown => "宿主正在关停",
            Self::InvalidRequest => "请求格式不正确",
            Self::PersistenceFailed => "无法保存生命周期记录",
        }
    }
}
impl From<SupervisorError> for Code {
    fn from(error: SupervisorError) -> Self {
        match error {
            SupervisorError::InstanceConflict => Self::InstanceConflict,
            SupervisorError::Contract => Self::ContractMismatch,
            SupervisorError::Configuration => Self::NotConfigured,
            SupervisorError::Stopped => Self::Cancelled,
            _ => Self::StartFailed,
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Status {
    pub state: State,
    pub version: Option<String>,
    pub data_port: Option<u16>,
    pub admin_port: Option<u16>,
    pub roles: Vec<Role>,
    pub last_error: Option<Code>,
    pub current_operation: Option<String>,
    pub verification: Option<VerificationReceipt>,
    pub budget_suggestion: Option<agent24_hw::IdorisBudgetSuggestion>,
    pub budget_unavailable_reason: Option<String>,
    /// The saved release is outside the managed root; the next Install migrates it.
    #[serde(default)]
    pub migration_pending: bool,
    /// Why the last migration attempt failed; the legacy install keeps working.
    #[serde(default)]
    pub last_migration_error: Option<Code>,
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Install,
    Connect,
    Start,
    Stop,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Downloading,
    Verifying,
    Installing,
    Setup,
    IssuingKey,
    VerifyingConnection,
    RollingBack,
    Done,
    Queued,
    Starting,
    Connecting,
    Stopping,
    Completed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Succeeded,
    Failed,
    Cancelled,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Operation {
    pub operation_id: String,
    pub action: Action,
    pub phase: Phase,
    pub result: Option<Outcome>,
    pub error_code: Option<Code>,
    pub cancellable: bool,
    idempotency_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    request_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup_code: Option<agent24_idoris::setup::SetupCode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download_progress: Option<agent24_idoris::download::DownloadProgress>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<VerificationReceipt>,
}

/// The API form of an [`Operation`] (I-1 N1). The idempotency key and
/// request fingerprint stay in `operations.json`, where retries across a
/// restart need them; no reader of the lifecycle routes receives them.
#[derive(Serialize)]
pub struct OperationView<'a> {
    operation_id: &'a str,
    action: &'a Action,
    phase: &'a Phase,
    result: &'a Option<Outcome>,
    error_code: &'a Option<Code>,
    cancellable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    setup_code: &'a Option<agent24_idoris::setup::SetupCode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    download_progress: &'a Option<agent24_idoris::download::DownloadProgress>,
    #[serde(skip_serializing_if = "Option::is_none")]
    verification: &'a Option<VerificationReceipt>,
}
impl Operation {
    pub fn view(&self) -> OperationView<'_> {
        OperationView {
            operation_id: &self.operation_id,
            action: &self.action,
            phase: &self.phase,
            result: &self.result,
            error_code: &self.error_code,
            cancellable: self.cancellable,
            setup_code: &self.setup_code,
            download_progress: &self.download_progress,
            verification: &self.verification,
        }
    }
}

pub struct KeychainSource(pub Arc<dyn Keychain>);
#[async_trait]
impl SecretSource for KeychainSource {
    async fn secret(&self, key_id: &str) -> Result<Zeroizing<String>, SecretError> {
        let store = self.0.clone();
        let id = key_id.to_owned();
        tokio::task::spawn_blocking(move || store.get(&id))
            .await
            .map_err(|_| SecretError::Unavailable)?
            .map_err(|e| {
                if e == KeychainError::KeychainLocked {
                    SecretError::Locked
                } else {
                    SecretError::Unavailable
                }
            })
    }
}
fn secret_code(error: SecretError) -> Code {
    match error {
        SecretError::Locked => Code::KeychainLocked,
        SecretError::Unavailable => Code::KeyUnavailable,
    }
}
// Keep credential failures typed across IL-09's generic ModelError boundary.
struct ClassifiedSecrets<'a> {
    source: &'a dyn SecretSource,
    failure: SyncMutex<Option<Code>>,
}
#[async_trait]
impl SecretSource for ClassifiedSecrets<'_> {
    async fn secret(&self, key_id: &str) -> Result<Zeroizing<String>, SecretError> {
        let result = self.source.secret(key_id).await;
        if let Err(error) = &result {
            *self.failure.lock().unwrap_or_else(|e| e.into_inner()) = Some(match error {
                SecretError::Locked => Code::KeychainLocked,
                SecretError::Unavailable => Code::KeyUnavailable,
            });
        }
        result
    }
}
pub struct ReadySignal(pub watch::Receiver<bool>);
#[async_trait]
impl SupervisorReady for ReadySignal {
    async fn wait_ready(&self, cancel: &CancellationToken) -> Result<(), ModelError> {
        let mut signal = self.0.clone();
        loop {
            if *signal.borrow_and_update() {
                return Ok(());
            }
            tokio::select! {
                _ = cancel.cancelled() => return Err(ModelError::Cancelled),
                changed = signal.changed() => if changed.is_err() { return Err(ModelError::Unavailable("iDoris supervisor stopped".into())); },
            }
        }
    }
}

/// Injection retains the real IL-07 supervisor in tests; only discovery is replaced.
#[async_trait]
pub trait Runtime: Send + Sync {
    async fn config(
        &self,
        root: &Path,
        record: &Installation,
        cancel: CancellationToken,
    ) -> Result<StartConfig, Code>;
    async fn inputs(&self, record: &Installation) -> Result<LaunchInputs, Code>;
}
pub struct SystemRuntime {
    pub secrets: Arc<dyn SecretSource>,
    /// Injectable so tests can simulate a machine without Gatekeeper access.
    pub signature: Arc<dyn SignatureVerifier + Send + Sync>,
    /// The pinned lock; tests substitute a fixture archive's digest.
    pub lock: Arc<IdorisLock>,
}
impl SystemRuntime {
    pub fn new(secrets: Arc<dyn SecretSource>) -> Self {
        Self {
            secrets,
            signature: Arc::new(Codesign),
            lock: Arc::new(IdorisLock::embedded().clone()),
        }
    }
}
#[async_trait]
impl Runtime for SystemRuntime {
    async fn config(
        &self,
        root: &Path,
        r: &Installation,
        cancel: CancellationToken,
    ) -> Result<StartConfig, Code> {
        if r.version != IdorisLock::embedded().tag.trim_start_matches('v') {
            return Err(Code::ContractMismatch);
        }
        let dir = r.release_dir.clone();
        let archive = r.archive_path.clone();
        let signature = self.signature.clone();
        let lock = self.lock.clone();
        let installed = tokio::task::spawn_blocking(move || {
            // Start must work offline: digests plus offline codesign only, never Gatekeeper.
            agent24_idoris::install::verify_existing(
                &dir,
                &lock,
                &VerifiedArchive {
                    path: archive,
                    sha256: String::new(),
                },
                signature.as_ref(),
                VerifyMode::AtRest,
            )
        })
        .await
        .map_err(|_| Code::StartFailed)?
        .map_err(|_| Code::NotInstalled)?;
        let components = ComponentsDir::new(
            root,
            r.components_dir.as_deref().ok_or(Code::NotConfigured)?,
        )
        .map_err(Code::from)?;
        let mut config = StartConfig::new(
            installed,
            components,
            DataPaths {
                db: root.join("data/idoris.sqlite3"),
                state: root.join("data/state"),
            },
        )
        .map_err(Code::from)?;
        config.ports = Ports {
            data: r.data_port,
            admin: r.admin_port,
        };
        config.shutdown = cancel;
        Ok(config)
    }
    async fn inputs(&self, r: &Installation) -> Result<LaunchInputs, Code> {
        let core = r.core_reserved_bytes.ok_or(Code::NotConfigured)?;
        let budget = tokio::task::spawn_blocking(move || {
            agent24_hw::HardwareReport::from_env(&agent24_hw::SystemProbe::new())
                .idoris_budget_bytes(core)
        })
        .await
        .map_err(|_| Code::StartFailed)?;
        let key = if let Some(id) = &r.omlx_key_id {
            Some(
                KeychainSource(Arc::new(agent24_idoris::keychain::OmlxKeychain))
                    .secret(id)
                    .await
                    .map_err(secret_code)?,
            )
        } else {
            SystemProvisioner
                .upstream_key()
                .await?
                .map(ApiKey::into_secret)
        };
        {
            let url = r.omlx_url.as_deref().ok_or(Code::NotConfigured)?;
            let endpoint = Endpoint::parse(url).map_err(|_| Code::NotConfigured)?;
            let api_key = key
                .clone()
                .map(ApiKey::new)
                .transpose()
                .map_err(|_| Code::NotConfigured)?;
            let results = OmlxProbe::new()
                .map_err(|_| Code::StartFailed)?
                .probe(Some(&endpoint), api_key.as_ref())
                .await;
            if !results
                .iter()
                .any(|p| p.endpoint == endpoint && matches!(p.status, ProbeStatus::Ready { .. }))
            {
                return Err(Code::NotConfigured);
            }
        }
        Ok(LaunchInputs {
            budget: Budget {
                allocated_bytes: budget,
            },
            omlx_key: key,
        })
    }
}
struct Inner {
    installation: Option<Installation>,
    state: State,
    last_error: Option<Code>,
    operations: BTreeMap<String, Operation>,
    active: Option<(String, CancellationToken)>,
    supervisor: Option<Supervisor>,
    generation: Option<String>,
    closing: bool,
    migration_error: Option<Code>,
    /// Release directory the running supervisor was started from (it keeps using it, and
    /// auto-restarts onto it, until stopped or restarted from a newer receipt).
    supervisor_release: Option<PathBuf>,
}
pub struct IdorisHost {
    /// State: installation receipt, `data/`, `instances/`, operation journal.
    root: PathBuf,
    /// Releases and the download cache (ADR-038 D1 managed root). Equal to `root`
    /// only in tests that never touch the real layout.
    install_root: PathBuf,
    inner: Mutex<Inner>,
    changed: Notify,
    runtime: Arc<dyn Runtime>,
    secrets: Arc<dyn SecretSource>,
    managed: Arc<ManagedIdoris>,
    router: Arc<ModelRouter>,
    draining: SyncMutex<Option<Weak<IdorisGateway>>>,
    provisioner: Arc<dyn Provisioner>,
    budget_suggestion: OnceCell<Result<agent24_hw::IdorisBudgetSuggestion, &'static str>>,
    data_keys: Arc<dyn Keychain>,
    omlx_keys: Arc<dyn Keychain>,
}
fn write_json(path: &Path, value: &impl Serialize) -> Result<(), Code> {
    let parent = path.parent().ok_or(Code::PersistenceFailed)?;
    let write = || -> io::Result<()> {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(parent)?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer(&mut file, value)?;
        file.flush()?;
        file.as_file().sync_all()?;
        file.persist(path).map_err(|e| e.error)?;
        std::fs::File::open(parent)?.sync_all()
    };
    write().map_err(|_| Code::PersistenceFailed)
}
/// Create the parent of the managed root and canonicalize it, so `starts_with` on a
/// receipt's canonical `release_dir` is canonical-to-canonical even when the data
/// directory is a symlink. The leaf itself is left to the installer (it must be 0700).
pub fn canonical_install_root(path: &Path) -> io::Result<PathBuf> {
    let (Some(parent), Some(leaf)) = (path.parent(), path.file_name()) else {
        return Err(io::Error::other("install root has no parent"));
    };
    std::fs::create_dir_all(parent)?;
    Ok(std::fs::canonicalize(parent)?.join(leaf))
}
impl IdorisHost {
    /// `root` holds state (receipt, data, instances); releases go to `install_root`,
    /// the ADR-038 managed root `<data dir>/components/idoris`.
    pub fn system(
        root: PathBuf,
        install_root: PathBuf,
        managed: Arc<ManagedIdoris>,
        router: Arc<ModelRouter>,
    ) -> io::Result<Arc<Self>> {
        let secrets: Arc<dyn SecretSource> = Arc::new(KeychainSource(Arc::new(SystemKeychain)));
        let install_root = canonical_install_root(&install_root)?;
        let mut host = Self::open(
            root,
            managed,
            router,
            Arc::new(SystemRuntime::new(secrets.clone())),
            secrets,
        )?;
        Arc::get_mut(&mut host)
            .ok_or_else(|| io::Error::other("host already shared"))?
            .install_root = install_root;
        Ok(host)
    }
    pub fn open(
        root: PathBuf,
        managed: Arc<ManagedIdoris>,
        router: Arc<ModelRouter>,
        runtime: Arc<dyn Runtime>,
        secrets: Arc<dyn SecretSource>,
    ) -> io::Result<Arc<Self>> {
        let installation: Option<Installation> = match std::fs::read(root.join("installation.json"))
        {
            Ok(body) => Some(serde_json::from_slice(&body).map_err(io::Error::other)?),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        if let Some(record) = &installation {
            record
                .validate()
                .map_err(|_| io::Error::other("invalid iDoris installation receipt"))?;
        }
        let mut operations: BTreeMap<String, Operation> =
            match std::fs::read(root.join("operations.json")) {
                Ok(body) => serde_json::from_slice(&body).map_err(io::Error::other)?,
                Err(e) if e.kind() == io::ErrorKind::NotFound => BTreeMap::new(),
                Err(e) => return Err(e),
            };
        for op in operations.values_mut().filter(|o| o.result.is_none()) {
            op.phase = Phase::Completed;
            op.result = Some(Outcome::Cancelled);
            op.error_code = Some(Code::Cancelled);
            op.cancellable = false;
        }
        let state = match &installation {
            None => State::NotInstalled,
            Some(r) if r.components_dir.is_none() || r.key_id.is_none() => State::NotConfigured,
            Some(_) => State::Stopped,
        };
        if installation.is_some() {
            router.block_idoris("managed iDoris stopped");
        }
        Ok(Arc::new(Self {
            install_root: root.clone(),
            root,
            runtime,
            secrets,
            managed,
            router,
            changed: Notify::new(),
            draining: SyncMutex::new(None),
            provisioner: Arc::new(SystemProvisioner),
            budget_suggestion: OnceCell::new(),
            data_keys: Arc::new(SystemKeychain),
            omlx_keys: Arc::new(agent24_idoris::keychain::OmlxKeychain),
            inner: Mutex::new(Inner {
                installation,
                state,
                last_error: None,
                operations,
                active: None,
                supervisor: None,
                generation: None,
                closing: false,
                migration_error: None,
                supervisor_release: None,
            }),
        }))
    }
    /// IL-11 commits a validated receipt here; HTTP lifecycle requests never accept paths.
    pub async fn record_installation(&self, record: Installation) -> Result<(), Code> {
        record.validate()?;
        let mut inner = self.inner.lock().await;
        if inner.active.is_some() || inner.supervisor.is_some() {
            return Err(Code::Busy);
        }
        if inner.closing {
            return Err(Code::ShuttingDown);
        }
        write_json(&self.root.join("installation.json"), &record)?;
        inner.state = if record.components_dir.is_some() && record.key_id.is_some() {
            State::Stopped
        } else {
            State::NotConfigured
        };
        inner.installation = Some(record);
        Ok(())
    }
    pub async fn status(&self) -> Status {
        let i = self.inner.lock().await;
        let r = i.installation.as_ref();
        let mut status = Status {
            state: i.state,
            version: r.map(|r| r.version.clone()),
            data_port: r.map(|r| r.data_port),
            admin_port: r.map(|r| r.admin_port),
            roles: r.map(|r| r.roles.clone()).unwrap_or_default(),
            last_error: i.last_error,
            current_operation: i.active.as_ref().map(|a| a.0.clone()),
            verification: r.and_then(|r| r.verification.clone()),
            budget_suggestion: None,
            budget_unavailable_reason: None,
            migration_pending: r.is_some_and(|r| !self.in_managed_root(r)),
            last_migration_error: i.migration_error,
        };
        drop(i);
        if status.state == State::NotConfigured
            || (status.state == State::Failed && status.roles.is_empty())
        {
            match self
                .budget_suggestion
                .get_or_init(|| self.provisioner.budget_suggestion())
                .await
            {
                Ok(suggestion) => status.budget_suggestion = Some(suggestion.clone()),
                Err(reason) => status.budget_unavailable_reason = Some((*reason).to_owned()),
            }
        }
        status
    }
    pub async fn operation(&self, id: &str) -> Result<Operation, Code> {
        self.inner
            .lock()
            .await
            .operations
            .get(id)
            .cloned()
            .ok_or(Code::NotFound)
    }
    pub async fn restore(self: &Arc<Self>) -> Result<(), Code> {
        self.recover_connect().await?;
        let restore = self
            .inner
            .lock()
            .await
            .installation
            .as_ref()
            .is_some_and(|r| r.desired_running && r.components_dir.is_some() && r.key_id.is_some());
        if restore {
            self.submit(Action::Start, None).await?;
        }
        Ok(())
    }
    pub async fn submit(
        self: &Arc<Self>,
        action: Action,
        idempotency: Option<&str>,
    ) -> Result<Operation, Code> {
        if !matches!(action, Action::Start | Action::Stop) {
            return Err(Code::InvalidRequest);
        }
        let mut i = self.inner.lock().await;
        if i.closing {
            return Err(Code::ShuttingDown);
        }
        let key = idempotency
            .map(str::to_owned)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        if key.is_empty()
            || key.len() > 128
            || !key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
        {
            return Err(Code::InvalidRequest);
        }
        if let Some(old) = i.operations.values().find(|op| op.idempotency_key == key) {
            return if old.action == action {
                Ok(old.clone())
            } else {
                Err(Code::InvalidRequest)
            };
        }
        if i.active.is_some()
            || (action == Action::Start
                && i.supervisor.as_ref().is_some_and(|supervisor| {
                    i.state != State::Running || !*supervisor.accepting_requests().borrow()
                }))
        {
            return Err(Code::Busy);
        }
        if action == Action::Start {
            let r = i.installation.as_ref().ok_or(Code::NotInstalled)?;
            if r.components_dir.is_none() || r.key_id.is_none() {
                return Err(Code::NotConfigured);
            }
        }
        let guard = self.coordinator_lock()?;
        let noop = (action == Action::Start && i.state == State::Running)
            || (action == Action::Stop
                && i.supervisor.is_none()
                && !self
                    .root
                    .join("data/state/managed-process.json")
                    .try_exists()
                    .map_err(|_| Code::PersistenceFailed)?);
        let op = Operation {
            operation_id: uuid::Uuid::new_v4().to_string(),
            action,
            phase: if noop {
                Phase::Completed
            } else {
                Phase::Queued
            },
            result: noop.then_some(Outcome::Succeeded),
            error_code: None,
            cancellable: !noop && action == Action::Start,
            idempotency_key: key,
            request_fingerprint: None,
            setup_code: None,
            download_progress: None,
            verification: None,
        };
        i.operations.insert(op.operation_id.clone(), op.clone());
        if let Err(e) = write_json(&self.root.join("operations.json"), &i.operations) {
            i.operations.remove(&op.operation_id);
            return Err(e);
        }
        // Commit the operation before the intent: a rejected start must never
        // leave an autostart intent for the next daemon. An interrupted queued
        // operation is cancelled by open().
        if let Some(r) = i.installation.as_mut() {
            let mut updated = r.clone();
            updated.desired_running = action == Action::Start;
            if let Err(error) = write_json(&self.root.join("installation.json"), &updated) {
                i.operations.remove(&op.operation_id);
                return Err(error);
            }
            *r = updated;
        }
        if noop {
            if action == Action::Stop {
                i.state = match &i.installation {
                    None => State::NotInstalled,
                    Some(r) if r.components_dir.is_none() || r.key_id.is_none() => {
                        State::NotConfigured
                    }
                    Some(_) => State::Stopped,
                };
                i.last_error = None;
            }
            return Ok(op);
        }
        let token = CancellationToken::new();
        i.active = Some((op.operation_id.clone(), token.clone()));
        let host = self.clone();
        let id = op.operation_id.clone();
        tokio::spawn(async move {
            let _guard = guard;
            let result = host.execute(action, &id, &token).await;
            host.finish(&id, action, result).await;
        });
        Ok(op)
    }
    async fn phase(&self, id: &str, phase: Phase) {
        let mut i = self.inner.lock().await;
        if let Some(op) = i.operations.get_mut(id) {
            op.phase = phase;
            if phase == Phase::RollingBack {
                op.cancellable = false;
            }
        }
        if write_json(&self.root.join("operations.json"), &i.operations).is_err() {
            tracing::warn!("cannot persist iDoris operation phase");
        }
    }
    fn block_gateway(&self) {
        if let Ok(gateway) = self.router.idoris_gateway() {
            *self.draining.lock().unwrap_or_else(|e| e.into_inner()) =
                Some(Arc::downgrade(&gateway));
        }
        self.router.block_idoris("managed iDoris not ready");
    }
    async fn prepare_gateway(
        &self,
        record: &Installation,
        cancel: &CancellationToken,
    ) -> Result<(ManagedConfig, IdorisGateway), Code> {
        let secrets = ClassifiedSecrets {
            source: self.secrets.as_ref(),
            failure: SyncMutex::new(None),
        };
        let config = ManagedConfig {
            url: format!("http://127.0.0.1:{}", record.data_port),
            key_id: record.key_id.clone().ok_or(Code::NotConfigured)?,
        };
        ManagedIdoris::prepare(&config, &secrets, cancel)
            .await
            .map(|gateway| (config, gateway))
            .map_err(|_| {
                if cancel.is_cancelled() {
                    Code::Cancelled
                } else {
                    secrets
                        .failure
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .unwrap_or(Code::StartFailed)
                }
            })
    }
    async fn connect_gateway(
        &self,
        record: &Installation,
        cancel: &CancellationToken,
    ) -> Result<(), Code> {
        let (config, gateway) = self.prepare_gateway(record, cancel).await?;
        if cancel.is_cancelled() {
            return Err(Code::Cancelled);
        }
        self.managed.persist_config(&config)?;
        self.router.replace_idoris(gateway);
        Ok(())
    }
    async fn execute(
        self: &Arc<Self>,
        action: Action,
        id: &str,
        cancel: &CancellationToken,
    ) -> Result<(), Code> {
        if action == Action::Stop {
            self.phase(id, Phase::Stopping).await;
            self.block_gateway();
            let (supervisor, record) = {
                let mut i = self.inner.lock().await;
                i.generation = None;
                (i.supervisor.take(), i.installation.clone())
            };
            if let Some(s) = supervisor {
                let report = s.stop().await.map_err(Code::from)?;
                if report.forced || report.drain_timed_out {
                    return Err(Code::StopTimeout);
                }
            } else {
                let record = record.ok_or(Code::InstanceConflict)?;
                Supervisor::recover_leftover(
                    DataPaths {
                        db: self.root.join("data/idoris.sqlite3"),
                        state: self.root.join("data/state"),
                    },
                    &record.release_dir.join("idoris"),
                )
                .await
                .map_err(Code::from)?;
            }
            return Ok(());
        }
        self.phase(id, Phase::Starting).await;
        let record = {
            let mut i = self.inner.lock().await;
            i.state = State::Starting;
            i.installation.clone().ok_or(Code::NotInstalled)?
        };
        let (supervisor, ready, refresh_failure) =
            self.start_record(&record, id, cancel, true, None).await?;
        {
            let mut i = self.inner.lock().await;
            i.supervisor = Some(supervisor);
            i.supervisor_release = Some(record.release_dir.clone());
            i.generation = Some(id.to_owned());
        }
        self.monitor(
            ready,
            record,
            id.to_owned(),
            cancel.clone(),
            refresh_failure,
        );
        Ok(())
    }
    async fn start_record(
        self: &Arc<Self>,
        record: &Installation,
        id: &str,
        cancel: &CancellationToken,
        publish: bool,
        override_inputs: Option<LaunchInputs>,
    ) -> Result<
        (
            Supervisor,
            watch::Receiver<bool>,
            Arc<SyncMutex<Option<Code>>>,
        ),
        Code,
    > {
        // Preparation is read-only; cancellation can drop it before any spawn.
        let (config, inputs) = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(Code::Cancelled),
            result = async {
                self.secrets.secret(record.key_id.as_deref().ok_or(Code::NotConfigured)?)
                    .await.map_err(secret_code)?;
                let config = self.runtime.config(&self.root, record, cancel.clone()).await?;
                let inputs = match override_inputs { Some(inputs) => inputs, None => self.runtime.inputs(record).await? };
                Ok::<_, Code>((config, inputs))
            } => result?,
        };
        let runtime = self.runtime.clone();
        let refresh_record = record.clone();
        let transient = if !publish && record.omlx_key_id.is_none() {
            inputs.omlx_key.clone()
        } else {
            None
        };
        let mut initial = Some(inputs);
        let refresh_failure = Arc::new(SyncMutex::new(None));
        let input_failure = refresh_failure.clone();
        let host = Arc::downgrade(self);
        let supervisor = Supervisor::start(
            config,
            move || {
                let first = initial.take();
                let runtime = runtime.clone();
                let r = refresh_record.clone();
                let failure = input_failure.clone();
                let transient = transient.clone();
                async move {
                    match first {
                        Some(inputs) => Ok(inputs),
                        None => {
                            let result = runtime.inputs(&r).await.map(|mut inputs| {
                                if transient.is_some() {
                                    inputs.omlx_key = transient;
                                }
                                inputs
                            });
                            result.map_err(|code| {
                                *failure.lock().unwrap_or_else(|e| e.into_inner()) = Some(code);
                                SupervisorError::Configuration
                            })
                        }
                    }
                }
            },
            move || async move {
                if let Some(host) = host.upgrade() {
                    host.block_gateway();
                    loop {
                        let pending = host
                            .draining
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .as_ref()
                            .is_some_and(|w| w.strong_count() > 0);
                        if !pending {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }
            },
        )
        .await
        .map_err(|error| {
            refresh_failure
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
                .unwrap_or_else(|| Code::from(error))
        })?;
        let ready = supervisor.accepting_requests();
        self.phase(id, Phase::Connecting).await;
        let connected = async {
            ReadySignal(ready.clone())
                .wait_ready(cancel)
                .await
                .map_err(|_| {
                    if cancel.is_cancelled() {
                        Code::Cancelled
                    } else {
                        Code::StartFailed
                    }
                })?;
            if publish {
                self.connect_gateway(record, cancel).await
            } else {
                Ok(())
            }
        }
        .await;
        if let Err(e) = connected {
            self.block_gateway();
            supervisor.stop().await.map_err(Code::from)?;
            return Err(e);
        }
        Ok((supervisor, ready, refresh_failure))
    }
    fn monitor(
        self: &Arc<Self>,
        mut signal: watch::Receiver<bool>,
        record: Installation,
        generation: String,
        cancel: CancellationToken,
        refresh_failure: Arc<SyncMutex<Option<Code>>>,
    ) {
        let host = Arc::downgrade(self);
        tokio::spawn(async move {
            loop {
                let changed = signal.changed().await;
                let Some(host) = host.upgrade() else {
                    return;
                };
                if changed.is_err() {
                    loop {
                        let notified = host.changed.notified();
                        let mut i = host.inner.lock().await;
                        if i.closing
                            || cancel.is_cancelled()
                            || i.generation.as_deref() != Some(&generation)
                        {
                            return;
                        }
                        if i.active.is_some() {
                            drop(i);
                            notified.await;
                            continue;
                        }
                        if let Some(supervisor) = i.supervisor.take() {
                            // The readiness sender has closed: the supervisor has
                            // already returned, so this join cannot wait for a stop.
                            let error = supervisor.wait().await.err().map(Code::from);
                            let error = refresh_failure
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .take()
                                .or(error)
                                .or(i.last_error);
                            host.block_gateway();
                            i.last_error = error;
                            i.state = match error {
                                Some(Code::InstanceConflict) => State::InstanceConflict,
                                Some(_) => State::Failed,
                                None => State::Stopped,
                            };
                        }
                        return;
                    }
                }
                if !*signal.borrow_and_update() {
                    let mut i = host.inner.lock().await;
                    if !i.closing
                        && !cancel.is_cancelled()
                        && i.generation.as_deref() == Some(&generation)
                        && i.supervisor.is_some()
                    {
                        host.block_gateway();
                        i.state = State::Starting;
                    }
                } else {
                    if cancel.is_cancelled() {
                        return;
                    }
                    let prepared = host.prepare_gateway(&record, &cancel).await;
                    let mut i = host.inner.lock().await;
                    if i.closing
                        || cancel.is_cancelled()
                        || i.generation.as_deref() != Some(&generation)
                        || i.supervisor.is_none()
                    {
                        return;
                    }
                    let result = prepared.and_then(|(config, gateway)| {
                        host.managed.persist_config(&config)?;
                        host.router.replace_idoris(gateway);
                        Ok(())
                    });
                    if result.is_ok() {
                        i.state = State::Running;
                    } else {
                        host.block_gateway();
                        i.last_error = result.err();
                        i.state = State::Failed;
                        if let Some(s) = &i.supervisor {
                            s.request_stop();
                        }
                    }
                }
            }
        });
    }
    async fn finish(&self, id: &str, action: Action, mut result: Result<(), Code>) {
        let mut i = self.inner.lock().await;
        if action == Action::Start
            && result.is_ok()
            && i.active
                .as_ref()
                .is_some_and(|(_, cancel)| cancel.is_cancelled())
        {
            let supervisor = i.supervisor.take();
            drop(i);
            self.block_gateway();
            result = match supervisor {
                Some(supervisor) => supervisor
                    .stop()
                    .await
                    .map_err(Code::from)
                    .and(Err(Code::Cancelled)),
                None => Err(Code::Cancelled),
            };
            i = self.inner.lock().await;
        }
        i.last_error = result.err();
        i.state = match result {
            Ok(())
                if matches!(action, Action::Start | Action::Connect) || i.supervisor.is_some() =>
            {
                if i.supervisor
                    .as_ref()
                    .is_some_and(|s| *s.accepting_requests().borrow())
                {
                    State::Running
                } else {
                    State::Starting
                }
            }
            Ok(()) => match &i.installation {
                None => State::NotInstalled,
                Some(r) if r.key_id.is_none() || r.components_dir.is_none() => State::NotConfigured,
                _ => State::Stopped,
            },
            Err(_)
                if matches!(action, Action::Install | Action::Connect)
                    && i.supervisor
                        .as_ref()
                        .is_some_and(|s| *s.accepting_requests().borrow()) =>
            {
                State::Running
            }
            Err(Code::InstanceConflict) => State::InstanceConflict,
            Err(Code::Cancelled) => match &i.installation {
                None => State::NotInstalled,
                Some(r) if r.key_id.is_none() || r.components_dir.is_none() => State::NotConfigured,
                _ => State::Stopped,
            },
            Err(_) => State::Failed,
        };
        // After every operation: if a migrated-away release is no longer in use, publish it.
        self.finalize_legacy_marks(&i);
        if let Some(op) = i.operations.get_mut(id) {
            op.phase = if matches!(action, Action::Install | Action::Connect) && result.is_ok() {
                Phase::Done
            } else {
                Phase::Completed
            };
            op.cancellable = false;
            op.error_code = result.err();
            op.result = Some(match result {
                Ok(()) => Outcome::Succeeded,
                Err(Code::Cancelled) => Outcome::Cancelled,
                Err(_) => Outcome::Failed,
            });
        }
        if write_json(&self.root.join("operations.json"), &i.operations).is_err() {
            if let Some(op) = i.operations.get_mut(id) {
                op.result = Some(Outcome::Failed);
                op.error_code = Some(Code::PersistenceFailed);
            }
            i.last_error = Some(Code::PersistenceFailed);
            i.state = State::Failed;
        }
        i.active = None;
        self.changed.notify_waiters();
    }
    pub async fn cancel(&self, id: &str) -> Result<Operation, Code> {
        let mut i = self.inner.lock().await;
        let op = i.operations.get(id).cloned().ok_or(Code::NotFound)?;
        if op.result.is_some() {
            return Ok(op);
        }
        if !op.cancellable {
            return Err(Code::NotCancellable);
        }
        if op.action == Action::Start
            && let Some(record) = i.installation.as_mut()
        {
            let mut updated = record.clone();
            updated.desired_running = false;
            write_json(&self.root.join("installation.json"), &updated)?;
            *record = updated;
        }
        if let Some((active, token)) = &i.active
            && active == id
        {
            token.cancel();
        }
        Ok(op)
    }
    /// Register before restore so daemon cancellation owns every spawned generation.
    pub fn stop_on_shutdown(
        self: &Arc<Self>,
        cancel: CancellationToken,
    ) -> tokio::task::JoinHandle<Result<(), Code>> {
        let host = self.clone();
        tokio::spawn(async move {
            cancel.cancelled().await;
            host.shutdown().await
        })
    }
    /// Close admission, cancel an in-flight start, then wait for reaping.
    pub async fn shutdown(&self) -> Result<(), Code> {
        loop {
            let notified = self.changed.notified();
            let mut i = self.inner.lock().await;
            i.closing = true;
            if let Some((_, token)) = &i.active {
                token.cancel();
                drop(i);
                notified.await;
                continue;
            }
            let supervisor = i.supervisor.take();
            drop(i);
            self.block_gateway();
            if let Some(supervisor) = supervisor {
                let report = supervisor.stop().await.map_err(Code::from)?;
                if report.forced || report.drain_timed_out {
                    return Err(Code::StopTimeout);
                }
            }
            let mut i = self.inner.lock().await;
            if i.installation.is_some() {
                i.state = State::Stopped;
            }
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests;
