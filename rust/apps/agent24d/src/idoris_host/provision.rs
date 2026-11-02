//! Install/connect transactions. Only non-sensitive intent and compensation IDs reach disk.
use super::*;
use agent24_idoris::{
    download::{DownloadOptions, DownloadProgress, download_locked_asset},
    install::{InstallError, InstallOptions, OperationLock, install_locked_asset, operation_lock},
    keys::{KeyError, Keys},
    omlx_probe::{ServiceIdentity, read_settings_key},
    setup::{SetupBudget, SetupError, run_setup},
};
use sha2::{Digest, Sha256};

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
#[derive(Serialize)]
pub struct Candidate {
    endpoint: String,
    identity: &'static str,
    authentication: &'static str,
    models: Vec<String>,
}
#[derive(Serialize)]
pub struct Candidates {
    candidates: Vec<Candidate>,
    pub key_from_local_settings: bool,
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
    async fn verify(&self, record: &Installation) -> Result<InstalledVersion, Code>;
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
        download_locked_asset(
            IdorisLock::embedded(),
            agent24_idoris::host_triple().ok_or(Code::NotInstalled)?,
            &root.join("cache"),
            &DownloadOptions {
                progress: Some(progress),
                ..DownloadOptions::default()
            },
            cancel,
        )
        .await
        .map_err(|e| match e {
            agent24_idoris::download::DownloadError::Cancelled => Code::Cancelled,
            agent24_idoris::download::DownloadError::Busy => Code::Busy,
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
            agent24_idoris::host_triple().ok_or(Code::NotInstalled)?,
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
        tokio::task::spawn_blocking(move || agent24_idoris::install::prepare_state_dirs(&root))
            .await
            .map_err(|_| Code::NotInstalled)?
            .map_err(install_code)
    }
    async fn verify(&self, r: &Installation) -> Result<InstalledVersion, Code> {
        let r = r.clone();
        tokio::task::spawn_blocking(move || {
            agent24_idoris::install::verify_existing(
                &r.release_dir,
                IdorisLock::embedded(),
                &VerifiedArchive {
                    path: r.archive_path,
                    sha256: String::new(),
                },
                &Codesign,
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
                agent24_idoris::omlx_probe::SettingsError::NotFound
                | agent24_idoris::omlx_probe::SettingsError::MissingKey => Ok(None),
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
            let cached = parent == old_root.join("cache")
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
impl IdorisHost {
    /// Constructor injection is available before the host is shared with routes.
    pub fn with_provisioner(
        mut self: Arc<Self>,
        provisioner: Arc<dyn Provisioner>,
        data_keys: Arc<dyn Keychain>,
        omlx_keys: Arc<dyn Keychain>,
    ) -> Result<Arc<Self>, Code> {
        let host = Arc::get_mut(&mut self).ok_or(Code::Busy)?;
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
    pub async fn candidates(&self, confirmed: Option<&str>) -> Result<Candidates, Code> {
        let endpoint = confirmed
            .map(Endpoint::parse)
            .transpose()
            .map_err(|_| Code::InvalidRequest)?;
        // Discovery without a confirmed endpoint never sends a credential.
        let key = self.provisioner.upstream_key().await?;
        let results = OmlxProbe::new()
            .map_err(|_| Code::OmlxUnreachable)?
            .probe(endpoint.as_ref(), key.as_ref())
            .await;
        Ok(Candidates {
            key_from_local_settings: key.is_some(),
            candidates: results
                .into_iter()
                .map(|p| {
                    let (authentication, models) = match p.status {
                        ProbeStatus::Ready { models } => ("ready", models),
                        ProbeStatus::AuthRequired => ("auth_required", vec![]),
                        ProbeStatus::AuthRejected => ("auth_rejected", vec![]),
                        ProbeStatus::Unreachable => ("unreachable", vec![]),
                        ProbeStatus::UnknownService => ("unknown_service", vec![]),
                        _ => ("invalid_response", vec![]),
                    };
                    Candidate {
                        endpoint: p.endpoint.as_str().into(),
                        identity: if p.identity == ServiceIdentity::Omlx {
                            "omlx"
                        } else {
                            "unknown_service"
                        },
                        authentication,
                        models,
                    }
                })
                .collect(),
        })
    }
    pub async fn install(
        self: &Arc<Self>,
        request: InstallRequest,
        key: Option<&str>,
    ) -> Result<Operation, Code> {
        if !request.consent {
            return Err(Code::ConsentRequired);
        }
        self.submit_provision(Action::Install, key, &request, None)
            .await
    }
    pub async fn connect(
        self: &Arc<Self>,
        request: ConnectRequest,
        key: Option<&str>,
    ) -> Result<Operation, Code> {
        let endpoint = Endpoint::parse(&request.endpoint).map_err(|_| Code::InvalidRequest)?;
        let mut request = request;
        request.endpoint = endpoint.as_str().into();
        let consent = request.consent.as_ref().ok_or(Code::ConsentRequired)?;
        if !consent.share_omlx_key
            || consent.daily_model != request.daily_model
            || consent.deep_model != request.deep_model
        {
            return Err(Code::ConsentRequired);
        }
        if std::iter::once(&request.daily_model)
            .chain(request.deep_model.iter())
            .any(|m| m.is_empty() || m.len() > 512 || m.chars().any(char::is_control))
        {
            return Err(Code::InvalidRequest);
        }
        self.submit_provision(Action::Connect, key, &request.clone(), Some(request))
            .await
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
            let valid = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(Code::Cancelled),
                verified = self.provisioner.verify(old) => verified.is_ok(),
            };
            if valid {
                if self.in_managed_root(old) {
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
                    self.mark_legacy_deletable(old);
                }
                Ok(())
            }
            other => other,
        }
    }
    /// A receipt is current when its release lives under the (canonical) managed root and,
    /// if it recorded one, was installed under that same root.
    pub(super) fn in_managed_root(&self, r: &Installation) -> bool {
        r.release_dir.starts_with(&self.install_root)
            && r.install_root
                .as_ref()
                .is_none_or(|recorded| *recorded == self.install_root)
    }
    /// Record the superseded legacy paths without deleting anything. The file is
    /// advisory only: any future `agent24 idoris gc` MUST re-validate every path with
    /// [`legacy_deletable`] (or stricter) and never trust this file's contents.
    fn mark_legacy_deletable(&self, old: &Installation) {
        let paths = legacy_deletable(&self.root, &[&old.release_dir, &old.archive_path]);
        if paths.is_empty() {
            return;
        }
        let _ = write_json(
            &self.root.join("legacy-install.deletable.json"),
            &serde_json::json!({
                "schema": 1,
                "advisory": "re-validate every path before deleting; see legacy_deletable()",
                "superseded_by": self.install_root,
                "paths": paths,
            }),
        );
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
        });
        let record = Installation {
            release_dir: installed.directory().into(),
            archive_path: archive.path,
            install_root: Some(self.install_root.clone()),
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
        let endpoint = Endpoint::parse(&request.endpoint).map_err(|_| Code::InvalidRequest)?;
        let (installed, budget, key) = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(Code::Cancelled),
            prepared = async {
        let installed = self.provisioner.verify(&previous).await?;
        let budget = self
            .provisioner
            .budget(request.core_reserved_bytes.or(previous.core_reserved_bytes))
            .await?;
        let key = if previous.omlx_url.as_deref() == Some(endpoint.as_str())
            && let Some(key_id) = &previous.omlx_key_id
        {
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
        let probe_client = OmlxProbe::new().map_err(|_| Code::OmlxUnreachable)?;
        let probes = tokio::select! {
            biased; _ = cancel.cancelled() => return Err(Code::Cancelled),
            results = probe_client.probe(Some(&endpoint), key.as_ref()) => results,
        };
        let probe = probes
            .into_iter()
            .find(|p| p.endpoint == endpoint)
            .ok_or(Code::OmlxUnreachable)?;
        if probe.identity != ServiceIdentity::Omlx
            && !matches!(probe.status, ProbeStatus::Unreachable)
        {
            return Err(Code::UnknownService);
        }
        let models = match &probe.status {
            ProbeStatus::Ready { models } if probe.identity == ServiceIdentity::Omlx => models,
            ProbeStatus::AuthRequired => return Err(Code::OmlxAuthRequired),
            ProbeStatus::AuthRejected => return Err(Code::OmlxAuthRejected),
            ProbeStatus::Unreachable => return Err(Code::OmlxUnreachable),
            _ => return Err(Code::UnknownService),
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
        if let Some(deep) = request.deep_model {
            bindings.push((Role::Deep, deep));
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
        pending.candidate.omlx_url = Some(request.endpoint);
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
        .map_err(key_code)?;
        let mut new_supervisor = None;
        let result = async {
            self.phase(id, Phase::Setup).await;
            let key = key.map(ApiKey::into_secret);
            let setup = match run_setup(
                &installed,
                &self.root.join("instances/managed"),
                &probe,
                key.clone(),
                &bindings,
                Some(budget),
                cancel,
            )
            .await
            {
                Ok(setup) => setup,
                Err(error) => {
                    if let SetupError::Rejected(code) = error
                        && let Some(op) = self.inner.lock().await.operations.get_mut(id)
                    {
                        op.setup_code = Some(code);
                    }
                    return Err(if matches!(error, SetupError::Cancelled) {
                        Code::Cancelled
                    } else {
                        Code::SetupFailed
                    });
                }
            };
            pending.candidate.components_dir = Some(setup.components_dir);
            write_json(&self.root.join("pending-connect.json"), &pending)?;
            check_cancel(cancel)?;
            self.phase(id, Phase::IssuingKey).await;
            let roles: Vec<&str> = if bindings.len() == 2 {
                vec!["daily", "deep"]
            } else {
                vec!["daily"]
            };
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
            self.block_gateway();
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
            let gateway = ManagedIdoris::prepare(&config, &secrets, cancel)
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
            let verification = gateway
                .verify_connection(false, cancel)
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
            self.router.replace_idoris(gateway);
            i.installation = Some(pending.candidate.clone());
            i.supervisor = new_supervisor.take();
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
                self.block_gateway();
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
        let installed = self.provisioner.verify(&pending.previous).await?;
        let keys = Keys::new(
            &installed,
            &self.root.join("data/idoris.sqlite3"),
            self.data_keys.clone(),
        )
        .map_err(key_code)?;
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
