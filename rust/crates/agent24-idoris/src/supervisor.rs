//! Managed iDoris lifecycle. Setup, key storage and budget discovery stay injected.
//!
//! v0.5.0 (`01484547`) reads exactly 64 lowercase hex bytes plus LF from stdin,
//! uses IDORIS_OMLX_API_KEY for serve, and exposes contract_version in /health.
//! No readiness or error classification is inferred from diagnostic output.

mod discovery;
mod identity;
mod logs;
mod register;
#[cfg(test)]
mod tests;

use crate::{IdorisLock, install::InstalledVersion};
use identity::{Lease, ProcessRecord};
use nix::{
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
use serde::Deserialize;
use std::{
    collections::VecDeque,
    fs::{self, File},
    future::Future,
    io::{self, Read},
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::AsyncWriteExt,
    process::{Child, Command},
    sync::watch,
    task::JoinHandle,
    time::{Instant, sleep, timeout},
};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

#[derive(Debug, thiserror::Error)]
pub enum SupervisorError {
    #[error("the same database already has an instance, or process identity cannot be verified")]
    InstanceConflict,
    #[error("iDoris exited unsuccessfully ({code:?})")]
    Failed { code: Option<i32> },
    #[error("iDoris discovery belongs to an earlier generation")]
    StaleDiscovery,
    #[error("iDoris authentication failed")]
    Authentication,
    #[error("invalid launcher configuration")]
    Configuration,
    #[error("iDoris budget admission failed")]
    Budget,
    #[error("iDoris health or contract validation failed")]
    Contract,
    #[error("iDoris did not become ready before the deadline")]
    StartupTimeout,
    #[error("Admin token stdin handoff failed")]
    TokenHandoff,
    #[error("iDoris shutdown was requested")]
    Stopped,
    #[error("iDoris supervisor I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("installed iDoris verification failed: {0}")]
    Installation(#[from] crate::install::InstallError),
}

/// IL-05 returns this after setup; canonicalization rejects symlink escapes.
#[derive(Clone, Debug)]
pub struct ComponentsDir(PathBuf);
impl ComponentsDir {
    pub fn new(managed_root: &Path, directory: &Path) -> Result<Self, SupervisorError> {
        if !managed_root.is_absolute() || !directory.is_absolute() {
            return Err(SupervisorError::Configuration);
        }
        let root = fs::canonicalize(managed_root)?;
        let directory = fs::canonicalize(directory)?;
        if !directory.is_dir() || !directory.starts_with(root) {
            return Err(SupervisorError::Configuration);
        }
        Ok(Self(directory))
    }
    pub fn path(&self) -> &Path {
        &self.0
    }
}

#[derive(Clone, Debug)]
pub struct DataPaths {
    pub db: PathBuf,
    pub state: PathBuf,
}
impl DataPaths {
    fn canonical(&self) -> Result<Self, SupervisorError> {
        if !self.db.is_absolute() || !self.state.is_absolute() {
            return Err(SupervisorError::Configuration);
        }
        fs::create_dir_all(&self.state)?;
        let parent = self.db.parent().ok_or(SupervisorError::Configuration)?;
        fs::create_dir_all(parent)?;
        let db = if self.db.exists() {
            fs::canonicalize(&self.db)?
        } else {
            fs::canonicalize(parent)?
                .join(self.db.file_name().ok_or(SupervisorError::Configuration)?)
        };
        Ok(Self {
            db,
            state: fs::canonicalize(&self.state)?,
        })
    }
}

/// Already computed by Agent24's budget owner. Zero is a real allocation.
#[derive(Clone, Copy, Debug)]
pub struct Budget {
    pub allocated_bytes: u64,
}

/// Never derive Debug: these values must not enter logs or task diagnostics.
pub struct LaunchInputs {
    pub budget: Budget,
    pub omlx_key: Option<Zeroizing<String>>,
}

#[derive(Clone, Copy, Debug)]
pub struct Ports {
    pub data: u16,
    pub admin: u16,
}
impl Default for Ports {
    fn default() -> Self {
        Self {
            data: 8740,
            admin: 8741,
        }
    }
}

/// Production defaults implement design §6 and the frozen §13.1 response.
#[derive(Clone, Debug)]
pub struct Timings {
    pub half_open: Duration,
    pub liveness: Duration,
    pub token_write: Duration,
    pub health_request: Duration,
    pub ready: Duration,
    pub poll: Duration,
    pub terminate: Duration,
    pub drain: Duration,
    pub restart_window: Duration,
    pub restart_delays: [Duration; 3],
}
impl Default for Timings {
    fn default() -> Self {
        Self {
            half_open: Duration::from_secs(60),
            liveness: Duration::from_secs(5),
            token_write: Duration::from_secs(10),
            health_request: Duration::from_secs(2),
            ready: Duration::from_secs(30),
            poll: Duration::from_millis(250),
            terminate: Duration::from_secs(17),
            drain: Duration::from_secs(30),
            restart_window: Duration::from_secs(300),
            restart_delays: [
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
            ],
        }
    }
}

pub struct StartConfig {
    pub installed: InstalledVersion,
    pub components: ComponentsDir,
    pub data: DataPaths,
    pub ports: Ports,
    pub log_dir: PathBuf,
    /// Opt-in sanitized child output; default logs only controlled phases.
    pub diagnostic_output: bool,
    pub timings: Timings,
    /// Cancel even while start, input refresh or restart backoff is pending.
    pub shutdown: CancellationToken,
    pub lock: &'static IdorisLock,
    registration: Option<register::Handle>,
    register_socket: Option<PathBuf>,
}
impl StartConfig {
    pub fn new(
        installed: InstalledVersion,
        components: ComponentsDir,
        data: DataPaths,
    ) -> Result<Self, SupervisorError> {
        let home = std::env::var_os("HOME").ok_or(SupervisorError::Configuration)?;
        Ok(Self {
            installed,
            components,
            data,
            ports: Ports::default(),
            log_dir: PathBuf::from(home).join("Library/Logs/Agent24/idoris"),
            diagnostic_output: false,
            timings: Timings::default(),
            shutdown: CancellationToken::new(),
            lock: IdorisLock::embedded(),
            registration: None,
            register_socket: None,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StopReport {
    pub forced: bool,
    pub drain_timed_out: bool,
}

/// A single owner holds the process, lease, credentials and restart task.
/// Dropping the handle requests shutdown; explicit stop also awaits reaping.
/// The gateway must subscribe to accepting_requests before serving traffic.
pub struct Supervisor {
    #[cfg(test)]
    initial_admin: Arc<Zeroizing<String>>,
    status: watch::Receiver<RuntimeStatus>,
    accepting: watch::Receiver<bool>,
    shutdown: CancellationToken,
    task: Option<JoinHandle<Result<StopReport, SupervisorError>>>,
}

#[derive(Clone, Debug, Default)]
pub struct RuntimeStatus {
    pub registered: bool,
    pub instance_id: Option<String>,
    pub ports: Option<Ports>,
    pub circuit_open: bool,
    pub crashes: u64,
    pub hangs: u64,
}

impl Supervisor {
    /// Reap a recorded orphan without launching a replacement. The lease and
    /// kernel PID/start-time/executable/UID checks are shared with start().
    pub async fn recover_leftover(
        data: DataPaths,
        executable: &Path,
    ) -> Result<(), SupervisorError> {
        let data = data.canonical()?;
        let lease = Lease::acquire(&data.state)?;
        lease
            .recover(&Timings::default(), &CancellationToken::new(), executable)
            .await
    }
    pub async fn start<F, Fut, D, Drain>(
        mut config: StartConfig,
        mut refresh: F,
        drain: D,
    ) -> Result<Self, SupervisorError>
    where
        F: FnMut() -> Fut + Send + 'static,
        Fut: Future<Output = Result<LaunchInputs, SupervisorError>> + Send,
        D: FnOnce() -> Drain + Send + 'static,
        Drain: Future<Output = ()> + Send,
    {
        if config.ports.data == 0
            || config.ports.admin == 0
            || config.ports.data == config.ports.admin
        {
            return Err(SupervisorError::Configuration);
        }
        config.data = config.data.canonical()?;
        let lease = Lease::acquire(&config.data.state)?;
        let registration = if launcher_enabled(&config) {
            match discovery::path(&config.data).and_then(|path| {
                register::Server::start(path, config.lock).map_err(SupervisorError::Io)
            }) {
                Ok(server) => {
                    config.register_socket = Some(server.socket_path().to_path_buf());
                    config.registration = Some(server.handle());
                    Some(server)
                }
                Err(error) => {
                    tracing::warn!(%error, "iDoris registration unavailable");
                    None
                }
            }
        } else {
            None
        };
        let config = Arc::new(config);
        let adopted = if registration.is_some() {
            try_adopt(&config, &lease, &mut refresh).await?
        } else {
            None
        };
        if adopted.is_none() {
            lease
                .recover(
                    &config.timings,
                    &config.shutdown,
                    &config.installed.binary(),
                )
                .await?;
        }
        let mut policy = RestartPolicy {
            open: false,
            attempts: VecDeque::new(),
        };
        let process = if let Some(process) = adopted {
            process
        } else {
            launch(
                &config,
                &lease,
                &mut refresh,
                &mut policy,
                Instant::now(),
                None,
            )
            .await?
        };
        let (accepting, receiver) = watch::channel(true);
        let (status, status_receiver) = watch::channel(RuntimeStatus {
            ports: Some(process.ports),
            registered: config
                .registration
                .as_ref()
                .and_then(|r| r.session())
                .is_some_and(|r| Some(&r.instance_id) == process.instance_id.as_ref()),
            instance_id: process.instance_id.clone(),
            ..RuntimeStatus::default()
        });
        let shutdown = config.shutdown.clone();
        #[cfg(test)]
        let initial_admin = process._admin.clone();
        let task = tokio::spawn(async move {
            let _registration = registration;
            supervise(
                process,
                config,
                lease,
                refresh,
                drain,
                Signals { accepting, status },
                policy,
            )
            .await
        });
        Ok(Self {
            #[cfg(test)]
            initial_admin,
            accepting: receiver,
            status: status_receiver,
            shutdown,
            task: Some(task),
        })
    }

    pub fn runtime_status(&self) -> watch::Receiver<RuntimeStatus> {
        self.status.clone()
    }
    pub fn accepting_requests(&self) -> watch::Receiver<bool> {
        self.accepting.clone()
    }
    pub fn request_stop(&self) {
        self.shutdown.cancel();
    }
    pub async fn stop(mut self) -> Result<StopReport, SupervisorError> {
        self.shutdown.cancel();
        self.join().await
    }
    /// Await a terminal failure (or a stop requested via the supplied token).
    pub async fn wait(mut self) -> Result<StopReport, SupervisorError> {
        self.join().await
    }
    async fn join(&mut self) -> Result<StopReport, SupervisorError> {
        let task = self.task.take().ok_or(SupervisorError::Stopped)?;
        task.await
            .map_err(|_| io::Error::other("supervisor task failed"))?
    }
}
impl Drop for Supervisor {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

struct RestartPolicy {
    open: bool,
    attempts: VecDeque<Instant>,
}
impl RestartPolicy {
    fn delay(
        &mut self,
        now: Instant,
        stable_since: Instant,
        timings: &Timings,
    ) -> Result<Duration, SupervisorError> {
        if !self.open && now.duration_since(stable_since) >= timings.restart_window {
            self.attempts.clear();
        }
        while self
            .attempts
            .front()
            .is_some_and(|at| now.duration_since(*at) >= timings.restart_window)
        {
            self.attempts.pop_front();
        }
        let failures = self.attempts.len();
        self.attempts.push_back(now);
        self.open |= failures >= 5;
        if self.open {
            return Ok(timings.half_open);
        }
        let base = timings.restart_delays[0].saturating_mul(1 << failures);
        Ok(base.min(Duration::from_secs(30)))
    }
}

struct Signals {
    accepting: watch::Sender<bool>,
    status: watch::Sender<RuntimeStatus>,
}

async fn supervise<F, Fut, D, Drain>(
    mut process: ManagedProcess,
    config: Arc<StartConfig>,
    lease: Lease,
    mut refresh: F,
    drain: D,
    signals: Signals,
    mut policy: RestartPolicy,
) -> Result<StopReport, SupervisorError>
where
    F: FnMut() -> Fut + Send,
    Fut: Future<Output = Result<LaunchInputs, SupervisorError>> + Send,
    D: FnOnce() -> Drain + Send,
    Drain: Future<Output = ()> + Send,
{
    let Signals {
        accepting,
        status: status_signal,
    } = &signals;
    let mut stable_since = Instant::now();
    let mut probes = tokio::time::interval(config.timings.liveness);
    probes.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    probes.tick().await;
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(config.timings.health_request)
        .build()
        .map_err(|_| SupervisorError::Configuration)?;
    let mut failures = 0;
    let mut registration_status = config.registration.as_ref().map(|r| r.subscribe());
    if let Some(receiver) = &mut registration_status {
        receiver.mark_changed();
    }
    loop {
        let status = tokio::select! {
            biased;
            _ = config.shutdown.cancelled() => {
                accepting.send_replace(false);
                let drain_timed_out = timeout(config.timings.drain, drain()).await.is_err();
                let forced = process.stop().await?;
                status_signal.send_modify(|s| s.registered = false);
                lease.clear()?;
                return Ok(StopReport { forced, drain_timed_out });
            }
            changed = async {
                match &mut registration_status {
                    Some(receiver) => receiver.changed().await,
                    None => std::future::pending().await,
                }
            } => {
                if changed.is_err() { registration_status = None; }
                status_signal.send_modify(|s| {
                    let session = config.registration.as_ref().and_then(|r| r.session());
                    s.registered = session.as_ref().is_some_and(|r| Some(&r.instance_id) == process.instance_id.as_ref());
                    s.instance_id = process.instance_id.clone();
                });
                continue;
            },
            status = process.child.wait() => match status {
                Ok(status) => Some(status),
                Err(error) => {
                    accepting.send_replace(false);
                    return Err(error.into());
                }
            },
            _ = probes.tick() => {
                let url = format!("http://127.0.0.1:{}/health", process.ports.data);
                let alive = tokio::select! {
                    _ = config.shutdown.cancelled() => continue,
                    result = async {
                        let mut response = client.get(url).send().await.ok()?;
                        if !response.status().is_success() { return None; }
                        let mut body = Vec::new();
                        while let Some(chunk) = response.chunk().await.ok()? {
                            if body.len() + chunk.len() > 16384 { return None; }
                            body.extend_from_slice(&chunk);
                        }
                        validate_health(&body, config.lock).ok()?;
                        let health: Health = serde_json::from_slice(&body).ok()?;
                        if process.instance_id.as_ref().is_some_and(|id| *id != health.instance_id) { return None; }
                        Some(())
                    } => result.is_some(),
                };
                failures = if alive { 0 } else { failures + 1 };
                if failures < 3 { continue; }
                accepting.send_replace(false);
                status_signal.send_modify(|s| { s.ports = None; s.hangs += 1; });
                process.output.event("hung: three failed health probes");
                process.stop().await?;
                None
            },
        };
        accepting.send_replace(false);
        status_signal.send_modify(|s| {
            s.ports = None;
            s.registered = false;
        });
        // Snapshot surviving workers at the reap itself, before the PGID
        // could be recycled; finish() then kills them with the group.
        if let Some(status) = status {
            process.adopt_survivors()?;
            process
                .output
                .event(&format!("exited code={:?}", status.code()));
            process.finish().await?;
            let expected = process.instance_id.as_deref().is_some_and(|id| {
                config
                    .registration
                    .as_ref()
                    .is_some_and(|r| r.take_expected_exit(id))
            });
            if expected_exit(status.code(), expected) {
                lease.clear()?;
                return Ok(StopReport::default());
            }
            if !retry_exit(status.code(), launcher_enabled(&config)) {
                lease.clear()?;
                return Err(exit_error(status));
            }
            status_signal.send_modify(|s| s.crashes += 1);
        }
        lease.clear()?;
        if config.shutdown.is_cancelled() {
            return Ok(StopReport::default());
        }
        let delay = policy.delay(Instant::now(), stable_since, &config.timings)?;
        status_signal.send_modify(|s| s.circuit_open = policy.open);
        accepting.send_replace(false);
        tokio::select! {
            biased;
            _ = config.shutdown.cancelled() => return Ok(StopReport::default()),
            _ = sleep(restart_backoff(delay, &config.timings)) => {},
        }
        // Retry crashes with a fresh generation; orphan expiry stops normally.
        process = match launch(
            &config,
            &lease,
            &mut refresh,
            &mut policy,
            Instant::now(),
            Some(&signals),
        )
        .await
        {
            Err(SupervisorError::Stopped) => return Ok(StopReport::default()),
            other => other?,
        };
        stable_since = Instant::now();
        policy.open = false;
        failures = 0;
        status_signal.send_modify(|s| {
            s.ports = Some(process.ports);
            s.instance_id = process.instance_id.clone();
            s.registered = config
                .registration
                .as_ref()
                .and_then(|r| r.session())
                .is_some_and(|r| Some(&r.instance_id) == process.instance_id.as_ref());
            s.circuit_open = false;
        });
        accepting.send_replace(true);
    }
}

fn restart_backoff(delay: Duration, timings: &Timings) -> Duration {
    if delay == timings.half_open {
        return delay;
    }
    let jitter = Duration::from_millis(u64::from(uuid::Uuid::new_v4().as_bytes()[0]) % 101);
    (delay + jitter).min(Duration::from_secs(30))
}

fn expected_exit(code: Option<i32>, bye: bool) -> bool {
    // Exit 5 still counts as an engine crash after graceful bye (§2.7.5).
    matches!(code, Some(0 | 4)) || (bye && code != Some(5))
}

fn retry_exit(code: Option<i32>, launcher: bool) -> bool {
    matches!(code, None | Some(5)) || (launcher && code == Some(1))
}

async fn launch<F, Fut>(
    config: &Arc<StartConfig>,
    lease: &Lease,
    refresh: &mut F,
    policy: &mut RestartPolicy,
    stable_since: Instant,
    signals: Option<&Signals>,
) -> Result<ManagedProcess, SupervisorError>
where
    F: FnMut() -> Fut + Send,
    Fut: Future<Output = Result<LaunchInputs, SupervisorError>> + Send,
{
    loop {
        let inputs = tokio::select! {
            biased;
            _ = config.shutdown.cancelled() => return Err(SupervisorError::Stopped),
            value = refresh() => value?,
        };
        match ManagedProcess::start(config.clone(), inputs, lease).await {
            Err(SupervisorError::Failed { code })
                if retry_exit(code, launcher_enabled(config))
                    && !config.shutdown.is_cancelled() =>
            {
                let delay = policy.delay(Instant::now(), stable_since, &config.timings)?;
                if let Some(signals) = signals {
                    signals.status.send_modify(|s| {
                        s.crashes += 1;
                        s.circuit_open = policy.open;
                    });
                    signals.accepting.send_replace(false);
                }
                tokio::select! {
                    biased;
                    _ = config.shutdown.cancelled() => return Err(SupervisorError::Stopped),
                    _ = sleep(restart_backoff(delay, &config.timings)) => {},
                }
            }
            Err(SupervisorError::Failed { code: Some(4) }) => return Err(SupervisorError::Stopped),
            _ if config.shutdown.is_cancelled() => return Err(SupervisorError::Stopped),
            other => return other,
        }
    }
}

fn exit_error(status: ExitStatus) -> SupervisorError {
    if status.code() == Some(3) {
        SupervisorError::InstanceConflict
    } else {
        SupervisorError::Failed {
            code: status.code(),
        }
    }
}

enum ProcessHandle {
    Spawned(Child),
    Orphan { record: ProcessRecord, exited: bool },
}
impl ProcessHandle {
    fn id(&self) -> Option<u32> {
        match self {
            Self::Spawned(child) => child.id(),
            Self::Orphan { record, .. } => Some(record.pid),
        }
    }
    fn take_stdin(&mut self) -> Option<tokio::process::ChildStdin> {
        match self {
            Self::Spawned(child) => child.stdin.take(),
            Self::Orphan { .. } => None,
        }
    }
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        use std::os::unix::process::ExitStatusExt;
        match self {
            Self::Spawned(child) => child.try_wait(),
            Self::Orphan { record, exited } => {
                if !*exited {
                    *exited = !record.alive().map_err(io::Error::other)?;
                }
                // An orphan is not our child, so its exit status cannot be reaped.
                Ok(exited.then(|| ExitStatus::from_raw(libc::SIGKILL)))
            }
        }
    }
    async fn wait(&mut self) -> io::Result<ExitStatus> {
        match self {
            Self::Spawned(child) => child.wait().await,
            orphan => loop {
                if let Some(status) = orphan.try_wait()? {
                    return Ok(status);
                }
                sleep(Duration::from_millis(100)).await;
            },
        }
    }
    #[cfg(test)]
    fn start_kill(&mut self) -> io::Result<()> {
        match self {
            Self::Spawned(child) => child.start_kill(),
            Self::Orphan { .. } => Err(io::Error::other("not a child")),
        }
    }
}

async fn try_adopt<F, Fut>(
    config: &Arc<StartConfig>,
    lease: &Lease,
    refresh: &mut F,
) -> Result<Option<ManagedProcess>, SupervisorError>
where
    F: FnMut() -> Fut + Send,
    Fut: Future<Output = Result<LaunchInputs, SupervisorError>> + Send,
{
    let path = discovery::path(&config.data)?;
    let Ok(discovered) = discovery::read_any(&path) else {
        return Ok(None);
    };
    let Some(record) = lease.adoptable(&config.installed.binary(), discovered.pid) else {
        return Ok(None);
    };
    #[cfg(target_os = "macos")]
    if (record.start_time / 1000).abs_diff(discovered.started_at_ms) > 5000 {
        return Ok(None);
    }
    let inputs = tokio::select! {
        _ = config.shutdown.cancelled() => return Err(SupervisorError::Stopped),
        value = refresh() => value?,
    };
    if let Some(launch) = &discovered.launch
        && (launch["db_path"].as_str() != config.data.db.to_str()
            || launch["state_dir"].as_str() != config.data.state.to_str()
            || launch["components_dir"].as_str() != config.components.path().to_str()
            || launch["allocated_budget_bytes"]
                .as_u64()
                .is_some_and(|n| n != inputs.budget.allocated_bytes))
    {
        return Ok(None);
    }
    let Some(registration) = &config.registration else {
        return Ok(None);
    };
    lease.write(&record)?;
    let mut updates = registration.subscribe();
    let adopted = async {
        let registered = timeout(Duration::from_secs(20), async {
            loop {
                if registration.session().is_some_and(|s| {
                    s.instance_id == discovered.instance_id && s.pid == discovered.pid
                }) {
                    break;
                }
                if updates.changed().await.is_err() {
                    return false;
                }
            }
            true
        })
        .await
        .unwrap_or(false);
        if !registered {
            return Ok(None);
        }
        let token = mint_token()?;
        if registration
            .set_admin_token(&discovered.instance_id, token.clone())
            .await
            .is_err()
        {
            return Ok(None);
        }
        let token = Arc::new(token);
        let output = logs::Output::new(
            &config.log_dir,
            token.clone(),
            inputs.omlx_key.map(Arc::new),
            config.diagnostic_output,
        )?;
        let mut process = ManagedProcess {
            ports: Ports {
                data: discovered.data_port,
                admin: discovered.admin_port,
            },
            discovery: Some(path),
            instance_id: Some(discovered.instance_id),
            child: ProcessHandle::Orphan {
                record,
                exited: false,
            },
            group: Some(Pid::from_raw(discovered.pid as i32)),
            members: Vec::new(),
            adopted: false,
            config: config.clone(),
            output,
            _admin: token,
        };
        if process
            .ready(Instant::now() + config.timings.ready)
            .await
            .is_err()
        {
            process.output.finish().await;
            return Ok(None);
        }
        process.remember_members()?;
        process.output.event("adopted");
        Ok::<_, SupervisorError>(Some(process))
    };
    tokio::select! {
        _ = config.shutdown.cancelled() => Err(SupervisorError::Stopped),
        result = adopted => result,
    }
}

struct ManagedProcess {
    ports: Ports,
    discovery: Option<PathBuf>,
    instance_id: Option<String>,
    child: ProcessHandle,
    group: Option<Pid>,
    members: Vec<ProcessRecord>,
    // Set once the leader's reap was observed and its survivors snapshotted.
    adopted: bool,
    config: Arc<StartConfig>,
    output: logs::Output,
    // Retained only in zeroizing memory for this generation and log redaction.
    _admin: Arc<Zeroizing<String>>,
}
impl ManagedProcess {
    async fn start(
        config: Arc<StartConfig>,
        inputs: LaunchInputs,
        lease: &Lease,
    ) -> Result<Self, SupervisorError> {
        if inputs.omlx_key.as_ref().is_some_and(|key| {
            key.is_empty() || key.chars().any(|ch| ch.is_whitespace() || ch.is_control())
        }) {
            return Err(SupervisorError::Configuration);
        }
        let token = Arc::new(mint_token()?);
        let mut output = logs::Output::new(
            &config.log_dir,
            token.clone(),
            inputs.omlx_key.map(Arc::new),
            config.diagnostic_output,
        )?;
        let mut command = serve_command(&config, &inputs.budget, output.omlx_key())?;
        let discovery = if launcher_enabled(&config) {
            Some(discovery::path(&config.data)?)
        } else {
            None
        };
        let mut child = command.spawn()?;
        let spawned = Instant::now();
        let pid = child
            .id()
            .ok_or_else(|| io::Error::other("missing child PID"))?;
        output.attach(child.stdout.take(), child.stderr.take());
        let mut process = Self {
            ports: config.ports,
            discovery,
            instance_id: None,
            child: ProcessHandle::Spawned(child),
            group: Some(Pid::from_raw(pid as i32)),
            members: Vec::new(),
            adopted: false,
            config,
            output,
            _admin: token,
        };
        let result = async {
            let mut stdin = process
                .child
                .take_stdin()
                .ok_or(SupervisorError::TokenHandoff)?;
            let frame = match &process.discovery {
                Some(path) => discovery::frame(
                    &process._admin,
                    path,
                    process.config.register_socket.as_deref(),
                )?,
                None => Zeroizing::new(process._admin.to_string()),
            };
            let handoff = write_token(
                &mut stdin,
                &frame,
                spawned + process.config.timings.token_write,
                &process.config.shutdown,
            )
            .await;
            if let Err(error) = handoff {
                if matches!(error, SupervisorError::TokenHandoff)
                    && let Ok(status) =
                        timeout(Duration::from_millis(50), process.child.wait()).await
                {
                    let status = status?;
                    process.adopt_survivors()?;
                    return Err(exit_error(status));
                }
                return Err(error);
            }
            drop(stdin);
            // Capture kernel identity, not argv or a PID-only marker. The
            // durable record is written after the immediate token handoff.
            let record = match ProcessRecord::capture(pid) {
                Ok(record) => record,
                Err(error) => {
                    // A fast failure may disappear before the kernel query;
                    // preserve the contractual exit code, particularly L4.
                    if let Ok(status) =
                        timeout(Duration::from_millis(50), process.child.wait()).await
                    {
                        let status = status?;
                        process.adopt_survivors()?;
                        return Err(exit_error(status));
                    }
                    return Err(error.into());
                }
            };
            lease.write(&record)?;
            process
                .ready(spawned + process.config.timings.ready)
                .await?;
            process.remember_members()?;
            Ok(())
        }
        .await;
        if let Err(error) = result {
            process.output.event(match &error {
                SupervisorError::InstanceConflict => "failed: instance conflict",
                SupervisorError::Authentication => "failed: authentication",
                SupervisorError::Contract => "failed: health contract",
                SupervisorError::TokenHandoff => "failed: token handoff",
                SupervisorError::StartupTimeout => "failed: readiness timeout",
                SupervisorError::Stopped => "shutdown requested",
                _ => "failed: startup",
            });
            // Always reap before returning a failed start. Cleanup failure
            // preserves the record, allowing a later recovery to fail closed.
            process.stop().await?;
            lease.clear()?;
            return Err(error);
        }
        process.output.event("ready");
        Ok(process)
    }

    fn remember_members(&mut self) -> io::Result<()> {
        if self.child.try_wait()?.is_some() {
            return self.adopt_survivors();
        }
        if let Some(group) = self.group {
            self.members = ProcessRecord::group_members(group)?;
        }
        Ok(())
    }

    /// Call right after any wait observes the leader's exit. tokio reaps only
    /// inside our own wait/try_wait calls, so the first observation is the
    /// reap. A PID that still names a process group is never reallocated, so
    /// members found now are this generation's workers (forked after ready,
    /// hence missing from the readiness snapshot). Later calls add nothing:
    /// once the group empties, its PGID may name an unrelated group.
    fn adopt_survivors(&mut self) -> io::Result<()> {
        if self.adopted {
            return Ok(());
        }
        self.adopted = true;
        if let Some(group) = self.group {
            for member in ProcessRecord::group_members(group)? {
                if !self.members.contains(&member) {
                    self.members.push(member);
                }
            }
        }
        Ok(())
    }

    /// Returns whether the signal was sent to a group proven to be ours.
    fn signal_owned_group(&mut self, signal: Signal) -> io::Result<bool> {
        let exited = self.child.try_wait()?.is_some();
        if exited {
            self.adopt_survivors()?;
        }
        if let Some(group) = self.group {
            // Once reaped, the numeric PGID alone proves no ownership. Only an
            // observed member with unchanged kernel identity can authorize
            // group cleanup. Unknown descendants are left alone.
            if !exited || self.members.iter().any(|member| member.in_group(group)) {
                signal_group(group, signal)?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn ready(&mut self, deadline: Instant) -> Result<(), SupervisorError> {
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(self.config.timings.health_request)
            .build()
            .map_err(|_| SupervisorError::Configuration)?;
        loop {
            if let Some(path) = &self.discovery {
                match discovery::read_any(path) {
                    Ok(record) if Some(record.pid) != self.child.id() => {}
                    Ok(record) => {
                        self.ports = Ports {
                            data: record.data_port,
                            admin: record.admin_port,
                        };
                        self.instance_id = Some(record.instance_id);
                    }
                    Err(SupervisorError::StaleDiscovery) => {}
                    Err(SupervisorError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            }
            let url = format!("http://127.0.0.1:{}/health", self.ports.data);
            let check = async {
                if self.discovery.is_some() && self.instance_id.is_none() {
                    return Ok(false);
                }
                if let Ok(mut response) = client.get(&url).send().await {
                    if matches!(response.status().as_u16(), 401 | 403) {
                        return Err(SupervisorError::Authentication);
                    }
                    if response.status().is_success() {
                        let mut body = Vec::new();
                        while let Some(chunk) = response
                            .chunk()
                            .await
                            .map_err(|_| SupervisorError::Contract)?
                        {
                            if body.len() + chunk.len() > 16 * 1024 {
                                return Err(SupervisorError::Contract);
                            }
                            body.extend_from_slice(&chunk);
                        }
                        validate_health(&body, self.config.lock)?;
                        let health: Health =
                            serde_json::from_slice(&body).map_err(|_| SupervisorError::Contract)?;
                        if self
                            .instance_id
                            .as_ref()
                            .is_some_and(|id| *id != health.instance_id)
                        {
                            return Err(SupervisorError::Contract);
                        }
                        if !listeners_owned(&self.config, self.ports, self.group).await? {
                            return Ok(false);
                        }
                        let admin_url =
                            format!("http://127.0.0.1:{}/admin/api/v1/status", self.ports.admin);
                        if let Ok(response) = client
                            .get(admin_url)
                            .bearer_auth(self._admin.as_str())
                            .send()
                            .await
                        {
                            return match response.status().as_u16() {
                                200 => Ok(true),
                                401 | 403 => Err(SupervisorError::Authentication),
                                _ => Err(SupervisorError::Contract),
                            };
                        }
                        return Ok(false);
                    }
                    if response.status().is_redirection() || response.status().is_client_error() {
                        return Err(SupervisorError::Contract);
                    }
                }
                Ok(false)
            };
            let ready = tokio::select! {
                biased;
                _ = self.config.shutdown.cancelled() => return Err(SupervisorError::Stopped),
                status = self.child.wait() => Err(status?),
                _ = tokio::time::sleep_until(deadline) => return Err(SupervisorError::StartupTimeout),
                value = check => Ok(value?),
            };
            let ready = match ready {
                Ok(ready) => ready,
                Err(status) => {
                    self.adopt_survivors()?;
                    return Err(exit_error(status));
                }
            };
            if ready {
                if let Some(status) = self.child.try_wait()? {
                    self.adopt_survivors()?;
                    return Err(exit_error(status));
                }
                return Ok(());
            }
            let exited = tokio::select! {
                biased;
                _ = self.config.shutdown.cancelled() => return Err(SupervisorError::Stopped),
                status = self.child.wait() => Some(status?),
                _ = tokio::time::sleep_until(deadline) => return Err(SupervisorError::StartupTimeout),
                _ = sleep(self.config.timings.poll) => None,
            };
            if let Some(status) = exited {
                self.adopt_survivors()?;
                return Err(exit_error(status));
            }
        }
    }

    /// `forced` is true only when SIGKILL was actually delivered to a group
    /// proven to be ours; a group we cannot prove is ours fails closed.
    async fn stop(&mut self) -> Result<bool, SupervisorError> {
        let group = self.group.ok_or(SupervisorError::Stopped)?;
        self.output.event("stopping");
        self.remember_members()?;
        self.signal_owned_group(Signal::SIGTERM)?;
        // The leader may exit before its runtime descendants. Their grace
        // period is part of the same 17 seconds, not a new budget.
        let deadline = Instant::now() + self.config.timings.terminate;
        let graceful = match tokio::time::timeout_at(deadline, self.child.wait()).await {
            Ok(status) => {
                status?;
                self.adopt_survivors()?;
                tokio::time::timeout_at(deadline, wait_group_gone(group))
                    .await
                    .map_or(Ok(false), |done| done.map(|()| true))?
            }
            Err(_) => false,
        };
        if !graceful {
            self.output.event("forced stop");
            if !self.signal_owned_group(Signal::SIGKILL)? {
                // Nothing provably ours to kill: report it, never forced=true.
                self.output.event("failed: unowned group survives");
                return Err(SupervisorError::InstanceConflict);
            }
            self.child.wait().await?;
            self.adopt_survivors()?;
            self.reclaimed(group).await?;
        }
        self.group = None;
        self.output.finish().await;
        Ok(!graceful)
    }
    /// Crash path: the leader is already reaped and its survivors adopted.
    async fn finish(&mut self) -> Result<(), SupervisorError> {
        if let Some(group) = self.group {
            if self.signal_owned_group(Signal::SIGKILL)? {
                self.reclaimed(group).await?;
            } else if group_exists(group)? {
                // A group with no proven member must not be signalled, and
                // must not be restarted beside either. Keep the record.
                return Err(SupervisorError::InstanceConflict);
            }
        }
        self.group = None;
        self.output.finish().await;
        Ok(())
    }
    /// SIGKILL'd members linger only until reaped by their new parent.
    async fn reclaimed(&self, group: Pid) -> Result<(), SupervisorError> {
        timeout(Duration::from_secs(5), wait_group_gone(group))
            .await
            .map_err(|_| SupervisorError::InstanceConflict)??;
        Ok(())
    }
}

async fn wait_group_gone(group: Pid) -> io::Result<()> {
    while group_exists(group)? {
        sleep(Duration::from_millis(10)).await;
    }
    Ok(())
}

/// F3/N1: loopback has no peer identity, so a 200 from a port proves
/// nothing. Before the Admin token leaves this process, every visible
/// LISTEN socket on both ports must be held by this generation's group.
async fn listeners_owned(
    config: &StartConfig,
    ports: Ports,
    group: Option<Pid>,
) -> Result<bool, SupervisorError> {
    let group = group.ok_or(SupervisorError::InstanceConflict)?;
    if launcher_enabled(config) {
        return match agent24_os_proc::owns_listening_ports(
            group.as_raw(),
            &[ports.data, ports.admin],
        ) {
            Ok(owned) => Ok(owned),
            Err(error)
                if error.kind() == io::ErrorKind::Unsupported
                    && (cfg!(test) || !cfg!(target_os = "macos")) =>
            {
                Ok(true)
            }
            Err(_) => Ok(false),
        };
    }
    for port in [ports.data, ports.admin] {
        let found = identity::listeners(port, config.timings.health_request).await;
        if !judge_listeners(found, group, |pid| {
            nix::unistd::getpgid(Some(Pid::from_raw(i32::try_from(pid).ok()?))).ok()
        })? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Only a listener confirmed to sit in another process group is a port
/// conflict (L4, never retried). A failed or timed-out query, a socket whose
/// holder is not visible, or a holder gone before its PGID was read proves
/// nothing either way: not ready yet, retried until the readiness deadline,
/// and the Admin token is still withheld.
fn judge_listeners(
    found: io::Result<identity::Listeners>,
    group: Pid,
    pgid_of: impl Fn(u32) -> Option<Pid>,
) -> Result<bool, SupervisorError> {
    let Ok(found) = found else {
        return Ok(false);
    };
    let mut proven = !found.unattributed && !found.pids.is_empty();
    for pid in found.pids {
        match pgid_of(pid) {
            Some(pgid) if pgid == group => {}
            Some(_) => return Err(SupervisorError::InstanceConflict),
            None => proven = false,
        }
    }
    Ok(proven)
}

/// A zombie-only group holds no ports, files or CPU and cannot be signalled
/// further, so it counts as gone; it vanishes once its parent reaps it.
fn group_exists(group: Pid) -> io::Result<bool> {
    let outcome = killpg_outcome(killpg(group, None), || {
        identity::group_has_live_member(group)
    })?;
    Ok(outcome == KillOutcome::Delivered)
}

#[derive(Debug, PartialEq, Eq)]
enum KillOutcome {
    Delivered,
    NoGroup,
    /// macOS only: every member is an unreaped zombie, so killpg (even with
    /// signal 0) fails with EPERM. Linux delivers to zombies and never gets
    /// here; there a zombie-only group reads as Delivered until reaped.
    ZombieOnly,
}

/// The one place EPERM is classified, shared by signal_group and
/// group_exists. EPERM is harmless only when no live member remains;
/// otherwise a member we may not signal survives, which stays an error.
fn killpg_outcome(
    result: nix::Result<()>,
    live_member: impl FnOnce() -> io::Result<bool>,
) -> io::Result<KillOutcome> {
    match result {
        Ok(()) => Ok(KillOutcome::Delivered),
        Err(nix::errno::Errno::ESRCH) => Ok(KillOutcome::NoGroup),
        Err(nix::errno::Errno::EPERM) if !live_member()? => Ok(KillOutcome::ZombieOnly),
        Err(error) => Err(io::Error::from_raw_os_error(error as i32)),
    }
}
impl Drop for ManagedProcess {
    fn drop(&mut self) {
        // Explicit stop already ran on every graceful path. A drop without it means
        // agent24d is going away abnormally: leave the launcher generation for the
        // next agent24d to adopt; its orphan timeout reaps it if nobody does.
        if launcher_enabled(&self.config) {
            return;
        }
        let _ = self.signal_owned_group(Signal::SIGKILL);
    }
}

/// A zombie-only group has nothing left to signal; callers then wait for it
/// to be reaped.
fn signal_group(group: Pid, signal: Signal) -> io::Result<()> {
    killpg_outcome(killpg(group, signal), || {
        identity::group_has_live_member(group)
    })?;
    Ok(())
}

fn launcher_enabled(config: &StartConfig) -> bool {
    semver::Version::parse(config.lock.tag.trim_start_matches('v'))
        .is_ok_and(|v| v >= semver::Version::new(0, 6, 0))
        || (cfg!(debug_assertions)
            && std::env::var("AGENT24_DEV_IDORIS_LAUNCHER").as_deref() == Ok("1"))
}

fn serve_command(
    config: &StartConfig,
    budget: &Budget,
    key: Option<&str>,
) -> Result<Command, SupervisorError> {
    // macOS has no fexecve: verify the installed bytes immediately before each
    // spawn, accepting the remaining same-UID path-swap race after this check.
    config.installed.verify_for_launch()?;
    let binary = fs::canonicalize(config.installed.binary())?;
    let directory = fs::canonicalize(config.installed.directory())?;
    if !binary.starts_with(&directory) {
        return Err(SupervisorError::Configuration);
    }
    // Revalidate the capability at use time; setup's directory may have been
    // replaced with a symlink after it returned.
    if fs::canonicalize(config.components.path())? != config.components.0 {
        return Err(SupervisorError::Configuration);
    }
    let mut command = Command::new(binary);
    command
        .env_clear()
        .current_dir(&directory)
        .args([
            "serve",
            if launcher_enabled(config) {
                "--launcher-stdin"
            } else {
                "--admin-token-stdin"
            },
        ])
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Only the launcher generation survives agent24d in its own session; v0.5.0
    // has no adopt path, so a surviving instance would block the next start.
    if launcher_enabled(config) {
        // A pipe would close with agent24d and make the surviving iDoris panic
        // on its next log write (iDoris #621), so its output goes to /dev/null.
        command
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(false);
        agent24_os_proc::independent_session(command.as_std_mut());
    } else {
        command.process_group(0);
    }
    // Fixed PATH prevents parent PATH injection into any runtime subprocess.
    command.env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin");
    for name in ["HOME", "TMPDIR", "LANG", "LC_ALL", "LC_CTYPE", "TZ"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
        .env("IDORIS_COMPONENTS_DIR", config.components.path())
        .env("IDORIS_DB_PATH", &config.data.db)
        .env("IDORIS_STATE_DIR", &config.data.state)
        .env("IDORIS_CATALOG", directory.join("config/catalog.yaml"))
        .env(
            "IDORIS_ROUTING_POLICY",
            directory.join("config/routing-policy.yaml"),
        )
        .env(
            "IDORIS_ALLOCATED_BUDGET_BYTES",
            budget.allocated_bytes.to_string(),
        )
        .env(
            "IDORIS_PORT",
            if launcher_enabled(config) {
                "0".into()
            } else {
                config.ports.data.to_string()
            },
        )
        .env(
            "IDORIS_ADMIN_PORT",
            if launcher_enabled(config) {
                "0".into()
            } else {
                config.ports.admin.to_string()
            },
        )
        .env("IDORIS_BIND_HOST", "127.0.0.1")
        .env("IDORIS_DEPLOY_MODE", "personal");
    if let Some(key) = key {
        command.env("IDORIS_OMLX_API_KEY", key);
    }
    Ok(command)
}

async fn write_token(
    writer: &mut (impl tokio::io::AsyncWrite + Unpin),
    token: &str,
    deadline: Instant,
    shutdown: &CancellationToken,
) -> Result<(), SupervisorError> {
    if Instant::now() >= deadline {
        return Err(SupervisorError::TokenHandoff);
    }
    let frame = Zeroizing::new(format!("{token}\n"));
    let handoff = async {
        writer.write_all(frame.as_bytes()).await?;
        writer.shutdown().await
    };
    tokio::select! {
        biased;
        _ = shutdown.cancelled() => Err(SupervisorError::Stopped),
        result = tokio::time::timeout_at(deadline, handoff) => {
            match result { Ok(Ok(())) => Ok(()), _ => Err(SupervisorError::TokenHandoff) }
        }
    }
}

fn mint_token() -> io::Result<Zeroizing<String>> {
    let mut bytes = Zeroizing::new([0_u8; 32]);
    File::open("/dev/urandom")?.read_exact(bytes.as_mut())?;
    let mut token = Zeroizing::new(String::with_capacity(64));
    const HEX: &[u8] = b"0123456789abcdef";
    for byte in bytes.iter() {
        token.push(HEX[(byte >> 4) as usize] as char);
        token.push(HEX[(byte & 15) as usize] as char);
    }
    Ok(token)
}

#[derive(Deserialize)]
struct Health {
    status: String,
    service: String,
    version: String,
    contract_version: String,
    instance_id: String,
    dev_no_key_enabled: bool,
}
fn validate_health(body: &[u8], lock: &IdorisLock) -> Result<(), SupervisorError> {
    let value: Health = serde_json::from_slice(body).map_err(|_| SupervisorError::Contract)?;
    if value.status != "ok"
        || value.service != "idoris"
        || value.version != lock.tag.trim_start_matches('v')
        || value.instance_id.trim().is_empty()
        || value.dev_no_key_enabled
        || lock.check_contract(&value.contract_version).is_err()
    {
        return Err(SupervisorError::Contract);
    }
    Ok(())
}
