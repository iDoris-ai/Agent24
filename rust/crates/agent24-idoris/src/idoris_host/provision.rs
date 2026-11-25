//! Install/connect transactions. Only non-sensitive intent and compensation IDs reach disk.
use super::*;
use crate::{
    download::{DownloadOptions, DownloadProgress, download_locked_asset},
    install::{InstallError, InstallOptions, OperationLock, install_locked_asset, operation_lock},
    keys::{KeyError, Keys},
    setup::{SetupBudget, SetupError},
    upstream::{SettingsError, read_settings_key},
};
use sha2::{Digest, Sha256};

/// Per-command bound for the key CLI. Tests run the fixture interpreter on loaded
/// machines where a cold start alone can exceed the production bound.
#[cfg(not(test))]
const KEY_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(test)]
const KEY_COMMAND_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallRequest {
    pub consent: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectConsent {
    pub share_omlx_key: bool,
    pub daily_model: String,
    #[serde(default)]
    pub deep_model: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectRequest {
    #[serde(default = "default_source")]
    pub source: String,
    pub endpoint: String,
    pub daily_model: String,
    #[serde(default)]
    pub deep_model: Option<String>,
    #[serde(default)]
    pub save_omlx_key: bool,
    #[serde(default)]
    pub replace: bool,
    /// Required on first connect; an absent allocation must never become zero.
    #[serde(default)]
    pub core_reserved_bytes: Option<u64>,
    #[serde(default)]
    pub consent: Option<ConnectConsent>,
}
fn default_source() -> String {
    "omlx".to_owned()
}
#[derive(Serialize)]
pub struct Candidate {
    pub endpoint: String,
    pub identity: &'static str,
    pub authentication: &'static str,
    pub models: Vec<String>,
}
#[derive(Serialize)]
pub struct Candidates {
    pub candidates: Vec<Candidate>,
    pub key_from_local_settings: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setup: Option<crate::setup::OmlxSetup>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingConnect {
    previous: Installation,
    candidate: Installation,
    committed: bool,
    operation_id: String,
}

/// Replace only platform boundaries in tests; setup, keys, supervisor and chat remain real.
#[async_trait]
pub trait Provisioner: Send + Sync {
    async fn budget_suggestion(&self) -> Result<agent24_hw::IdorisBudgetSuggestion, &'static str> {
        tokio::task::spawn_blocking(|| {
            agent24_hw::HardwareReport::from_env(&agent24_hw::SystemProbe::new())
                .idoris_budget_suggestion()
        })
        .await
        .map_err(|_| "hardware_probe_failed")?
    }
    async fn download(
        &self,
        root: &Path,
        cancel: &CancellationToken,
        progress: watch::Sender<DownloadProgress>,
    ) -> Result<VerifiedArchive, Code>;
    async fn install(
        &self,
        root: &Path,
        archive: &VerifiedArchive,
        cancel: &CancellationToken,
    ) -> Result<InstalledVersion, Code>;
    async fn verify(
        &self,
        record: &Installation,
        mode: VerifyMode,
    ) -> Result<InstalledVersion, Code>;
    /// Create the state directories (`data/`, `instances/`) below the state root.
    async fn prepare_state(&self, _root: &Path) -> Result<(), Code> {
        Ok(())
    }
    async fn upstream_key(&self) -> Result<Option<ApiKey>, Code>;
    async fn budget(&self, core: Option<u64>) -> Result<SetupBudget, Code>;
}
pub struct SystemProvisioner;
#[async_trait]
impl Provisioner for SystemProvisioner {
    async fn download(
        &self,
        root: &Path,
        cancel: &CancellationToken,
        progress: watch::Sender<DownloadProgress>,
    ) -> Result<VerifiedArchive, Code> {
        if let Some(bundle) = std::env::var_os("AGENT24_IDORIS_BUNDLE") {
            let path = std::path::PathBuf::from(bundle);
            tokio::task::spawn_blocking(move || {
                crate::download::bundled_locked_asset(
                    IdorisLock::embedded(),
                    crate::host_triple()
                        .ok_or(crate::download::DownloadError::UnsupportedPlatform)?,
                    &path,
                )
            })
            .await
            .map_err(|_| Code::NotInstalled)?
        } else {
            download_locked_asset(
                IdorisLock::embedded(),
                crate::host_triple().ok_or(Code::NotInstalled)?,
                &root.join("cache"),
                &DownloadOptions {
                    progress: Some(progress),
                    ..DownloadOptions::default()
                },
                cancel,
            )
            .await
        }
        .map_err(|e| match e {
            crate::download::DownloadError::Cancelled => Code::Cancelled,
            crate::download::DownloadError::Busy => Code::Busy,
            _ => Code::NotInstalled,
        })
    }
    async fn install(
        &self,
        root: &Path,
        archive: &VerifiedArchive,
        cancel: &CancellationToken,
    ) -> Result<InstalledVersion, Code> {
        let executable = std::env::current_exe().map_err(|_| Code::NotInstalled)?;
        let mut options = InstallOptions::new(root, executable);
        options.child_args = vec!["__idoris-install-child".into()];
        install_locked_asset(
            IdorisLock::embedded(),
            crate::host_triple().ok_or(Code::NotInstalled)?,
            archive,
            &options,
            Codesign,
            cancel,
        )
        .await
        .map_err(install_code)
    }
    async fn prepare_state(&self, root: &Path) -> Result<(), Code> {
        let root = root.to_owned();
        tokio::task::spawn_blocking(move || crate::install::prepare_state_dirs(&root))
            .await
            .map_err(|_| Code::NotInstalled)?
            .map_err(install_code)
    }
    async fn verify(&self, r: &Installation, mode: VerifyMode) -> Result<InstalledVersion, Code> {
        let r = r.clone();
        tokio::task::spawn_blocking(move || {
            crate::install::verify_existing(
                &r.release_dir,
                IdorisLock::embedded(),
                &VerifiedArchive {
                    path: r.archive_path,
                    sha256: String::new(),
                },
                &Codesign,
                mode,
            )
        })
        .await
        .map_err(|_| Code::NotInstalled)?
        .map_err(install_code)
    }
    async fn upstream_key(&self) -> Result<Option<ApiKey>, Code> {
        tokio::task::spawn_blocking(read_settings_key)
            .await
            .map_err(|_| Code::KeyUnavailable)?
            .map(Some)
            .or_else(|e| match e {
                SettingsError::NotFound | SettingsError::MissingKey => Ok(None),
                _ => Err(Code::KeyUnavailable),
            })
    }
    async fn budget(&self, core: Option<u64>) -> Result<SetupBudget, Code> {
        tokio::task::spawn_blocking(move || {
            let report = agent24_hw::HardwareReport::from_env(&agent24_hw::SystemProbe::new());
            SetupBudget::from_report(Some(&report), core).map_err(|_| Code::NotConfigured)
        })
        .await
        .map_err(|_| Code::NotConfigured)?
    }
}
/// The only legacy paths that may ever be listed as deletable (H2 allow-list): a release
/// directory `<old_root>/<major.minor.patch>` or a verified archive
/// `<old_root>/cache/<name>.verified`, each directly below the canonical old root and
/// not a symlink. Everything else under the old root (`data/`, `instances/`,
/// `transactions/`, `installation.json`, `operations.json`, the root itself) is state
/// and never matches.
pub(super) fn legacy_deletable(old_root: &Path, candidates: &[&Path]) -> Vec<PathBuf> {
    let Ok(old_root) = std::fs::canonicalize(old_root) else {
        return Vec::new();
    };
    let release_name = |name: &str| {
        let parts: Vec<_> = name.split('.').collect();
        parts.len() == 3
            && parts.iter().all(|p| {
                !p.is_empty()
                    && p.bytes().all(|b| b.is_ascii_digit())
                    && (p.len() == 1 || !p.starts_with('0'))
            })
    };
    // Canonical on both sides, and the cache itself must be a real directory: a symlinked
    // `cache` could otherwise make any directory look like "the cache".
    let cache = std::fs::symlink_metadata(old_root.join("cache"))
        .ok()
        .filter(|m| m.file_type().is_dir())
        .and_then(|_| std::fs::canonicalize(old_root.join("cache")).ok());
    candidates
        .iter()
        .filter_map(|path| {
            let name = path.file_name()?.to_str()?;
            if std::fs::symlink_metadata(path)
                .ok()?
                .file_type()
                .is_symlink()
            {
                return None;
            }
            let parent = std::fs::canonicalize(path.parent()?).ok()?;
            let release = parent == old_root && release_name(name);
            let cached = cache.as_ref() == Some(&parent)
                && name.len() > ".verified".len()
                && name.ends_with(".verified")
                && !name.starts_with('.');
            (release || cached).then(|| parent.join(name))
        })
        .collect()
}
fn install_code(e: InstallError) -> Code {
    match e {
        InstallError::Busy => Code::Busy,
        InstallError::Cancelled => Code::Cancelled,
        _ => Code::NotInstalled,
    }
}
fn key_code(e: KeyError) -> Code {
    match e {
        KeyError::Keychain(KeychainError::KeychainLocked) => Code::KeychainLocked,
        KeyError::Busy => Code::Busy,
        _ => Code::KeyUnavailable,
    }
}
fn keychain_code(e: KeychainError) -> Code {
    if e == KeychainError::KeychainLocked {
        Code::KeychainLocked
    } else {
        Code::KeyUnavailable
    }
}
fn check_cancel(cancel: &CancellationToken) -> Result<(), Code> {
    if cancel.is_cancelled() {
        Err(Code::Cancelled)
    } else {
        Ok(())
    }
}
pub(super) async fn wait_operation(host: &Arc<IdorisHost>, id: &str) -> Result<(), Code> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(300);
    loop {
        let operation = host.operation(id).await?;
        if let Some(result) = operation.result {
            return match result {
                Outcome::Succeeded => Ok(()),
                Outcome::Failed | Outcome::Cancelled => {
                    Err(operation.error_code.unwrap_or(Code::SetupFailed))
                }
            };
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(Code::Busy);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
/// Read only the local upstream's model catalogue, never sending credentials.
pub async fn upstream_models(input: &str) -> Result<Vec<String>, Code> {
    let endpoint = Endpoint::parse_openai_compat(input).map_err(|_| Code::InvalidRequest)?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|_| Code::OmlxUnreachable)?;
    let response = client
        .get(format!("{}v1/models", endpoint.as_str()))
        .send()
        .await
        .map_err(|_| Code::OmlxUnreachable)?;
    if !response.status().is_success() {
        return Err(Code::OmlxUnreachable);
    }
    #[derive(Deserialize)]
    struct Model {
        id: String,
    }
    #[derive(Deserialize)]
    struct Catalogue {
        data: Vec<Model>,
    }
    let body = response.bytes().await.map_err(|_| Code::UnknownService)?;
    let catalogue: Catalogue = serde_json::from_slice(&body).map_err(|_| Code::UnknownService)?;
    Ok(catalogue.data.into_iter().map(|m| m.id).collect())
}

pub(super) fn bootstrap_daily(
    models: &[String],
    recommendation: Option<&serde_json::Value>,
) -> Option<String> {
    recommendation
        .filter(|r| r["installed"].as_bool() == Some(true))
        .and_then(|r| r["model_id"].as_str())
        .filter(|id| models.iter().any(|m| m == id) && is_chat_model_id(id))
        .map(str::to_owned)
        .or_else(|| models.iter().find(|m| is_chat_model_id(m)).cloned())
}

impl IdorisHost {
    /// Best-effort first launch bootstrap. Callers should run this in the
    /// background so an unavailable local oMLX never delays daemon readiness.
    pub async fn bootstrap_core(self: &Arc<Self>) -> Result<(), Code> {
        self.bootstrap_core_inner(None).await
    }
    #[cfg(test)]
    pub(super) async fn bootstrap_core_with_candidates(
        self: &Arc<Self>,
        candidates: Candidates,
    ) -> Result<(), Code> {
        self.bootstrap_core_inner(Some(candidates)).await
    }
    async fn bootstrap_core_inner(
        self: &Arc<Self>,
        candidate_override: Option<Candidates>,
    ) -> Result<(), Code> {
        // Each bootstrap is a new attempt: a fixed idempotency key would replay
        // the previous (possibly failed, or long finished) operation forever.
        let attempt = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or_default();
        let key = |step: &str| format!("core-{step}-{attempt}");
        let status = self.status().await;
        if status.state == State::NotInstalled {
            let operation = self
                .install(InstallRequest { consent: false }, Some(&key("install")))
                .await?;
            wait_operation(self, &operation.operation_id).await?;
        }
        let status = self.status().await;
        if status.state == State::NotConfigured || status.roles.is_empty() {
            let candidates = match candidate_override {
                Some(candidates) => candidates,
                None => self.candidates(None).await?,
            };
            let candidate = candidates
                .candidates
                .iter()
                .find(|candidate| {
                    candidate.identity == "omlx"
                        && candidate.authentication == "ready"
                        && candidate.models.iter().any(|m| is_chat_model_id(m))
                })
                .ok_or(Code::OmlxUnreachable)?;
            let recommendation = self
                .read_model_cli(&["recommend", "--role", "chat", "--json"])
                .await;
            let daily_model = bootstrap_daily(&candidate.models, recommendation.as_ref())
                .ok_or(Code::OmlxUnreachable)?;
            let operation = self
                .connect(
                    ConnectRequest {
                        source: "omlx".into(),
                        endpoint: candidate.endpoint.clone(),
                        // Complex tasks are sent as `idoris/deep`; an unbound deep
                        // role leaves the key without it and iDoris answers 403.
                        deep_model: Some(daily_model.clone()),
                        daily_model,
                        save_omlx_key: false,
                        replace: false,
                        core_reserved_bytes: status
                            .budget_suggestion
                            .map(|budget| budget.core_reserved_bytes),
                        consent: None,
                    },
                    Some(&key("connect")),
                )
                .await?;
            wait_operation(self, &operation.operation_id).await?;
        } else if status.state == State::Stopped {
            let operation = self.submit(Action::Start, Some(&key("start"))).await?;
            wait_operation(self, &operation.operation_id).await?;
        }
        Ok(())
    }
    /// Constructor injection is available before the host is shared with routes.
    pub fn with_provisioner(
        mut self: Arc<Self>,
        provisioner: Arc<dyn Provisioner>,
        data_keys: Arc<dyn Keychain>,
        omlx_keys: Arc<dyn Keychain>,
    ) -> Result<Arc<Self>, Code> {
        let host = Arc::get_mut(&mut self).ok_or(Code::Busy)?;
        host.runtime.rebind(provisioner.clone(), omlx_keys.clone());
        host.provisioner = provisioner;
        host.secrets = Arc::new(KeychainSource(data_keys.clone()));
        host.data_keys = data_keys;
        host.omlx_keys = omlx_keys;
        Ok(self)
    }
    pub(super) fn coordinator_lock(&self) -> Result<OperationLock, Code> {
        // A separate outer lock spans download's and install's inner lock lifetimes.
        write_json(
            &self.root.join("transactions/owner.json"),
            &serde_json::json!({"schema": 1}),
        )?;
        operation_lock(&self.root.join("transactions")).map_err(install_code)
    }
    pub async fn candidates(&self, _confirmed: Option<&str>) -> Result<Candidates, Code> {
        let models = self.local_omlx_models().await?;
        let setup = self
            .inner
            .lock()
            .await
            .installation
            .as_ref()
            .and_then(|r| r.omlx_setup.clone());
        // Compatibility envelope for the desktop: models come from the local inventory.
        Ok(Candidates {
            candidates: vec![Candidate {
                endpoint: setup
                    .as_ref()
                    .map(|s| s.endpoint.clone())
                    .unwrap_or_default(),
                identity: "omlx",
                authentication: "ready",
                models: models
                    .into_iter()
                    .filter(|m| matches!(m.kind.as_str(), "chat" | "vision"))
                    .map(|m| m.omlx_id)
                    .collect(),
            }],
            key_from_local_settings: self.provisioner.upstream_key().await?.is_some(),
            setup,
        })
    }
    async fn local_omlx_models(&self) -> Result<Vec<LocalModel>, Code> {
        let value = self
            .read_model_cli(&["models", "local", "--json"])
            .await
            .ok_or(Code::OmlxUnreachable)?;
        parse_local_models(value).map_err(|_| Code::OmlxUnreachable)
    }
    pub async fn install(
        self: &Arc<Self>,
        request: InstallRequest,
        key: Option<&str>,
    ) -> Result<Operation, Code> {
        self.submit_provision(Action::Install, key, &request, None)
            .await
    }
    pub async fn connect(
        self: &Arc<Self>,
        request: ConnectRequest,
        key: Option<&str>,
    ) -> Result<Operation, Code> {
        let mut request = request;
        if request.source == "openai_compat" {
            if request.save_omlx_key {
                return Err(Code::InvalidRequest);
            }
            let endpoint = Endpoint::parse_openai_compat(&request.endpoint)
                .map_err(|_| Code::InvalidRequest)?;
            request.endpoint = endpoint.as_str().into();
        }
        if request.source == "openai_compat" && request.deep_model.is_none() {
            request.deep_model = Some(request.daily_model.clone());
        }
        // Core bootstrap is automatic. The selected models and upstream key are
        // local configuration used only by iDoris and do not need a separate prompt.
        if std::iter::once(&request.daily_model)
            .chain(request.deep_model.iter())
            .any(|m| {
                m.is_empty()
                    || m.len() > 128
                    || !m
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._:/-".contains(&b))
            })
            || !matches!(request.source.as_str(), "omlx" | "openai_compat")
        {
            return Err(Code::InvalidRequest);
        }
        self.submit_provision(Action::Connect, key, &request.clone(), Some(request))
            .await
    }
    /// Replace the daily binding using the committed source and other bindings.
    /// Wait for verification before callers persist their selected default.
    pub async fn replace_daily_model(self: &Arc<Self>, model: &str) -> Result<(), Code> {
        let connection = self
            .inner
            .lock()
            .await
            .installation
            .as_ref()
            .and_then(|r| r.connection.clone());
        let Some(mut request) = connection else {
            return Ok(());
        };
        request.daily_model = model.to_owned();
        request.replace = true;
        request.consent = None;
        let operation = self
            .connect(request, Some(&format!("default-{}", uuid::Uuid::new_v4())))
            .await?;
        wait_operation(self, &operation.operation_id).await
    }
    async fn submit_provision(
        self: &Arc<Self>,
        action: Action,
        key: Option<&str>,
        request: &impl Serialize,
        connect: Option<ConnectRequest>,
    ) -> Result<Operation, Code> {
        let fingerprint = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(request).map_err(|_| Code::InvalidRequest)?)
        );
        let key = key.ok_or(Code::InvalidRequest)?;
        if key.is_empty()
            || key.len() > 128
            || !key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
        {
            return Err(Code::InvalidRequest);
        }
        let mut i = self.inner.lock().await;
        if i.closing {
            return Err(Code::ShuttingDown);
        }
        if let Some(old) = i.operations.values().find(|op| op.idempotency_key == key) {
            return if old.action == action && old.request_fingerprint.as_ref() == Some(&fingerprint)
            {
                Ok(old.clone())
            } else {
                Err(Code::InvalidRequest)
            };
        }
        if i.active.is_some() {
            return Err(Code::Busy);
        }
        if let Some(request) = &connect {
            let record = i.installation.as_ref().ok_or(Code::NotInstalled)?;
            if record.key_id.is_some() && !request.replace {
                return Err(Code::ReplaceRequired);
            }
        }
        let guard = self.coordinator_lock()?;
        if self.root.join("pending-connect.json").exists() {
            return Err(Code::Busy);
        }
        let op = Operation {
            operation_id: uuid::Uuid::new_v4().to_string(),
            action,
            phase: Phase::Queued,
            result: None,
            error_code: None,
            cancellable: true,
            idempotency_key: key.into(),
            request_fingerprint: Some(fingerprint),
            setup_code: None,
            setup_endpoint: None,
            download_progress: None,
            verification: None,
        };
        // The receipt is deliberately structured; arbitrary user text is never persisted.
        let receipt = serde_json::json!({"action": action, "version": IdorisLock::embedded().tag, "confirmed_at": chrono::Utc::now().to_rfc3339(), "request": request});
        if action == Action::Install {
            write_json(
                &self.root.join(format!("consents/{}.json", op.operation_id)),
                &receipt,
            )?;
        }
        i.operations.insert(op.operation_id.clone(), op.clone());
        if let Err(e) = write_json(&self.root.join("operations.json"), &i.operations) {
            i.operations.remove(&op.operation_id);
            return Err(e);
        }
        let token = CancellationToken::new();
        i.active = Some((op.operation_id.clone(), token.clone()));
        let host = self.clone();
        let id = op.operation_id.clone();
        tokio::spawn(async move {
            let _guard = guard;
            let result = if let Some(request) = connect {
                host.execute_connect(&id, request, &token).await
            } else {
                host.execute_install(&id, &token).await
            };
            host.finish(&id, action, result).await;
        });
        Ok(op)
    }
    async fn execute_install(&self, id: &str, cancel: &CancellationToken) -> Result<(), Code> {
        let old = self.inner.lock().await.installation.clone();
        self.phase(id, Phase::Verifying).await;
        // ADR-038 D1: releases live under the managed root. A receipt that still
        // points at the legacy tree is migrated by re-downloading (never moved);
        // the legacy copy keeps serving until the new one has verified.
        let mut migrating = false;
        if let Some(old) = &old {
            // An explicit Install is the moment to be online: without an install-time
            // notarization record, check it in full (`Install`); with one, the at-rest
            // checks are enough to call the install intact.
            let mode = if old.notarized_verified_at.is_some() {
                VerifyMode::AtRest
            } else {
                VerifyMode::Install
            };
            let valid = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(Code::Cancelled),
                verified = self.provisioner.verify(old, mode) => verified.is_ok(),
            };
            if valid {
                if self.in_managed_root(old) {
                    if mode == VerifyMode::Install {
                        self.record_notarization(old).await?;
                    }
                    return Ok(());
                }
                migrating = true;
            }
        }
        let result = self.download_and_install(id, old.clone(), cancel).await;
        match result {
            // A failed migration must not turn a working legacy install into a failed one.
            Err(code) if migrating && code != Code::Cancelled => {
                tracing::warn!(
                    ?code,
                    "iDoris managed-root migration failed; legacy install kept"
                );
                self.inner.lock().await.migration_error = Some(code);
                Ok(())
            }
            Ok(()) if migrating => {
                self.inner.lock().await.migration_error = None;
                if let Some(old) = &old {
                    self.queue_legacy_deletable(old);
                }
                Ok(())
            }
            other => other,
        }
    }
    /// Stamp a receipt whose full install-time check has just passed.
    async fn record_notarization(&self, old: &Installation) -> Result<(), Code> {
        if !IdorisLock::embedded().component_notarized() {
            return Ok(());
        }
        let mut record = old.clone();
        record.notarized_verified_at = Some(chrono::Utc::now().to_rfc3339());
        let mut i = self.inner.lock().await;
        write_json(&self.root.join("installation.json"), &record)?;
        i.installation = Some(record);
        Ok(())
    }
    /// A receipt is current when its release lives under the (canonical) managed root and,
    /// if it recorded one, was installed under that same root.
    pub(super) fn in_managed_root(&self, r: &Installation) -> bool {
        r.release_dir.starts_with(&self.install_root)
            && r.install_root
                .as_ref()
                .is_none_or(|recorded| *recorded == self.install_root)
    }
    /// Remember the superseded legacy paths. They are NOT yet deletable: the running
    /// supervisor may still be executing from them (and auto-restarts onto them) until it
    /// is stopped or restarted from the new receipt. `finalize_legacy_marks` publishes them
    /// once nothing holds them.
    fn queue_legacy_deletable(&self, old: &Installation) {
        let _ = write_json(
            &self.root.join("legacy-install.pending.json"),
            &serde_json::json!({"schema": 1, "paths": [old.release_dir, old.archive_path]}),
        );
    }
    /// Publish queued legacy paths as deletable, only when the receipt already points at the
    /// managed root and no running instance holds them. The marker is advisory: any future
    /// `agent24 idoris gc` MUST re-validate every path with [`legacy_deletable`] (or
    /// stricter), MUST exclude any path held by a running instance, and never trust this file.
    pub(super) fn finalize_legacy_marks(&self, inner: &Inner) {
        let pending_path = self.root.join("legacy-install.pending.json");
        let Ok(body) = std::fs::read(&pending_path) else {
            return;
        };
        let Ok(pending) = serde_json::from_slice::<serde_json::Value>(&body) else {
            return;
        };
        let Ok(queued) = serde_json::from_value::<Vec<PathBuf>>(pending["paths"].clone()) else {
            return;
        };
        if !inner
            .installation
            .as_ref()
            .is_some_and(|r| self.in_managed_root(r))
        {
            return;
        }
        let held = inner.supervisor.is_some()
            && inner
                .supervisor_release
                .as_ref()
                .is_some_and(|dir| queued.contains(dir));
        if held {
            return;
        }
        let candidates: Vec<&Path> = queued.iter().map(PathBuf::as_path).collect();
        let paths = legacy_deletable(&self.root, &candidates);
        if !paths.is_empty()
            && write_json(
                &self.root.join("legacy-install.deletable.json"),
                &serde_json::json!({
                    "schema": 1,
                    "advisory": "re-validate every path before deleting and exclude any path held by a running instance; see legacy_deletable()",
                    "superseded_by": self.install_root,
                    "paths": paths,
                }),
            )
            .is_err()
        {
            return;
        }
        let _ = std::fs::remove_file(pending_path);
    }
    async fn download_and_install(
        &self,
        id: &str,
        old: Option<Installation>,
        cancel: &CancellationToken,
    ) -> Result<(), Code> {
        check_cancel(cancel)?;
        self.phase(id, Phase::Downloading).await;
        let (sender, mut progress) = watch::channel(DownloadProgress::default());
        self.download_progress(id, DownloadProgress::default())
            .await;
        let download = self
            .provisioner
            .download(&self.install_root, cancel, sender);
        tokio::pin!(download);
        let mut progress_open = true;
        let archive = loop {
            tokio::select! {
                result = &mut download => {
                    let latest = *progress.borrow_and_update();
                    self.download_progress(id, latest).await;
                    break result?;
                }
                changed = progress.changed(), if progress_open => {
                    if changed.is_ok() {
                        let latest = *progress.borrow_and_update();
                        self.download_progress(id, latest).await;
                    } else {
                        progress_open = false;
                    }
                }
            }
        };
        self.phase(id, Phase::Verifying).await;
        check_cancel(cancel)?;
        self.phase(id, Phase::Installing).await;
        let installed = self
            .provisioner
            .install(&self.install_root, &archive, cancel)
            .await?;
        self.provisioner.prepare_state(&self.root).await?;
        check_cancel(cancel)?;
        let record = old.unwrap_or(Installation {
            omlx_setup: None,
            connection: None,
            version: IdorisLock::embedded().tag.trim_start_matches('v').into(),
            release_dir: installed.directory().into(),
            archive_path: archive.path.clone(),
            components_dir: None,
            key_id: None,
            data_port: 9011,
            admin_port: 9012,
            roles: vec![],
            core_reserved_bytes: None,
            omlx_url: None,
            omlx_key_id: None,
            desired_running: false,
            verification: None,
            install_root: None,
            notarized_verified_at: None,
        });
        let record = Installation {
            release_dir: installed.directory().into(),
            archive_path: archive.path,
            install_root: Some(self.install_root.clone()),
            // The installer just ran the full check, notarization included.
            notarized_verified_at: IdorisLock::embedded()
                .component_notarized()
                .then(|| chrono::Utc::now().to_rfc3339()),
            ..record
        };
        let mut i = self.inner.lock().await;
        check_cancel(cancel)?;
        i.operations.get_mut(id).ok_or(Code::NotFound)?.cancellable = false;
        write_json(&self.root.join("installation.json"), &record)?;
        i.installation = Some(record);
        Ok(())
    }
    async fn download_progress(&self, id: &str, progress: DownloadProgress) {
        if let Some(op) = self.inner.lock().await.operations.get_mut(id) {
            op.download_progress = Some(progress);
        }
    }
    async fn execute_connect(
        self: &Arc<Self>,
        id: &str,
        request: ConnectRequest,
        cancel: &CancellationToken,
    ) -> Result<(), Code> {
        let previous = self
            .inner
            .lock()
            .await
            .installation
            .clone()
            .ok_or(Code::NotInstalled)?;
        let (installed, budget, key) = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(Code::Cancelled),
            prepared = async {
        let installed = self.provisioner.verify(&previous, VerifyMode::AtRest).await?;
        let budget = self
            .provisioner
            .budget(request.core_reserved_bytes.or(previous.core_reserved_bytes))
            .await?;
        let key = if request.source == "openai_compat" {
            None
        } else if let Some(key_id) = &previous.omlx_key_id {
            let store = self.omlx_keys.clone();
            let key_id = key_id.clone();
            Some(
                ApiKey::new(
                    tokio::task::spawn_blocking(move || store.get(&key_id))
                        .await
                        .map_err(|_| Code::KeyUnavailable)?
                        .map_err(keychain_code)?,
                )
                .map_err(|_| Code::KeyUnavailable)?,
            )
        } else {
            self.provisioner.upstream_key().await?
        };
                Ok::<_, Code>((installed, budget, key))
            } => prepared?,
        };
        let compat = request.source == "openai_compat";
        let models = if compat {
            tokio::select! {
                biased; _ = cancel.cancelled() => return Err(Code::Cancelled),
                result = upstream_models(&request.endpoint) => result?,
            }
        } else {
            self.local_omlx_models()
                .await?
                .into_iter()
                .map(|m| m.omlx_id)
                .collect()
        };
        if !models.contains(&request.daily_model)
            || request
                .deep_model
                .as_ref()
                .is_some_and(|m| !models.contains(m))
        {
            return Err(Code::ModelNotFound);
        }
        write_json(
            &self.root.join(format!("consents/{id}.json")),
            &serde_json::json!({
                "action": "connect", "confirmed_at": chrono::Utc::now().to_rfc3339(), "request": request
            }),
        )?;
        let mut bindings = vec![(Role::Daily, request.daily_model.clone())];
        if let Some(deep) = request.deep_model.clone() {
            bindings.push((Role::Deep, deep));
        }
        const EMBED_MODEL: &str = "bge-m3-mlx-fp16";
        let embed = models.iter().find(|model| {
            if compat {
                model.to_ascii_lowercase().contains("bge-m3")
            } else {
                model.as_str() == EMBED_MODEL
            }
        });
        if let Some(embed) = embed {
            bindings.push((Role::Embed, embed.clone()));
        }
        let mut pending = PendingConnect {
            previous: previous.clone(),
            candidate: previous.clone(),
            committed: false,
            operation_id: id.into(),
        };
        pending.candidate.components_dir = None;
        pending.candidate.key_id = None;
        pending.candidate.omlx_key_id = None;
        pending.candidate.connection = Some(request.clone());
        // Unvalidated until setup resolves it (bootstrap may pass ""); a crash
        // here must leave a journal that recovery can still validate.
        pending.candidate.omlx_url = None;
        pending.candidate.roles = bindings.iter().map(|b| b.0).collect();
        pending.candidate.core_reserved_bytes =
            request.core_reserved_bytes.or(previous.core_reserved_bytes);
        pending.candidate.desired_running = true;
        write_json(&self.root.join("pending-connect.json"), &pending)?;
        let keys = Keys::new(
            &installed,
            &self.root.join("data/idoris.sqlite3"),
            self.data_keys.clone(),
        )
        .map_err(key_code)?
        .with_timeout(KEY_COMMAND_TIMEOUT);
        let mut new_supervisor = None;
        let result = async {
            self.phase(id, Phase::Setup).await;
            let key = key.map(ApiKey::into_secret);
            let setup_result = if compat {
                crate::setup::run_setup_upstream(
                    &installed, &self.root.join("instances/managed"), &request.endpoint,
                    &bindings, "lmstudio", cancel,
                ).await
            } else {
                crate::setup::run_setup_auto(&installed, &self.root.join("instances/managed"),
                    key.clone(), &bindings, Some(budget), cancel).await
            };
            let setup = match setup_result {
                Ok(setup) => setup,
                Err(error) => {
                    if let Some(op) = self.inner.lock().await.operations.get_mut(id) {
                        match &error {
                            SetupError::Rejected(code) => op.setup_code = Some(*code),
                            SetupError::AuthRequired { endpoint } => {
                                op.setup_code = Some(crate::setup::SetupCode::AuthRequired);
                                op.setup_endpoint = Some(endpoint.clone());
                            }
                            _ => {}
                        }
                    }
                    return Err(if matches!(error, SetupError::Cancelled) {
                        Code::Cancelled
                    } else {
                        Code::SetupFailed
                    });
                }
            };
            if let Some(result) = &setup.omlx {
                pending.candidate.omlx_url = Some(result.endpoint.clone());
                if let Some(connection) = &mut pending.candidate.connection {
                    connection.endpoint = result.endpoint.clone();
                }
            }
            pending.candidate.omlx_setup = setup.omlx;
            pending.candidate.components_dir = Some(setup.components_dir);
            write_json(&self.root.join("pending-connect.json"), &pending)?;
            check_cancel(cancel)?;
            self.phase(id, Phase::IssuingKey).await;
            let roles: Vec<&str> = bindings.iter().map(|(role, _)| match role {
                Role::Daily => "daily",
                Role::Deep => "deep",
                Role::Embed => "embed",
            }).collect();
            // Do not drop issuance on cancellation. Its bounded command must finish or compensate.
            let issued = keys
                .issue_pending("managed", &roles)
                .await
                .map_err(key_code)?;
            pending.candidate.key_id = Some(issued.key_id);
            drop(issued.secret);
            write_json(&self.root.join("pending-connect.json"), &pending)?;
            check_cancel(cancel)?;
            if request.save_omlx_key
                && let Some(key) = key.as_ref()
            {
                let key_id = format!("omlx_{}", uuid::Uuid::new_v4().simple());
                pending.candidate.omlx_key_id = Some(key_id.clone());
                write_json(&self.root.join("pending-connect.json"), &pending)?;
                let store = self.omlx_keys.clone();
                let key = key.clone();
                tokio::task::spawn_blocking(move || store.put(&key_id, &key))
                    .await
                    .map_err(|_| Code::KeyUnavailable)?
                    .map_err(keychain_code)?;
            }
            // Old configuration remains on disk while the replacement is tested.
            self.starting_gateway();
            let old = self.inner.lock().await.supervisor.take();
            if let Some(old) = old {
                old.stop().await.map_err(Code::from)?;
            }
            self.phase(id, Phase::Starting).await;
            let (supervisor, ready, failure) = self
                .start_record(
                    &pending.candidate,
                    id,
                    cancel,
                    false,
                    Some(LaunchInputs {
                        budget: Budget {
                            allocated_bytes: budget.bytes(),
                        },
                        omlx_key: key.clone(),
                    }),
                )
                .await?;
            if let Some(ports) = supervisor.runtime_status().borrow().ports {
                pending.candidate.data_port = ports.data;
                pending.candidate.admin_port = ports.admin;
            }
            new_supervisor = Some(supervisor);
            self.phase(id, Phase::VerifyingConnection).await;
            let config = ManagedConfig {
                url: format!("http://127.0.0.1:{}", pending.candidate.data_port),
                key_id: pending
                    .candidate
                    .key_id
                    .clone()
                    .ok_or(Code::KeyUnavailable)?,
            };
            let secrets = ClassifiedSecrets {
                source: self.secrets.as_ref(),
                failure: SyncMutex::new(None),
            };
            let mut gateway = ManagedIdoris::prepare(&config, &secrets, cancel, self.router.as_ref())
                .await
                .map_err(|_| {
                    if cancel.is_cancelled() {
                        Code::Cancelled
                    } else {
                        secrets
                            .failure
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .take()
                            .unwrap_or(Code::VerifyFailed)
                    }
                })?;
            gateway.embedding_enabled = pending.candidate.roles.contains(&Role::Embed);
            let verification = self
                .router
                .verify_connection(&gateway, false, cancel)
                .await
                .map_err(|_| {
                    if cancel.is_cancelled() {
                        Code::Cancelled
                    } else {
                        Code::VerifyFailed
                    }
                })?;
            if verification.model_id.len() > 256
                || verification.model_id.chars().any(|c| {
                    c.is_control()
                        || matches!(c, '\u{061c}' | '\u{200e}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
                })
            {
                return Err(Code::VerifyFailed);
            }
            pending.candidate.verification = Some(VerificationReceipt {
                model_id: verification.model_id,
                record_id: verification.record_id,
                locality: verification.locality,
                verified_at: chrono::Utc::now().to_rfc3339(),
            });
            let mut i = self.inner.lock().await;
            check_cancel(cancel)?;
            i.operations.get_mut(id).ok_or(Code::NotFound)?.cancellable = false;
            // The commit boundary rejects cancellation until the new receipt and gateway agree.
            self.managed.persist_config(&config)?;
            write_json(&self.root.join("installation.json"), &pending.candidate)?;
            pending.committed = true;
            write_json(&self.root.join("pending-connect.json"), &pending)?;
            i.operations.get_mut(id).ok_or(Code::NotFound)?.verification =
                pending.candidate.verification.clone();
            self.router
                .replace_idoris(gateway)
                .map_err(|_| Code::StartFailed)?;
            i.installation = Some(pending.candidate.clone());
            i.supervisor = new_supervisor.take();
            i.supervisor_release = Some(pending.candidate.release_dir.clone());
            i.generation = Some(id.to_owned());
            drop(i);
            self.monitor(
                ready,
                pending.candidate.clone(),
                id.to_owned(),
                cancel.clone(),
                failure,
            );
            Ok(())
        }
        .await;
        if let Err(error) = result {
            self.phase(id, Phase::RollingBack).await;
            let stopped = if let Some(supervisor) = new_supervisor {
                self.end_gateway(&error);
                supervisor.stop().await.map_err(Code::from).map(|_| ())
            } else {
                Ok(())
            };
            self.rollback_connect(&pending, &keys).await?;
            stopped?;
            return Err(error);
        }
        // A failed cleanup keeps the committed journal for startup recovery.
        self.finalize_connect(&pending, &keys).await?;
        Ok(())
    }
    async fn rollback_connect(&self, pending: &PendingConnect, keys: &Keys) -> Result<(), Code> {
        keys.retry_pending().await.map_err(key_code)?;
        if let Some(key_id) = &pending.candidate.key_id {
            keys.revoke(key_id).await.map_err(key_code)?;
            let store = self.data_keys.clone();
            let id = key_id.clone();
            let deleted = tokio::task::spawn_blocking(move || store.delete(&id))
                .await
                .map_err(|_| Code::KeyUnavailable)?;
            if let Err(e) = deleted
                && e != KeychainError::NotFound
            {
                return Err(keychain_code(e));
            }
        }
        if let Some(id) = &pending.candidate.omlx_key_id {
            let store = self.omlx_keys.clone();
            let id = id.clone();
            let deleted = tokio::task::spawn_blocking(move || store.delete(&id))
                .await
                .map_err(|_| Code::KeyUnavailable)?;
            if let Err(e) = deleted
                && e != KeychainError::NotFound
            {
                return Err(keychain_code(e));
            }
        }
        self.managed
            .restore_config(pending.previous.key_id.as_ref().map(|id| ManagedConfig {
                url: format!("http://127.0.0.1:{}", pending.previous.data_port),
                key_id: id.clone(),
            }))?;
        write_json(&self.root.join("installation.json"), &pending.previous)?;
        self.inner.lock().await.installation = Some(pending.previous.clone());
        if let Some(path) = &pending.candidate.components_dir {
            self.remove_generation(path)?;
        }
        std::fs::remove_file(self.root.join("pending-connect.json"))
            .map_err(|_| Code::PersistenceFailed)?;
        Ok(())
    }
    async fn finalize_connect(&self, pending: &PendingConnect, keys: &Keys) -> Result<(), Code> {
        keys.commit_issue().map_err(key_code)?;
        if let Some(old) = &pending.previous.key_id
            && Some(old) != pending.candidate.key_id.as_ref()
        {
            keys.revoke(old).await.map_err(key_code)?;
            let store = self.data_keys.clone();
            let id = old.clone();
            let deleted = tokio::task::spawn_blocking(move || store.delete(&id))
                .await
                .map_err(|_| Code::KeyUnavailable)?;
            if let Err(e) = deleted
                && e != KeychainError::NotFound
            {
                return Err(keychain_code(e));
            }
        }
        if let Some(old) = &pending.previous.omlx_key_id
            && Some(old) != pending.candidate.omlx_key_id.as_ref()
        {
            let store = self.omlx_keys.clone();
            let id = old.clone();
            let deleted = tokio::task::spawn_blocking(move || store.delete(&id))
                .await
                .map_err(|_| Code::KeyUnavailable)?;
            if let Err(e) = deleted
                && e != KeychainError::NotFound
            {
                return Err(keychain_code(e));
            }
        }
        std::fs::remove_file(self.root.join("pending-connect.json"))
            .map_err(|_| Code::PersistenceFailed)?;
        Ok(())
    }
    fn remove_generation(&self, path: &Path) -> Result<(), Code> {
        let parent = self.root.join("instances/managed/components");
        if path.parent() != Some(parent.as_path())
            || path
                .file_name()
                .is_none_or(|name| !name.to_string_lossy().starts_with("gen-"))
        {
            return Err(Code::PersistenceFailed);
        }
        match path.canonicalize() {
            Ok(canonical)
                if canonical == path
                    && parent.canonicalize().ok().as_deref() == Some(parent.as_path()) =>
            {
                std::fs::remove_dir_all(path).map_err(|_| Code::PersistenceFailed)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            _ => Err(Code::PersistenceFailed),
        }
    }
    pub(super) async fn recover_connect(&self) -> Result<(), Code> {
        let _guard = self.coordinator_lock()?;
        let body = match std::fs::read(self.root.join("pending-connect.json")) {
            Ok(body) => body,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(_) => return Err(Code::PersistenceFailed),
        };
        let pending: PendingConnect =
            serde_json::from_slice(&body).map_err(|_| Code::PersistenceFailed)?;
        pending.previous.validate()?;
        pending.candidate.validate()?;
        let installed = self
            .provisioner
            .verify(&pending.previous, VerifyMode::AtRest)
            .await?;
        let keys = Keys::new(
            &installed,
            &self.root.join("data/idoris.sqlite3"),
            self.data_keys.clone(),
        )
        .map_err(key_code)?
        .with_timeout(KEY_COMMAND_TIMEOUT);
        if !pending.committed
            && self.inner.lock().await.supervisor.is_none()
            && self
                .root
                .join("data/state/managed-process.json")
                .try_exists()
                .map_err(|_| Code::PersistenceFailed)?
            && let Err(error) = Supervisor::recover_leftover(
                DataPaths {
                    db: self.root.join("data/idoris.sqlite3"),
                    state: self.root.join("data/state"),
                },
                &pending.previous.release_dir.join("idoris"),
            )
            .await
        {
            // A process identity conflict must not prevent credential revocation.
            keys.retry_pending().await.map_err(key_code)?;
            return Err(Code::from(error));
        }
        if pending.committed {
            self.finalize_connect(&pending, &keys).await?;
            let mut i = self.inner.lock().await;
            if let Some(op) = i.operations.get_mut(&pending.operation_id) {
                op.result = Some(Outcome::Succeeded);
                op.error_code = None;
                op.phase = Phase::Done;
                op.cancellable = false;
                op.verification = pending.candidate.verification.clone();
            }
            write_json(&self.root.join("operations.json"), &i.operations)?;
            Ok(())
        } else {
            self.rollback_connect(&pending, &keys).await
        }
    }
}

/// oMLX lists every model it serves; the daily role needs a chat model, not an
/// embedding, speech, OCR or image one. Same exclusions as the desktop picker.
pub fn is_chat_model_id(id: &str) -> bool {
    const NON_CHAT: [&str; 13] = [
        "flux",
        "ocr",
        "tts",
        "voice",
        "whisper",
        "asr",
        "embed",
        "bge",
        "rerank",
        "mage",
        "diffusion",
        "sd-",
        "-vl",
    ];
    let name = id.to_ascii_lowercase();
    !NON_CHAT.iter().any(|part| name.contains(part))
}

#[cfg(test)]
mod chat_model_filter_tests {
    #[test]
    fn embedding_and_speech_models_are_not_picked_for_daily_chat() {
        for id in [
            "bge-m3-mlx-fp16",
            "Qwen3-Embedding-0.6B",
            "whisper-large",
            "Qwen2.5-VL-7B",
        ] {
            assert!(!super::is_chat_model_id(id), "{id}");
        }
        assert!(super::is_chat_model_id("Qwen3-8B-4bit"));
    }
}

#[derive(Deserialize)]
struct LocalModel {
    omlx_id: String,
    kind: String,
}
fn parse_local_models(value: serde_json::Value) -> Result<Vec<LocalModel>, serde_json::Error> {
    #[derive(Deserialize)]
    struct LocalModels {
        models: Vec<LocalModel>,
    }
    serde_json::from_value::<LocalModels>(value).map(|list| list.models)
}
