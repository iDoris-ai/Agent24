use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use crate::domain::Supervisors;
use agent24_domain::tool::{
    ModuleToolAdvertView, ModuleToolCallError, ModuleToolErrorCode, ModuleToolResult,
};
use agent24_os_proto::drain::{DrainState, Generation};
use agent24_os_proto::kernel_call::{
    KernelLimits, KernelRequest, KernelRequestIds, send_kernel_request, send_module_tool_request,
};
use agent24_tools::{ModuleToolAuthorization, ModuleToolContext, ModuleToolRuntime};
use async_trait::async_trait;
use axum::body::Bytes;
use tokio_util::sync::CancellationToken;

const MAX_TOOL_RESPONSE_BYTES: usize = 64 * 1024;

/// How long a revocation waits for the module to say it stopped the work
/// the revoked grants started (K1-6a-G §2.5).
pub const REVOCATION_NOTICE_WITHIN: Duration = Duration::from_secs(10);

/// What a module answered when told a revocation ended its grants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevocationStop {
    /// It answered 2xx: it reached a safe point and stopped.
    Confirmed,
    /// No 2xx within the bound, or it was running but could not be asked.
    Unconfirmed,
    /// No run of it: nothing of it was running to stop.
    NotRunning,
}

impl RevocationStop {
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Confirmed => "confirmed",
            Self::Unconfirmed => "unconfirmed",
            Self::NotRunning => "not_running",
        }
    }
}

/// An end of grants told to their module (K1-6a-G §2.5.1): of `operation`
/// (`"*"`: every tool), at `ended_at`, for `reason`. `authorization_refs`
/// are the grants it ended; `None` when they could not be read, and the
/// module then stops by operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantEnd {
    pub module: String,
    pub operation: String,
    pub ended_at: String,
    pub reason: &'static str,
    pub authorization_refs: Option<Vec<String>>,
}

pub struct AgentModuleToolRuntime {
    supervisors: Arc<OnceLock<Arc<Supervisors>>>,
    operations: Arc<HashSet<(String, String)>>,
    request_ids: KernelRequestIds,
    /// Modules whose enable epoch this daemon has brought up to date
    /// (K1-6a-G §2.1 F3); a grant holds for no other.
    epoch_observed: std::sync::RwLock<HashSet<String>>,
    /// Runs a test puts in place of the supervisors' slots.
    #[cfg(test)]
    test_slots: std::sync::Mutex<HashMap<String, Arc<agent24_os_proto::drain::Current>>>,
}

impl AgentModuleToolRuntime {
    pub fn new(operations: HashSet<(String, String)>) -> Arc<Self> {
        Arc::new(Self {
            supervisors: Arc::new(OnceLock::new()),
            operations: Arc::new(operations),
            request_ids: KernelRequestIds::new_random(),
            epoch_observed: std::sync::RwLock::default(),
            #[cfg(test)]
            test_slots: std::sync::Mutex::default(),
        })
    }

    pub fn supervisors_slot(&self) -> Arc<OnceLock<Arc<Supervisors>>> {
        Arc::clone(&self.supervisors)
    }

    fn generation(&self, module: &str) -> Result<Arc<Generation>, ModuleToolCallError> {
        let current = self
            .slot(module)
            .ok_or(ModuleToolCallError::ModuleUnavailable)?;
        let generation = current.get();
        if generation.state() != DrainState::Running {
            return Err(ModuleToolCallError::ModuleUnavailable);
        }
        Ok(generation)
    }

    fn slot(&self, module: &str) -> Option<Arc<agent24_os_proto::drain::Current>> {
        #[cfg(test)]
        if let Some(current) = self.test_slots.lock().ok()?.get(module) {
            return Some(Arc::clone(current));
        }
        self.supervisors.get()?.running_slot(module)
    }

    /// Tells `end.module` that `end` ended its grants, and waits, at most
    /// `within`, for it to say it stopped what they started (K1-6a-G §2.5).
    /// A run that is starting, draining or stopping is still a run, so it is
    /// unconfirmed; not running means none can be alive
    /// (`Supervisors::surely_stopped`).
    pub async fn notify_revoked(&self, end: &GrantEnd, within: Duration) -> RevocationStop {
        let module = end.module.as_str();
        let Some(current) = self.slot(module) else {
            // Out of the list is not gone: a disabled run drains first.
            return match self.supervisors.get() {
                Some(supervisors) if !supervisors.surely_stopped(module) => {
                    RevocationStop::Unconfirmed
                }
                _ => RevocationStop::NotRunning,
            };
        };
        let generation = current.get();
        if generation.state() != DrainState::Running {
            return RevocationStop::Unconfirmed;
        }
        let namespace = agent24_domain::DomainOsManifest::declared_namespace(module);
        let mut body = serde_json::json!({
            "operation": end.operation,
            "revoked_at": end.ended_at,
            "reason": end.reason,
        });
        if let Some(refs) = &end.authorization_refs {
            body["authorization_refs"] = serde_json::json!(refs);
        }
        let request = KernelRequest {
            path: format!("{namespace}/_a24/authorization/revoked"),
            extra_headers: Vec::new(),
            body: Bytes::from(body.to_string()),
        };
        let limits = KernelLimits {
            total: within,
            max_response_bytes: MAX_TOOL_RESPONSE_BYTES,
        };
        match send_kernel_request(&generation, &self.request_ids, request, limits, None).await {
            Ok(response) if response.status.is_success() => RevocationStop::Confirmed,
            _ => RevocationStop::Unconfirmed,
        }
    }

    /// This start has seen `module`'s enablement and brought its enable
    /// epoch up to date (K1-6a-G §2.1 F3): its grants may hold from now on.
    pub fn epoch_observed(&self, module: &str) {
        if let Ok(mut observed) = self.epoch_observed.write() {
            observed.insert(module.to_owned());
        }
    }

    /// The current identity of `module`'s running package (K1-6a-G §2.2),
    /// read per lookup; not running, or no identity, is `None`. So is a
    /// module whose enable epoch this start has not brought up to date
    /// (§2.1 F3): an ephemeral daemon never does (F6).
    pub fn artifact_source(self: &Arc<Self>) -> agent24_policy::consent_gate::ArtifactSource {
        let runtime = Arc::clone(self);
        Arc::new(move |module: &str| {
            if !runtime
                .epoch_observed
                .read()
                .is_ok_and(|observed| observed.contains(module))
            {
                return None;
            }
            runtime
                .generation(module)
                .ok()
                .and_then(|generation| generation.artifact().map(str::to_owned))
        })
    }

    fn unavailable() -> ModuleToolCallError {
        ModuleToolCallError::Module {
            code: ModuleToolErrorCode::ModuleUnavailable,
            retryable: false,
            details: None,
            unknown_code: None,
        }
    }
}

#[async_trait]
impl ModuleToolRuntime for AgentModuleToolRuntime {
    async fn check_available(
        &self,
        module: &str,
        operation: &str,
    ) -> Result<(), ModuleToolCallError> {
        if !self
            .operations
            .contains(&(module.to_owned(), operation.to_owned()))
        {
            return Err(Self::unavailable());
        }
        self.generation(module).map(|_| ())
    }

    async fn invoke(
        &self,
        context: ModuleToolContext,
        arguments: serde_json::Map<String, serde_json::Value>,
        timeout: Duration,
        cancel: CancellationToken,
    ) -> Result<ModuleToolResult, ModuleToolCallError> {
        self.check_available(&context.module_id, &context.operation)
            .await?;
        let generation = self.generation(&context.module_id)?;
        // K1-6a-G §2.2: the call goes only to a run of the package the grant
        // was given for; the module may have restarted on another since the
        // lookup, which no consent change announces.
        if !serves_granted_package(&generation, context.expected_artifact.as_deref()) {
            return Err(stale_package());
        }
        let path = format!(
            "/api/v1/{}/_a24/tools/{}",
            context.module_id, context.operation
        );
        let body = serde_json::to_vec(&serde_json::json!({
            "context": context,
            "arguments": arguments,
        }))
        .map_err(|_| ModuleToolCallError::InvalidResult)?;
        let request = KernelRequest {
            path,
            extra_headers: Vec::new(),
            body: Bytes::from(body),
        };
        let (_, result) = send_module_tool_request(
            &generation,
            &self.request_ids,
            request,
            KernelLimits {
                total: timeout,
                max_response_bytes: MAX_TOOL_RESPONSE_BYTES,
            },
            &cancel,
        )
        .await?;
        Ok(result)
    }
}

/// Whether `generation` runs the package a grant was given for: both
/// identities known and equal.
fn serves_granted_package(generation: &Generation, expected: Option<&str>) -> bool {
    expected.is_some() && generation.artifact() == expected
}

/// What a call gets when the running package is not the one granted: the
/// host's own cancellation, before anything is sent (K1-6a-G §2.5), which
/// the caller reports as `permission_denied`.
fn stale_package() -> ModuleToolCallError {
    ModuleToolCallError::Cancelled
}

/// Module id of the Documenting OS, as adopted in ADR-DOC-01
/// (`docs/documenting/adr/ADR-DOC-01-placement-and-integration.md`). Gate 5
/// (jason 2026-10-08 ruling #5) withholds this module's tools whenever a remote
/// model tier exists, until K1-6a and K1-6b are both accepted.
const DOCUMENTS_MODULE_ID: &str = "documents";

/// Whether gate 5 treats `module` as a document-class module.
fn is_document_class_module(module: &str) -> bool {
    module == DOCUMENTS_MODULE_ID
}

/// Whether a remote model tier exists right now (K1-6a-G §2.4): read on
/// every advert and every call, never a startup snapshot. `None` means the
/// tier configuration could not be read, which gate 5 treats as a remote
/// tier being present.
pub type RemoteTierProbe = Arc<dyn Fn() -> Option<bool> + Send + Sync>;

pub struct AgentModuleToolAdvertView {
    runtime: Arc<AgentModuleToolRuntime>,
    authorizations: HashMap<(String, String), Arc<dyn ModuleToolAuthorization>>,
    remote_tier: RemoteTierProbe,
}

impl AgentModuleToolAdvertView {
    pub fn new(
        runtime: Arc<AgentModuleToolRuntime>,
        authorizations: HashMap<(String, String), Arc<dyn ModuleToolAuthorization>>,
        remote_tier: RemoteTierProbe,
    ) -> Arc<Self> {
        Arc::new(Self {
            runtime,
            authorizations,
            remote_tier,
        })
    }
}

#[async_trait]
impl ModuleToolAdvertView for AgentModuleToolAdvertView {
    async fn module_ready(&self, module: &str) -> bool {
        self.runtime.generation(module).is_ok()
    }

    async fn operation_available(&self, module: &str, operation: &str) -> bool {
        self.runtime
            .operations
            .contains(&(module.to_owned(), operation.to_owned()))
    }

    async fn has_current_consent(&self, module: &str, operation: &str) -> bool {
        match self
            .authorizations
            .get(&(module.to_owned(), operation.to_owned()))
        {
            Some(authorization) => authorization.has_current_consent(module, operation).await,
            None => false,
        }
    }

    async fn outbound_gate_ready(&self, module: &str, operation: &str) -> bool {
        self.operation_available(module, operation).await
    }

    async fn blocked_by_remote_tier_guard(&self, module: &str, _operation: &str) -> bool {
        is_document_class_module(module) && (self.remote_tier)().unwrap_or(true)
    }
}

/// The daemon's clock for consent checks. Milliseconds: a grant's remaining
/// time is measured from it (K1-6a-G §2.5), and whole seconds would end a
/// call up to a second late. No clock reads as no time, expiring the grant.
pub(crate) fn consent_clock() -> String {
    agent24_core::util::now_iso8601_millis().unwrap_or_default()
}

/// A module tool's authorization against the consent store (K1-6a-G §2.5):
/// it checks grants in `store` and hears of every change committed to that
/// same store, so a revocation reaches a call in flight. The daemon and the
/// tests both build it here.
pub(crate) fn store_authorization(
    store: &agent24_store::Store,
    request: agent24_policy::consent_gate::ConsentGateRequest,
    now: Arc<dyn Fn() -> String + Send + Sync>,
    artifact: agent24_policy::consent_gate::ArtifactSource,
) -> agent24_policy::consent_gate::ModuleConsentAuthorization {
    agent24_policy::consent_gate::ModuleConsentAuthorization::new(
        Arc::new(agent24_policy::consent_gate::StoreConsentGate::new(
            store.clone(),
        )),
        request,
        now,
    )
    .with_consent_changes(store.clone())
    .with_artifact_source(artifact)
}

/// What this start sees of each of `modules` (K1-6a-G §2.1 F3): recorded,
/// advancing an enable epoch where it changed, and only then are the
/// module's grants let hold. A module this fails for keeps none.
pub(crate) async fn observe_enable_epochs<'a>(
    store: &agent24_store::Store,
    runtime: &AgentModuleToolRuntime,
    config: &crate::os_config::OsConfig,
    path: &std::path::Path,
    modules: impl IntoIterator<Item = &'a str>,
) {
    let modules: std::collections::BTreeSet<&str> = modules.into_iter().collect();
    for module in modules {
        let entry = config.observed_entry(path, module);
        match store
            .observe_module_enablement(module, config.is_enabled(module), &entry)
            .await
        {
            Ok(_) => runtime.epoch_observed(module),
            Err(error) => {
                tracing::error!(module, %error, "enable epoch not observed; its grants stay stale");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn gate_five_recognises_the_adopted_documents_module_id() {
        // ADR-DOC-01 adopted "documents"; the earlier "documenting" literal
        // never matched any real module, leaving gate 5 dead (#790 review).
        assert!(is_document_class_module("documents"));
        assert!(!is_document_class_module("documenting"));
        assert!(!is_document_class_module("sin90"));
    }
    use agent24_domain::tool::{ModuleToolCallError, ModuleToolResult};
    use agent24_domain::{EgressGate, EgressResource};
    use agent24_policy::consent_gate::{
        ConsentGate, ConsentGateRequest, ModuleConsentAuthorization, StoreConsentGate,
    };
    use agent24_protocol::{RiskClass, ToolInfo};
    use agent24_store::{
        ActorRef, AttemptId, AuditTimestamp, AuthorizationRef, ConsentSource, HostRiskLevel,
        ModuleId, ModuleToolAuditEvent, ModuleToolAuditRelation, OperationId, RunId, Store,
        ToolCallId, ToolPermissionSummary,
    };
    use agent24_tools::{
        ApprovalGate, GateDecision, ModuleTool, ModuleToolAuthorization, RiskOverrides, Tool,
        ToolContext, ToolError, ToolRegistry,
    };
    use async_trait::async_trait;
    use serde_json::{Map, Value};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct FakeModule {
        running: AtomicBool,
        calls: AtomicUsize,
        failure: std::sync::Mutex<Option<ModuleToolCallError>>,
        pending: AtomicBool,
        wait_for_cancel: AtomicBool,
        slow_check: AtomicBool,
        unresponsive_check: AtomicBool,
        entered: tokio::sync::Notify,
        /// What `TestAdvertView`'s gate 5 reads, per call.
        remote_tier: AtomicBool,
        /// Notified as `check_available` starts.
        check_entered: tokio::sync::Notify,
        /// Run once, synchronously, as `check_available` / `invoke` is
        /// first polled: whatever they do lands inside that one poll, so
        /// the wait racing it is not polled again first.
        on_check: Hook,
        on_invoke: Hook,
        /// `(authorized_resources, authorization_ref)` of each dispatch.
        dispatched: std::sync::Mutex<Vec<(Vec<String>, String)>>,
        /// `(expected_artifact, the context as sent on the wire)`.
        contexts: std::sync::Mutex<Vec<(Option<String>, String)>>,
    }

    type Hook = std::sync::Mutex<Option<Box<dyn FnOnce() + Send>>>;

    fn hook(hook: &Hook, run: impl FnOnce() + Send + 'static) {
        *hook.lock().unwrap() = Some(Box::new(run));
    }

    fn run_hook(hook: &Hook) {
        let run = hook.lock().unwrap().take();
        if let Some(run) = run {
            run();
        }
    }

    #[async_trait]
    impl ModuleToolRuntime for FakeModule {
        async fn check_available(&self, _: &str, _: &str) -> Result<(), ModuleToolCallError> {
            self.check_entered.notify_one();
            run_hook(&self.on_check);
            if self.unresponsive_check.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
            if self.slow_check.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
            if self.running.load(Ordering::SeqCst) {
                Ok(())
            } else {
                Err(ModuleToolCallError::ModuleUnavailable)
            }
        }

        async fn invoke(
            &self,
            context: ModuleToolContext,
            _: Map<String, Value>,
            _: Duration,
            cancel: CancellationToken,
        ) -> Result<ModuleToolResult, ModuleToolCallError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.contexts.lock().unwrap().push((
                context.expected_artifact.clone(),
                serde_json::to_string(&context).unwrap(),
            ));
            self.dispatched
                .lock()
                .unwrap()
                .push((context.authorized_resources, context.authorization_ref));
            self.entered.notify_one();
            run_hook(&self.on_invoke);
            if self.wait_for_cancel.load(Ordering::SeqCst) {
                cancel.cancelled().await;
                return Err(self
                    .failure
                    .lock()
                    .unwrap()
                    .clone()
                    .unwrap_or(ModuleToolCallError::Cancelled));
            }
            if let Some(error) = self.failure.lock().unwrap().clone() {
                return Err(error);
            }
            if self.pending.load(Ordering::SeqCst) {
                return Ok(ModuleToolResult::Pending {
                    job: serde_json::json!({"id": "job-1"}),
                });
            }
            Ok(ModuleToolResult::Completed {
                payload: serde_json::json!({"ok": true}),
                replayed: false,
            })
        }
    }

    struct TestAdvertView {
        authorization: Arc<dyn ModuleToolAuthorization>,
        running: Arc<FakeModule>,
        outbound_ready: AtomicBool,
    }

    #[async_trait]
    impl ModuleToolAdvertView for TestAdvertView {
        async fn module_ready(&self, _: &str) -> bool {
            self.running.running.load(Ordering::SeqCst)
        }
        async fn operation_available(&self, _: &str, _: &str) -> bool {
            true
        }
        async fn has_current_consent(&self, module: &str, operation: &str) -> bool {
            self.authorization
                .has_current_consent(module, operation)
                .await
        }
        async fn outbound_gate_ready(&self, _: &str, _: &str) -> bool {
            self.outbound_ready.load(Ordering::SeqCst)
        }
        async fn blocked_by_remote_tier_guard(&self, _: &str, _: &str) -> bool {
            self.running.remote_tier.load(Ordering::SeqCst)
        }
    }

    struct CountingApproval {
        calls: AtomicUsize,
        deny: AtomicBool,
    }

    struct RelaxModuleRisk;

    struct PausingAuthorization {
        entered: tokio::sync::Notify,
    }

    struct RetainingAuthorization {
        entered_second: tokio::sync::Notify,
        calls: AtomicUsize,
        retained: std::sync::Mutex<Option<ToolContext>>,
    }

    #[async_trait]
    impl ModuleToolAuthorization for PausingAuthorization {
        async fn authorize(
            &self,
            _: &str,
            _: &str,
            _: &ToolContext,
        ) -> Result<agent24_tools::ModuleToolGrantContext, ModuleToolCallError> {
            self.entered.notify_one();
            std::future::pending::<()>().await;
            Err(ModuleToolCallError::Cancelled)
        }
    }

    #[async_trait]
    impl ModuleToolAuthorization for RetainingAuthorization {
        async fn authorize(
            &self,
            _: &str,
            _: &str,
            ctx: &ToolContext,
        ) -> Result<agent24_tools::ModuleToolGrantContext, ModuleToolCallError> {
            *self.retained.lock().unwrap() = Some(ctx.clone());
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                Ok(agent24_tools::ModuleToolGrantContext {
                    authorized_resources: vec![],
                    authorization_ref: "grant:retained".to_owned(),
                    per_call_approval: false,
                    valid_for: None,
                    artifact: None,
                })
            } else {
                self.entered_second.notify_one();
                std::future::pending::<()>().await;
                Err(ModuleToolCallError::Cancelled)
            }
        }

        // Its second lookup never answers; these tests are about which
        // lookup a call makes, not the check after a call returns.
        async fn still_current(&self, _: &str, _: &str, _: &ToolContext, _: &str) -> bool {
            true
        }
    }

    impl RiskOverrides for RelaxModuleRisk {
        fn resolve(&self, _: &str) -> Option<RiskClass> {
            Some(RiskClass::Read)
        }
    }

    #[async_trait]
    impl ApprovalGate for CountingApproval {
        async fn check(
            &self,
            _: &ToolInfo,
            _: &ToolContext,
            _: &Map<String, Value>,
            _: Option<&str>,
            _: &CancellationToken,
        ) -> GateDecision {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.deny.load(Ordering::SeqCst) {
                GateDecision::Deny("user refused".to_owned())
            } else {
                GateDecision::Allow
            }
        }
    }

    /// The package the fixture's grants name, and the one "running".
    const FIXTURE_PACKAGE: &str = "sha256:fixture-package";

    fn fixture_scope() -> agent24_store::GrantScope {
        agent24_store::GrantScope {
            resources: Vec::new(),
            artifact: Some(FIXTURE_PACKAGE.to_owned()),
        }
    }

    fn fixture_package() -> agent24_policy::consent_gate::ArtifactSource {
        Arc::new(|_: &str| Some(FIXTURE_PACKAGE.to_owned()))
    }

    fn summary() -> ToolPermissionSummary {
        ToolPermissionSummary::new(
            "fake_module",
            "write_local",
            "1.0.0",
            ConsentSource::FirstParty,
            Some("host-approved module operation".to_owned()),
            Some("host-approved local write".to_owned()),
            None,
            HostRiskLevel::High,
        )
    }

    async fn fixture(
        risk: RiskClass,
        granted: bool,
        running: bool,
    ) -> (ToolRegistry, Arc<FakeModule>, Arc<CountingApproval>, Store) {
        let (registry, runtime, approvals, store, _) =
            fixture_using(risk, granted, running, true, None, None).await;
        (registry, runtime, approvals, store)
    }

    async fn fixture_with_outbound(
        risk: RiskClass,
        granted: bool,
        running: bool,
        outbound_ready: bool,
    ) -> (ToolRegistry, Arc<FakeModule>, Arc<CountingApproval>, Store) {
        let (registry, runtime, approvals, store, _) =
            fixture_using(risk, granted, running, outbound_ready, None, None).await;
        (registry, runtime, approvals, store)
    }

    async fn fixture_using(
        risk: RiskClass,
        granted: bool,
        running: bool,
        outbound_ready: bool,
        supplied_store: Option<Store>,
        supplied_authorization: Option<Arc<dyn ModuleToolAuthorization>>,
    ) -> (
        ToolRegistry,
        Arc<FakeModule>,
        Arc<CountingApproval>,
        Store,
        Arc<ModuleTool>,
    ) {
        let store = match supplied_store {
            Some(store) => store,
            None => Store::open_memory().await.unwrap(),
        };
        let summary = summary();
        if granted {
            store
                .grant_module_consent_scoped(
                    &summary,
                    &fixture_scope(),
                    "2026-10-08T00:00:00Z",
                    "2026-11-08T00:00:00Z",
                )
                .await
                .unwrap();
        }
        let request = ConsentGateRequest {
            module: summary.module.clone(),
            op: summary.op.clone(),
            module_version: summary.module_version.clone(),
            current_scope_fingerprint: summary.scope_fingerprint(),
            risk,
            org_restricted: false,
            source_restricted: false,
            reversible_draft_declared: false,
            draft_verification: None,
            current_artifact: None,
        };
        let default_authorization: Arc<dyn ModuleToolAuthorization> =
            Arc::new(super::store_authorization(
                &store,
                request,
                Arc::new(|| "2026-10-08T00:00:01Z".to_owned()),
                fixture_package(),
            ));
        let authorization = supplied_authorization.unwrap_or(default_authorization);
        let runtime = Arc::new(FakeModule {
            running: AtomicBool::new(running),
            calls: AtomicUsize::new(0),
            failure: std::sync::Mutex::new(None),
            pending: AtomicBool::new(false),
            wait_for_cancel: AtomicBool::new(false),
            slow_check: AtomicBool::new(false),
            unresponsive_check: AtomicBool::new(false),
            entered: tokio::sync::Notify::new(),
            remote_tier: AtomicBool::new(false),
            check_entered: tokio::sync::Notify::new(),
            on_check: Hook::default(),
            on_invoke: Hook::default(),
            dispatched: std::sync::Mutex::default(),
            contexts: std::sync::Mutex::default(),
        });
        let test_view = Arc::new(TestAdvertView {
            authorization: Arc::clone(&authorization),
            running: Arc::clone(&runtime),
            outbound_ready: AtomicBool::new(outbound_ready),
        });
        let view: Arc<dyn ModuleToolAdvertView> = test_view;
        let tool = Arc::new(
            ModuleTool::new(
                "fake_module",
                "write_local",
                "fake module tool",
                serde_json::json!({"type":"object"}),
                Duration::from_secs(2),
                Duration::from_secs(1),
                risk,
                view,
                authorization,
                runtime.clone(),
                store.clone(),
                "agent24d",
            )
            .unwrap(),
        );
        let approvals = Arc::new(CountingApproval {
            calls: AtomicUsize::new(0),
            deny: AtomicBool::new(false),
        });
        let mut registry = ToolRegistry::new().with_risk_overrides(Arc::new(RelaxModuleRisk));
        if risk.requires_approval() {
            registry = registry.with_gate(approvals.clone());
        }
        registry
            .register_module_tool(tool.clone())
            .expect("fixture registers a unique module tool name");
        (registry, runtime, approvals, store, tool)
    }

    fn context() -> ToolContext {
        ToolContext::legacy("run", None, None, "tool-call")
    }

    #[tokio::test]
    async fn granted_fake_module_is_advertised_and_callable() {
        let (registry, runtime, _, store) = fixture(RiskClass::Read, true, true).await;
        assert_eq!(registry.live_adverts().await.len(), 1);
        let mut arguments = Map::new();
        arguments.insert(
            "credential".to_owned(),
            Value::String("do-not-audit-this-secret".to_owned()),
        );
        let result = registry
            .dispatch(
                "fake_module.write_local",
                &context(),
                &arguments,
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.contains("\"ok\":true"));
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 1);
        let audit = store.list_audit().await.unwrap();
        assert_eq!(audit.len(), 2, "pre-dispatch and terminal audit required");
        assert_eq!(audit[0].action, "k1.module_tool.pre_dispatch");
        assert_eq!(audit[1].detail["terminal"]["result"], "success");
        assert_eq!(audit[0].detail["pre_dispatch"]["actor"], "agent24d");
        assert_eq!(audit[0].detail["pre_dispatch"]["run_id"], "run");
        assert_eq!(audit[0].detail["pre_dispatch"]["module_id"], "fake_module");
        let audit_text =
            serde_json::to_string(&audit.iter().map(|entry| &entry.detail).collect::<Vec<_>>())
                .unwrap();
        assert!(!audit_text.contains("do-not-audit-this-secret"));
    }

    /// K1-6a-G §2.2, §4 case 9: the module gets the grant's resource set as
    /// recorded and its `ga1:` reference; the audit names the set only by
    /// count and digest. A grant without resources dispatches an empty list.
    #[tokio::test]
    async fn a_grant_dispatches_its_resources_and_audits_only_their_digest() {
        let store = Store::open_memory().await.unwrap();
        let (registry, runtime, _, store, _) =
            fixture_using(RiskClass::Read, true, true, true, Some(store), None).await;
        let call = || async {
            registry
                .dispatch(
                    "fake_module.write_local",
                    &context(),
                    &Map::new(),
                    &CancellationToken::new(),
                )
                .await
                .unwrap()
        };
        call().await;
        let scoped = store
            .grant_module_consent_scoped(
                &summary(),
                &agent24_store::GrantScope {
                    resources: vec!["doc:doc_02".to_owned(), "doc:doc_01".to_owned()],
                    artifact: Some(FIXTURE_PACKAGE.to_owned()),
                },
                "2026-10-08T00:00:00Z",
                "2026-11-08T00:00:00Z",
            )
            .await
            .unwrap();
        call().await;

        let dispatched = runtime.dispatched.lock().unwrap().clone();
        assert_eq!(dispatched.len(), 2);
        assert!(dispatched[0].0.is_empty());
        assert!(dispatched[0].1.starts_with("ga1:"));
        assert_eq!(dispatched[1].0, ["doc:doc_01", "doc:doc_02"]);
        assert_eq!(dispatched[1].1, scoped.authorization_ref());
        assert_ne!(dispatched[0].1, dispatched[1].1);

        let rows = store.list_audit().await.unwrap();
        let pre: Vec<_> = rows
            .iter()
            .filter_map(|row| row.detail.get("pre_dispatch"))
            .collect();
        assert_eq!(pre.len(), 2);
        assert_eq!(pre[0]["resource_ref"], agent24_store::resource_set_ref(&[]));
        assert_eq!(
            pre[1]["resource_ref"],
            agent24_store::resource_set_ref(&scoped.resources)
        );
        assert_eq!(pre[1]["authorization_ref"], scoped.authorization_ref());
        let audit_text =
            serde_json::to_string(&rows.iter().map(|row| &row.detail).collect::<Vec<_>>()).unwrap();
        assert!(!audit_text.contains("doc_01"), "{audit_text}");
    }

    /// K1-6a-G §4 case 10: a stored set that is malformed or over the cap is
    /// refused at dispatch, audited as denied, and never trimmed into one
    /// that would pass; a set at the cap goes through whole.
    #[tokio::test]
    async fn a_stored_resource_set_that_is_malformed_or_too_large_is_not_dispatched() {
        let (registry, runtime, _, store) = fixture(RiskClass::Read, true, true).await;
        let set_resources = |resources: Vec<String>| {
            let store = store.clone();
            async move {
                sqlx::query("UPDATE module_consents SET resources = ?")
                    .bind(serde_json::to_string(&resources).unwrap())
                    .execute(agent24_store::test_hooks::pool(&store))
                    .await
                    .unwrap();
            }
        };
        let call = || async {
            registry
                .dispatch(
                    "fake_module.write_local",
                    &context(),
                    &Map::new(),
                    &CancellationToken::new(),
                )
                .await
        };
        let many = |n: usize| (0..n).map(|i| format!("doc:d{i}")).collect::<Vec<_>>();

        for bad in [
            vec!["doc:a".to_owned(), "not a resource".to_owned()],
            vec!["doc:a:b".to_owned()],
            many(agent24_store::MAX_GRANT_RESOURCES + 1),
        ] {
            set_resources(bad.clone()).await;
            let error = call().await.unwrap_err();
            assert!(
                error.to_string().contains("permission_denied"),
                "{bad:?}: {error}"
            );
            assert_eq!(runtime.calls.load(Ordering::SeqCst), 0, "{bad:?}");
            let rows = store.list_audit().await.unwrap();
            let last = &rows[rows.len() - 1];
            assert_eq!(last.detail["terminal"]["result"], "denied", "{bad:?}");
            let pre = &rows[rows.len() - 2].detail["pre_dispatch"];
            assert!(pre.get("resource_ref").is_none_or(Value::is_null), "{pre}");
        }

        set_resources(many(agent24_store::MAX_GRANT_RESOURCES)).await;
        call().await.unwrap();
        let dispatched = runtime.dispatched.lock().unwrap().clone();
        assert_eq!(dispatched.len(), 1);
        assert_eq!(dispatched[0].0.len(), agent24_store::MAX_GRANT_RESOURCES);
    }

    /// A call that asks for approval is refused before any card when its
    /// stored resource set could not be dispatched, as #1130 L3 has it for
    /// every refusal the dispatch would make.
    #[tokio::test]
    async fn a_resource_set_that_cannot_be_dispatched_shows_no_card() {
        let (registry, runtime, approvals, store) =
            fixture(RiskClass::WriteLocal, true, true).await;
        sqlx::query("UPDATE module_consents SET resources = ?")
            .bind(r#"["doc:a:b"]"#)
            .execute(agent24_store::test_hooks::pool(&store))
            .await
            .unwrap();
        let error = registry
            .dispatch(
                "fake_module.write_local",
                &context(),
                &Map::new(),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("permission_denied"), "{error}");
        assert_eq!(approvals.calls.load(Ordering::SeqCst), 0);
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 0);
        let rows = store.list_audit().await.unwrap();
        assert_eq!(rows.len(), 2);
        assert!(
            rows[0].detail["pre_dispatch"]["authorization_ref"]
                .as_str()
                .unwrap()
                .starts_with("ga1:")
        );
        assert_eq!(rows[1].detail["terminal"]["result"], "denied");
    }

    /// K1-6a-G §2.2, §4 case 6: the module gets the package the grant was
    /// given for; a grant for another package (same version) is stale —
    /// refused and audited before the module, and not advertised.
    #[tokio::test]
    async fn a_grant_for_another_package_is_stale_and_never_dispatched() {
        let store = Store::open_memory().await.unwrap();
        store
            .grant_module_consent_scoped(
                &summary(),
                &agent24_store::GrantScope {
                    resources: Vec::new(),
                    artifact: Some("sha256:the-package-the-user-saw".to_owned()),
                },
                "2026-10-08T00:00:00Z",
                "2026-11-08T00:00:00Z",
            )
            .await
            .unwrap();
        let (registry, runtime, _, store, _) =
            fixture_using(RiskClass::Read, false, true, true, Some(store), None).await;
        assert!(registry.live_adverts().await.is_empty());
        let error = registry
            .dispatch(
                "fake_module.write_local",
                &context(),
                &Map::new(),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("permission_denied"), "{error}");
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 0);
        let rows = store.list_audit().await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].detail["terminal"]["result"], "denied");
    }

    /// The runtime holds the run it sends to against the granted package:
    /// both known and equal, never `None` on either side.
    #[test]
    fn only_a_run_of_the_granted_package_is_served() {
        let run = |artifact: Option<&str>| {
            agent24_os_proto::drain::Generation::serving_package(
                std::path::PathBuf::from("/nonexistent/upstream.sock"),
                artifact.map(str::to_owned),
            )
        };
        assert!(super::serves_granted_package(
            &run(Some("sha256:a")),
            Some("sha256:a")
        ));
        assert!(!super::serves_granted_package(
            &run(Some("sha256:b")),
            Some("sha256:a")
        ));
        assert!(!super::serves_granted_package(&run(None), Some("sha256:a")));
        assert!(!super::serves_granted_package(&run(Some("sha256:a")), None));
        assert!(!super::serves_granted_package(&run(None), None));
    }

    /// K1-6a-G §2.5: a revocation is told to the running module, at the
    /// module's reserved path; only its 2xx within the bound confirms the
    /// stop. A run still starting, or out of the list but still stopping,
    /// is unconfirmed; only a module with no run has nothing running.
    #[tokio::test]
    async fn a_revocation_is_told_to_the_running_module() {
        let sock = std::path::PathBuf::from(format!(
            "/tmp/a24-revoke-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        let told = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
        let answer = Arc::new(std::sync::Mutex::new(axum::http::StatusCode::OK));
        let app = axum::Router::new().route(
            "/api/v1/fake_module/_a24/authorization/revoked",
            axum::routing::post({
                let (told, answer) = (Arc::clone(&told), Arc::clone(&answer));
                move |axum::Json(body): axum::Json<Value>| async move {
                    told.lock().unwrap().push(body);
                    let status = *answer.lock().unwrap();
                    if status == axum::http::StatusCode::ACCEPTED {
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                    status
                }
            }),
        );
        tokio::spawn(async move { axum::serve(listener, app).await });
        let runtime = AgentModuleToolRuntime::new(HashSet::new());
        let end = |authorization_refs| GrantEnd {
            module: "fake_module".to_owned(),
            operation: "*".to_owned(),
            ended_at: "2026-10-24T00:00:00Z".to_owned(),
            reason: "epoch",
            authorization_refs,
        };
        let told_refs = end(Some(vec!["ga1:a".to_owned()]));
        let notify = |within| runtime.notify_revoked(&told_refs, within);
        let within = Duration::from_secs(2);
        assert_eq!(notify(within).await, RevocationStop::NotRunning);
        // Out of the list but still draining from a disable: not yet gone.
        let supervisors = Arc::new(Supervisors::default());
        assert!(
            runtime
                .supervisors_slot()
                .set(Arc::clone(&supervisors))
                .is_ok()
        );
        let (stop, status) =
            tokio::sync::watch::channel(agent24_os_proto::supervisor::Status::Stopping);
        supervisors.mark_disabled("fake_module", status);
        assert_eq!(notify(within).await, RevocationStop::Unconfirmed);
        stop.send_replace(agent24_os_proto::supervisor::Status::Stopped);
        assert_eq!(notify(within).await, RevocationStop::NotRunning);
        let generation = agent24_os_proto::drain::Generation::serving_package(sock.clone(), None);
        runtime.test_slots.lock().unwrap().insert(
            "fake_module".to_owned(),
            agent24_os_proto::drain::Current::new(Arc::clone(&generation)),
        );
        // A run still starting is a run: not told, and not gone.
        assert_eq!(notify(within).await, RevocationStop::Unconfirmed);
        assert!(told.lock().unwrap().is_empty());
        assert!(generation.ready());
        assert_eq!(notify(within).await, RevocationStop::Confirmed);
        assert_eq!(
            told.lock().unwrap()[0],
            serde_json::json!({
                "operation": "*",
                "revoked_at": "2026-10-24T00:00:00Z",
                "reason": "epoch",
                "authorization_refs": ["ga1:a"],
            })
        );
        // References not read: none sent, and the module stops by operation.
        assert_eq!(
            runtime.notify_revoked(&end(None), within).await,
            RevocationStop::Confirmed
        );
        assert_eq!(
            told.lock().unwrap()[1],
            serde_json::json!({
                "operation": "*",
                "revoked_at": "2026-10-24T00:00:00Z",
                "reason": "epoch",
            })
        );
        *answer.lock().unwrap() = axum::http::StatusCode::INTERNAL_SERVER_ERROR;
        assert_eq!(notify(within).await, RevocationStop::Unconfirmed);
        // Answered only after the bound: unconfirmed, and no later.
        *answer.lock().unwrap() = axum::http::StatusCode::ACCEPTED;
        let started = std::time::Instant::now();
        assert_eq!(
            notify(Duration::from_millis(300)).await,
            RevocationStop::Unconfirmed
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        let _ = std::fs::remove_file(&sock);
    }

    /// K1-6a-G §2.1 F3/F6: no grant holds for a module whose enable epoch
    /// this start has not brought up to date; a store that cannot record
    /// what the start sees leaves it so. Once one has, the running package
    /// is read as before.
    #[tokio::test]
    async fn grants_hold_only_once_this_start_observed_the_module() {
        let runtime = AgentModuleToolRuntime::new(HashSet::new());
        let generation = agent24_os_proto::drain::Generation::serving_package(
            std::path::PathBuf::from("/nonexistent/upstream.sock"),
            Some("sha256:a".to_owned()),
        );
        assert!(generation.ready());
        runtime.test_slots.lock().unwrap().insert(
            "fake_module".to_owned(),
            agent24_os_proto::drain::Current::new(generation),
        );
        let source = runtime.artifact_source();
        assert_eq!(source("fake_module"), None);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("os.json");
        let config = crate::os_config::OsConfig::load(&path).unwrap();
        let broken = Store::open_memory().await.unwrap();
        sqlx::query("DROP TABLE module_enable_epochs")
            .execute(agent24_store::test_hooks::pool(&broken))
            .await
            .unwrap();
        observe_enable_epochs(&broken, &runtime, &config, &path, ["fake_module"]).await;
        assert_eq!(source("fake_module"), None);
        let store = Store::open_memory().await.unwrap();
        observe_enable_epochs(&store, &runtime, &config, &path, ["fake_module"]).await;
        assert_eq!(source("fake_module").as_deref(), Some("sha256:a"));
        assert_eq!(source("another_module"), None);
    }

    /// The runtime itself turns away a call for a package other than the one
    /// running, before anything is sent: a grant for A never reaches B.
    #[tokio::test]
    async fn invoke_refuses_a_run_of_another_package_before_sending() {
        let runtime = AgentModuleToolRuntime::new(HashSet::from([(
            "fake_module".to_owned(),
            "write_local".to_owned(),
        )]));
        let generation = agent24_os_proto::drain::Generation::serving_package(
            std::path::PathBuf::from("/nonexistent/upstream.sock"),
            Some("sha256:b".to_owned()),
        );
        assert!(generation.ready());
        runtime.test_slots.lock().unwrap().insert(
            "fake_module".to_owned(),
            agent24_os_proto::drain::Current::new(generation),
        );
        let invoke = |expected: Option<&str>| {
            runtime.invoke(
                ModuleToolContext {
                    run_id: "run".to_owned(),
                    session_id: None,
                    tool_call_id: "call".to_owned(),
                    module_id: "fake_module".to_owned(),
                    operation: "write_local".to_owned(),
                    authorized_resources: Vec::new(),
                    authorization_ref: "ga1:x".to_owned(),
                    expected_artifact: expected.map(str::to_owned),
                },
                Map::new(),
                Duration::from_secs(1),
                CancellationToken::new(),
            )
        };
        for expected in [Some("sha256:a"), None] {
            assert!(
                matches!(invoke(expected).await, Err(ModuleToolCallError::Cancelled)),
                "{expected:?}"
            );
        }
        // The package granted is the one running: the call goes out, and
        // fails only on the socket that is not there.
        assert!(!matches!(
            invoke(Some("sha256:b")).await,
            Err(ModuleToolCallError::Cancelled)
        ));
    }

    /// A runtime's own refusal, with the caller not cancelling, is the grant
    /// no longer holding: audited `cancelled`, `permission_denied` to the
    /// model, whatever the check after the call says.
    #[tokio::test]
    async fn a_runtime_refusal_reaches_the_model_as_permission_denied() {
        let (registry, runtime, _, store) = fixture(RiskClass::Read, true, true).await;
        *runtime.failure.lock().unwrap() = Some(ModuleToolCallError::Cancelled);
        let error = registry
            .dispatch(
                "fake_module.write_local",
                &context(),
                &Map::new(),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("permission_denied"), "{error}");
        let rows = store.list_audit().await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].detail["terminal"]["result"], "cancelled");
    }

    /// A dispatch carries the granted package to the runtime, off the wire:
    /// the module never sees it.
    #[tokio::test]
    async fn the_granted_package_reaches_the_runtime_but_not_the_module() {
        let (registry, runtime, _, _) = fixture(RiskClass::Read, true, true).await;
        registry
            .dispatch(
                "fake_module.write_local",
                &context(),
                &Map::new(),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        let seen = runtime.contexts.lock().unwrap().clone();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0.as_deref(), Some(FIXTURE_PACKAGE));
        assert!(!seen[0].1.contains("expected_artifact"), "{}", seen[0].1);
        assert!(!seen[0].1.contains(FIXTURE_PACKAGE), "{}", seen[0].1);
    }

    #[tokio::test]
    async fn missing_consent_is_not_advertised_and_never_reaches_fake_module() {
        let (registry, runtime, _, store) = fixture(RiskClass::Read, false, true).await;
        assert!(registry.live_adverts().await.is_empty());
        let error = registry
            .dispatch(
                "fake_module.write_local",
                &context(),
                &Map::new(),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("permission_denied"));
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 0);
        let rows = store.list_audit().await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].detail["terminal"]["result"], "denied");
        assert_eq!(
            rows[0].detail["pre_dispatch"]["authorization_ref"],
            "denied:no_grant"
        );
    }

    #[tokio::test]
    async fn revoked_consent_is_audited_as_denied_with_revoked_reason() {
        let (registry, runtime, _, store) = fixture(RiskClass::Read, true, true).await;
        store
            .revoke_module_consent(
                "fake_module",
                Some("write_local"),
                "2026-10-08T00:00:00.500Z",
            )
            .await
            .unwrap();

        let error = registry
            .dispatch(
                "fake_module.write_local",
                &context(),
                &Map::new(),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("permission_denied"));
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 0);
        let rows = store.list_audit().await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0].detail["pre_dispatch"]["authorization_ref"],
            "denied:revoked"
        );
        assert_eq!(rows[1].detail["terminal"]["result"], "denied");
    }

    #[tokio::test]
    async fn write_local_module_tool_asks_existing_approval_gate_every_call() {
        let (registry, runtime, approvals, _) = fixture(RiskClass::WriteLocal, true, true).await;
        assert_eq!(registry.live_adverts().await.len(), 1);
        for _ in 0..2 {
            registry
                .dispatch(
                    "fake_module.write_local",
                    &context(),
                    &Map::new(),
                    &CancellationToken::new(),
                )
                .await
                .unwrap();
        }
        assert_eq!(approvals.calls.load(Ordering::SeqCst), 2);
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn stopped_module_tool_returns_module_unavailable_without_dispatch() {
        let (registry, runtime, _, _) = fixture(RiskClass::Read, true, false).await;
        assert!(registry.live_adverts().await.is_empty());
        let error = registry
            .dispatch(
                "fake_module.write_local",
                &context(),
                &Map::new(),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("module_unavailable"));
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn module_tool_records_each_execution_terminal_code() {
        let cases = [
            (ModuleToolCallError::ModuleUnavailable, "failed"),
            (ModuleToolCallError::Timeout, "timeout"),
            (ModuleToolCallError::Cancelled, "cancelled"),
            (ModuleToolCallError::ResultUnknown, "result_unknown"),
        ];
        for (failure, expected) in cases {
            let (registry, runtime, _, store) = fixture(RiskClass::Read, true, true).await;
            *runtime.failure.lock().unwrap() = Some(failure);
            assert!(
                registry
                    .dispatch(
                        "fake_module.write_local",
                        &context(),
                        &Map::new(),
                        &CancellationToken::new(),
                    )
                    .await
                    .is_err()
            );
            let rows = store.list_audit().await.unwrap();
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[1].detail["terminal"]["result"], expected);
        }
    }

    #[tokio::test]
    async fn response_too_large_envelope_is_unknown_and_non_retryable() {
        let (registry, runtime, _, store) = fixture(RiskClass::Read, true, true).await;
        *runtime.failure.lock().unwrap() = Some(
            agent24_os_proto::kernel_call::parse_module_tool_envelope(
                br#"{"kind":"error","code":"response_too_large","retryable":true}"#,
            )
            .unwrap_err(),
        );
        let error = registry
            .dispatch(
                "fake_module.write_local",
                &context(),
                &Map::new(),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        let rows = store.list_audit().await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].detail["terminal"]["result"], "result_unknown");
        assert_unknown_module_error(error, "response_too_large");
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn pending_module_result_is_persisted_and_returned_as_unknown() {
        let (registry, runtime, _, store) = fixture(RiskClass::Read, true, true).await;
        runtime.pending.store(true, Ordering::SeqCst);

        let error = registry
            .dispatch(
                "fake_module.write_local",
                &context(),
                &Map::new(),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();

        let rows = store.list_audit().await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].detail["terminal"]["result"], "result_unknown");
        assert_unknown_module_error(error, "result_unknown");
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 1);
    }

    fn assert_unknown_module_error(error: agent24_tools::ToolError, code: &str) {
        let agent24_tools::ToolError::Failed(message) = error else {
            panic!("expected structured module error");
        };
        let json: Value = serde_json::from_str(&message).unwrap();
        assert_eq!(json["error"]["code"], code);
        assert_eq!(json["error"]["retryable"], false);
        assert_eq!(json["error"]["result_unknown"], true);
    }

    #[tokio::test]
    async fn audit_unavailable_fails_closed_before_module_dispatch() {
        let (registry, runtime, _, store) = fixture(RiskClass::Read, true, true).await;
        sqlx::query("CREATE TRIGGER reject_audit BEFORE INSERT ON audit_log BEGIN SELECT RAISE(FAIL, 'offline'); END")
            .execute(agent24_store::test_hooks::pool(&store))
            .await
            .unwrap();
        let error = registry
            .dispatch(
                "fake_module.write_local",
                &context(),
                &Map::new(),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("module_tool_audit_unavailable"));
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 0);
        assert!(store.list_audit().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn deleted_pre_dispatch_insert_never_reaches_module() {
        let (registry, runtime, _, store) = fixture(RiskClass::Read, true, true).await;
        sqlx::query("CREATE TRIGGER delete_pre AFTER INSERT ON audit_log WHEN NEW.action='k1.module_tool.pre_dispatch' BEGIN DELETE FROM audit_log WHERE seq=NEW.seq; END")
            .execute(agent24_store::test_hooks::pool(&store)).await.unwrap();
        let error = registry
            .dispatch(
                "fake_module.write_local",
                &context(),
                &Map::new(),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("module_tool_audit_unavailable"),
            "{error}"
        );
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 0);
        assert!(store.list_audit().await.unwrap().is_empty());
        store.verify_audit_chain().await.unwrap();
    }

    #[tokio::test]
    async fn module_result_unknown_during_cancellation_settle_is_non_retryable() {
        let (registry, runtime, _, store) = fixture(RiskClass::Read, true, true).await;
        runtime.wait_for_cancel.store(true, Ordering::SeqCst);
        *runtime.failure.lock().unwrap() = Some(ModuleToolCallError::Module {
            code: ModuleToolErrorCode::ResultUnknown,
            retryable: true,
            details: None,
            unknown_code: None,
        });
        let cancel = CancellationToken::new();
        let cancel_task = cancel.clone();
        let entered = runtime.entered.notified();
        let dispatch = tokio::spawn(async move {
            registry
                .dispatch("fake_module.write_local", &context(), &Map::new(), &cancel)
                .await
        });
        entered.await;
        cancel_task.cancel();
        let agent24_tools::ToolError::Failed(message) = dispatch.await.unwrap().unwrap_err() else {
            panic!("expected structured module error");
        };
        let json: Value = serde_json::from_str(&message).unwrap();
        assert_eq!(json["error"]["code"], "result_unknown");
        assert_eq!(json["error"]["retryable"], false);
        assert_eq!(json["error"]["result_unknown"], true);
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 1);
        let rows = store.list_audit().await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].detail["terminal"]["result"], "result_unknown");
    }

    #[tokio::test]
    async fn hash_mismatch_fails_closed_before_module_dispatch() {
        let (registry, runtime, _, store) = fixture(RiskClass::Read, true, true).await;
        store
            .append_audit("2026-10-08T00:00:00Z", "test", "corruptible", &Value::Null)
            .await
            .unwrap();
        sqlx::query("UPDATE audit_log SET hash='tampered' WHERE seq=1")
            .execute(agent24_store::test_hooks::pool(&store))
            .await
            .unwrap();
        assert!(store.verify_audit_chain().await.is_err());
        let relation = ModuleToolAuditRelation {
            attempt_id: Some(AttemptId::new("probe-attempt").unwrap()),
            actor: ActorRef::new("agent24d").unwrap(),
            run_id: RunId::new("probe-run").unwrap(),
            session_ref: None,
            tool_call_id: ToolCallId::new("probe-call").unwrap(),
            module_id: ModuleId::new("fake_module").unwrap(),
            operation_id: OperationId::new("write_local").unwrap(),
            authorization_ref: AuthorizationRef::new("grant:probe").unwrap(),
            resource_ref: None,
        };
        assert!(
            store
                .append_module_tool_audit_event(
                    &AuditTimestamp::new("2026-10-08T00:00:01.000Z").unwrap(),
                    &ModuleToolAuditEvent::PreDispatch(relation),
                )
                .await
                .is_err()
        );

        let result = registry
            .dispatch(
                "fake_module.write_local",
                &context(),
                &Map::new(),
                &CancellationToken::new(),
            )
            .await;
        assert!(result.is_err());
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 0);
        assert_eq!(store.list_audit().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn deleted_audit_tail_fails_closed_before_module_dispatch() {
        let (registry, runtime, _, store) = fixture(RiskClass::Read, true, true).await;
        for (ts, action) in [
            ("2026-10-08T00:00:00Z", "kept"),
            ("2026-10-08T00:00:01Z", "deleted"),
        ] {
            store
                .append_audit(ts, "test", action, &Value::Null)
                .await
                .unwrap();
        }
        // sqlite_sequence still records seq 2, so the remaining row is no
        // longer the tail the log has issued.
        sqlx::query("DELETE FROM audit_log WHERE seq=2")
            .execute(agent24_store::test_hooks::pool(&store))
            .await
            .unwrap();
        assert!(store.verify_audit_chain().await.is_err());

        let result = registry
            .dispatch(
                "fake_module.write_local",
                &context(),
                &Map::new(),
                &CancellationToken::new(),
            )
            .await;
        assert!(result.is_err());
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 0);
        let rows = store.list_audit().await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].action, "kept");
    }

    #[tokio::test]
    async fn parent_cancellation_finishes_module_audit_without_retry() {
        let (registry, runtime, _, store) = fixture(RiskClass::Read, true, true).await;
        runtime.wait_for_cancel.store(true, Ordering::SeqCst);
        let cancel = CancellationToken::new();
        let cancel_task = cancel.clone();
        let entered = runtime.entered.notified();
        let dispatch = tokio::spawn(async move {
            registry
                .dispatch("fake_module.write_local", &context(), &Map::new(), &cancel)
                .await
        });
        entered.await;
        // Parent cancellation now occurs after PreDispatch and actual runtime
        // entry, so the test exercises the future-drop window deterministically.
        cancel_task.cancel();
        assert!(dispatch.await.unwrap().is_err());
        let rows = store.list_audit().await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].detail["terminal"]["result"], "cancelled");
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn cancellation_after_terminal_commit_before_ack_reconciles_persisted_success() {
        let (registry, runtime, _, store) = fixture(RiskClass::Read, true, true).await;
        let commit_gate = agent24_store::test_hooks::pause_next_terminal_commit_ack(&store);
        let committed = commit_gate.notified();
        let cancel = CancellationToken::new();
        let cancel_task = cancel.clone();
        let dispatch = tokio::spawn(async move {
            registry
                .dispatch("fake_module.write_local", &context(), &Map::new(), &cancel)
                .await
        });

        // This boundary is after SQLite COMMIT succeeds and before the append
        // result is acknowledged to ModuleTool::call.
        committed.await;
        let rows = store.list_audit().await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].detail["terminal"]["result"], "success");

        cancel_task.cancel();
        let recovered = dispatch.await.unwrap().unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&recovered).unwrap(),
            serde_json::json!({"completed": true, "response_available": false})
        );
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            store.list_audit().await.unwrap()[1].detail["terminal"]["result"],
            "success"
        );
    }

    #[tokio::test]
    async fn total_budget_timeout_during_check_available_records_terminal() {
        let (registry, runtime, _, store) = fixture(RiskClass::Read, true, true).await;
        runtime.slow_check.store(true, Ordering::SeqCst);

        assert!(
            registry
                .dispatch(
                    "fake_module.write_local",
                    &context(),
                    &Map::new(),
                    &CancellationToken::new(),
                )
                .await
                .is_err()
        );
        let rows = store.list_audit().await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].detail["terminal"]["result"], "cancelled");
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn expired_cancellation_grace_writes_result_unknown_terminal() {
        let (registry, runtime, _, store) = fixture(RiskClass::Read, true, true).await;
        runtime.unresponsive_check.store(true, Ordering::SeqCst);

        assert!(
            registry
                .dispatch(
                    "fake_module.write_local",
                    &context(),
                    &Map::new(),
                    &CancellationToken::new(),
                )
                .await
                .is_err()
        );
        let rows = store.list_audit().await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].detail["terminal"]["result"], "result_unknown");
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn terminal_audit_write_failure_during_cancellation_is_typed_unknown() {
        for fallback in [false, true] {
            let (registry, runtime, _, store) = fixture(RiskClass::Read, true, true).await;
            runtime.wait_for_cancel.store(true, Ordering::SeqCst);
            sqlx::query("CREATE TRIGGER reject_terminal BEFORE INSERT ON audit_log WHEN NEW.action='k1.module_tool.terminal' BEGIN SELECT RAISE(FAIL, 'offline'); END")
                .execute(agent24_store::test_hooks::pool(&store))
                .await
                .unwrap();
            let cancel = CancellationToken::new();
            let cancel_task = cancel.clone();
            let entered = runtime.entered.notified();
            let dispatch = tokio::spawn(async move {
                registry
                    .dispatch("fake_module.write_local", &context(), &Map::new(), &cancel)
                    .await
            });
            entered.await;
            // Occupy the sole audit connection after actual module entry.
            // Holding it past the settle grace drops the original terminal
            // writer, so the interrupted fallback must perform the append.
            let audit_connection = if fallback {
                Some(
                    agent24_store::test_hooks::pool(&store)
                        .acquire()
                        .await
                        .unwrap(),
                )
            } else {
                None
            };
            cancel_task.cancel();
            if fallback {
                tokio::time::sleep(Duration::from_secs(6)).await;
                assert!(
                    !dispatch.is_finished(),
                    "audit connection should block settlement"
                );
            }
            drop(audit_connection);
            assert_unknown_module_error(
                dispatch.await.unwrap().unwrap_err(),
                "module_tool_audit_degraded_result_unknown",
            );
            assert_eq!(runtime.calls.load(Ordering::SeqCst), 1);
            assert_eq!(store.list_audit().await.unwrap().len(), 1);
            store.verify_audit_chain().await.unwrap();
        }
    }

    #[tokio::test]
    async fn terminal_audit_write_failure_exposes_unknown_result_without_retry() {
        let (registry, runtime, _, store) = fixture(RiskClass::Read, true, true).await;
        sqlx::query("CREATE TRIGGER reject_terminal BEFORE INSERT ON audit_log WHEN NEW.action='k1.module_tool.terminal' BEGIN SELECT RAISE(FAIL, 'offline'); END")
            .execute(agent24_store::test_hooks::pool(&store))
            .await
            .unwrap();

        let error = registry
            .dispatch(
                "fake_module.write_local",
                &context(),
                &Map::new(),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert_unknown_module_error(error, "module_tool_audit_degraded_result_unknown");
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 1);
        assert_eq!(store.list_audit().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn post_invoke_permission_denied_with_terminal_write_failure_is_unknown_without_retry() {
        let (registry, runtime, _, store) = fixture(RiskClass::Read, true, true).await;
        *runtime.failure.lock().unwrap() = Some(ModuleToolCallError::Module {
            code: ModuleToolErrorCode::PermissionDenied,
            retryable: false,
            details: None,
            unknown_code: None,
        });
        sqlx::query("CREATE TRIGGER reject_terminal BEFORE INSERT ON audit_log WHEN NEW.action='k1.module_tool.terminal' BEGIN SELECT RAISE(FAIL, 'offline'); END")
            .execute(agent24_store::test_hooks::pool(&store))
            .await
            .unwrap();

        let error = registry
            .dispatch(
                "fake_module.write_local",
                &context(),
                &Map::new(),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();

        assert_unknown_module_error(error, "module_tool_audit_degraded_result_unknown");
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 1);
        assert_eq!(store.list_audit().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn cancellation_settle_permission_denied_with_terminal_write_failure_is_unknown() {
        let (registry, runtime, _, store) = fixture(RiskClass::Read, true, true).await;
        runtime.wait_for_cancel.store(true, Ordering::SeqCst);
        *runtime.failure.lock().unwrap() = Some(ModuleToolCallError::Module {
            code: ModuleToolErrorCode::PermissionDenied,
            retryable: false,
            details: None,
            unknown_code: None,
        });
        sqlx::query("CREATE TRIGGER reject_terminal BEFORE INSERT ON audit_log WHEN NEW.action='k1.module_tool.terminal' BEGIN SELECT RAISE(FAIL, 'offline'); END")
            .execute(agent24_store::test_hooks::pool(&store))
            .await
            .unwrap();
        let cancel = CancellationToken::new();
        let cancel_task = cancel.clone();
        let entered = runtime.entered.notified();
        let dispatch = tokio::spawn(async move {
            registry
                .dispatch("fake_module.write_local", &context(), &Map::new(), &cancel)
                .await
        });
        entered.await;
        cancel_task.cancel();

        assert_unknown_module_error(
            dispatch.await.unwrap().unwrap_err(),
            "module_tool_audit_degraded_result_unknown",
        );
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 1);
        assert_eq!(store.list_audit().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn approval_gate_denial_is_audited_before_returning() {
        let (registry, runtime, approvals, store) =
            fixture(RiskClass::WriteLocal, true, true).await;
        approvals.deny.store(true, Ordering::SeqCst);
        let error = registry
            .dispatch(
                "fake_module.write_local",
                &context(),
                &Map::new(),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("denied"));
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 0);
        let rows = store.list_audit().await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].detail["terminal"]["result"], "denied");
        assert_ne!(
            rows[0].detail["pre_dispatch"]["authorization_ref"],
            "denied:no_grant"
        );
    }

    #[tokio::test]
    async fn a_pre_dispatch_record_left_by_interruption_is_not_reported_complete() {
        use agent24_store::{
            ActorRef, AttemptId, AuditTimestamp, AuthorizationRef, ModuleId, ModuleToolAuditEvent,
            ModuleToolAuditRelation, OperationId, RunId, SessionRef, ToolCallId,
        };
        let store = Store::open_memory().await.unwrap();
        let timestamp = AuditTimestamp::new("2026-10-08T00:00:00.000Z").unwrap();
        let relation = ModuleToolAuditRelation {
            attempt_id: Some(AttemptId::new("attempt-1").unwrap()),
            actor: ActorRef::new("agent24d").unwrap(),
            run_id: RunId::new("run").unwrap(),
            session_ref: Some(SessionRef::new("session").unwrap()),
            tool_call_id: ToolCallId::new("call").unwrap(),
            module_id: ModuleId::new("fake_module").unwrap(),
            operation_id: OperationId::new("write_local").unwrap(),
            authorization_ref: AuthorizationRef::new("grant:ref").unwrap(),
            resource_ref: None,
        };
        store
            .append_module_tool_audit_event(
                &timestamp,
                &ModuleToolAuditEvent::PreDispatch(relation),
            )
            .await
            .unwrap();
        let rows = store.list_audit().await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].action, "k1.module_tool.pre_dispatch");
    }

    #[tokio::test]
    async fn cancellation_during_authorization_cannot_reuse_prior_same_relation_success() {
        let store = Store::open_memory().await.unwrap();
        let timestamp = AuditTimestamp::new("2026-10-08T00:00:00.000Z").unwrap();
        let prior_relation = ModuleToolAuditRelation {
            attempt_id: Some(AttemptId::new("prior-attempt").unwrap()),
            actor: ActorRef::new("agent24d").unwrap(),
            run_id: RunId::new("run").unwrap(),
            session_ref: None,
            tool_call_id: ToolCallId::new("tool-call").unwrap(),
            module_id: ModuleId::new("fake_module").unwrap(),
            operation_id: OperationId::new("write_local").unwrap(),
            authorization_ref: AuthorizationRef::new("grant:prior").unwrap(),
            resource_ref: None,
        };
        store
            .append_module_tool_audit_event(
                &timestamp,
                &ModuleToolAuditEvent::PreDispatch(prior_relation.clone()),
            )
            .await
            .unwrap();
        store
            .append_module_tool_audit_event(
                &timestamp,
                &ModuleToolAuditEvent::Terminal {
                    relation: prior_relation,
                    result: agent24_store::ModuleToolResultCode::Success,
                    duration_ms: None,
                    size_bytes: None,
                },
            )
            .await
            .unwrap();

        let authorization = Arc::new(PausingAuthorization {
            entered: tokio::sync::Notify::new(),
        });
        let entered = authorization.entered.notified();
        let (registry, runtime, _, store, _tool) = fixture_using(
            RiskClass::Read,
            false,
            true,
            true,
            Some(store),
            Some(authorization.clone()),
        )
        .await;
        let cancel = CancellationToken::new();
        let cancel_task = cancel.clone();
        let dispatch = tokio::spawn(async move {
            registry
                .dispatch("fake_module.write_local", &context(), &Map::new(), &cancel)
                .await
        });
        entered.await;
        cancel_task.cancel();
        let error = dispatch.await.unwrap().unwrap_err();
        assert_unknown_module_error(error, "result_unknown");
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 0);
        let rows = store.list_audit().await.unwrap();
        assert_eq!(rows.len(), 2, "no false terminal may be appended");
        assert_eq!(rows[1].detail["terminal"]["result"], "success");
        store.verify_audit_chain().await.unwrap();
    }

    #[tokio::test]
    async fn reused_context_during_authorization_cannot_recover_prior_invocation_success() {
        let authorization = Arc::new(RetainingAuthorization {
            entered_second: tokio::sync::Notify::new(),
            calls: AtomicUsize::new(0),
            retained: std::sync::Mutex::new(None),
        });
        let (registry, runtime, _, store, _tool) = fixture_using(
            RiskClass::Read,
            false,
            true,
            true,
            None,
            Some(authorization.clone()),
        )
        .await;

        registry
            .dispatch(
                "fake_module.write_local",
                &context(),
                &Map::new(),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 1);
        let reused = authorization.retained.lock().unwrap().clone().unwrap();
        let second_auth = authorization.entered_second.notified();
        let cancel = CancellationToken::new();
        let cancel_task = cancel.clone();
        let second = tokio::spawn(async move {
            registry
                .dispatch("fake_module.write_local", &reused, &Map::new(), &cancel)
                .await
        });
        second_auth.await;
        cancel_task.cancel();

        let error = second.await.unwrap().unwrap_err();
        assert_unknown_module_error(error, "result_unknown");
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 1);
        let rows = store.list_audit().await.unwrap();
        assert_eq!(rows.len(), 2, "recovery must not invent a second terminal");
        assert_eq!(rows[1].detail["terminal"]["result"], "success");
        store.verify_audit_chain().await.unwrap();
    }

    #[tokio::test]
    async fn direct_module_tool_call_does_not_settle_a_retained_context_attempt() {
        let authorization = Arc::new(RetainingAuthorization {
            entered_second: tokio::sync::Notify::new(),
            calls: AtomicUsize::new(0),
            retained: std::sync::Mutex::new(None),
        });
        let (registry, runtime, _, store, tool) = fixture_using(
            RiskClass::Read,
            false,
            true,
            true,
            None,
            Some(authorization.clone()),
        )
        .await;
        registry
            .dispatch(
                "fake_module.write_local",
                &context(),
                &Map::new(),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        let retained = authorization.retained.lock().unwrap().clone().unwrap();

        let entered = authorization.entered_second.notified();
        let direct_tool = tool.clone();
        let direct_ctx = retained.clone();
        let direct = tokio::spawn(async move {
            direct_tool
                .call(&direct_ctx, &Map::new(), &CancellationToken::new())
                .await
        });
        entered.await;
        // Drop the direct call while authorization is still blocked, before it
        // has written a PreDispatch row, then exercise the same recovery hook
        // used by the registry after an expired call.
        direct.abort();
        let _ = direct.await;
        let recovered = tool.audit_interrupted(&retained).await.unwrap().unwrap();

        assert_unknown_module_error(recovered.unwrap_err(), "result_unknown");
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 1);
        assert_eq!(store.list_audit().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn interrupted_settlement_cannot_cross_module_tool_adapters() {
        let authorization = Arc::new(RetainingAuthorization {
            entered_second: tokio::sync::Notify::new(),
            calls: AtomicUsize::new(0),
            retained: std::sync::Mutex::new(None),
        });
        let (registry, runtime, _, store, tool) = fixture_using(
            RiskClass::Read,
            false,
            true,
            true,
            None,
            Some(authorization.clone()),
        )
        .await;
        registry
            .dispatch(
                "fake_module.write_local",
                &context(),
                &Map::new(),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        let retained = authorization.retained.lock().unwrap().clone().unwrap();
        authorization.calls.store(0, Ordering::SeqCst);
        let invocation = tool.prepare_invocation(&retained).unwrap();
        tool.call_with_invocation(
            &retained,
            &invocation,
            &Map::new(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        let other_auth: Arc<dyn ModuleToolAuthorization> = Arc::new(PausingAuthorization {
            entered: tokio::sync::Notify::new(),
        });
        let other_runtime = Arc::new(FakeModule {
            running: AtomicBool::new(true),
            calls: AtomicUsize::new(0),
            failure: std::sync::Mutex::new(None),
            pending: AtomicBool::new(false),
            wait_for_cancel: AtomicBool::new(false),
            slow_check: AtomicBool::new(false),
            unresponsive_check: AtomicBool::new(false),
            entered: tokio::sync::Notify::new(),
            remote_tier: AtomicBool::new(false),
            check_entered: tokio::sync::Notify::new(),
            on_check: Hook::default(),
            on_invoke: Hook::default(),
            dispatched: std::sync::Mutex::default(),
            contexts: std::sync::Mutex::default(),
        });
        let other: Arc<dyn agent24_domain::tool::ModuleToolAdvertView> = Arc::new(TestAdvertView {
            authorization: other_auth.clone(),
            running: other_runtime.clone(),
            outbound_ready: AtomicBool::new(true),
        });
        let other_tool = ModuleTool::new(
            "other_module",
            "other_operation",
            "other tool adapter",
            serde_json::json!({"type":"object"}),
            Duration::from_secs(2),
            Duration::from_secs(1),
            RiskClass::Read,
            other,
            other_auth,
            other_runtime,
            store.clone(),
            "agent24d",
        )
        .unwrap();

        let recovered = other_tool
            .audit_interrupted_with_invocation(&retained, &invocation)
            .await
            .unwrap()
            .unwrap();
        assert_unknown_module_error(recovered.unwrap_err(), "result_unknown");
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn sequential_reuse_of_capability_fails_closed_before_authorization() {
        struct BlockingSecondAuth {
            entered_second: tokio::sync::Notify,
            calls: AtomicUsize,
        }
        #[async_trait]
        impl ModuleToolAuthorization for BlockingSecondAuth {
            async fn authorize(
                &self,
                _module: &str,
                _op: &str,
                _ctx: &ToolContext,
            ) -> Result<agent24_tools::ModuleToolGrantContext, ModuleToolCallError> {
                if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    Ok(agent24_tools::ModuleToolGrantContext {
                        authorized_resources: vec![],
                        authorization_ref: "grant:first".into(),
                        per_call_approval: false,
                        valid_for: None,
                        artifact: None,
                    })
                } else {
                    self.entered_second.notify_one();
                    std::future::pending().await
                }
            }
            async fn has_current_consent(&self, _module: &str, _op: &str) -> bool {
                true
            }
            // Its second lookup never answers; this test counts lookups
            // made by a call, not the check after a call returns.
            async fn still_current(&self, _: &str, _: &str, _: &ToolContext, _: &str) -> bool {
                true
            }
        }

        let auth = Arc::new(BlockingSecondAuth {
            entered_second: tokio::sync::Notify::new(),
            calls: AtomicUsize::new(0),
        });
        let (_registry, runtime, _, _store, tool) =
            fixture_using(RiskClass::Read, false, true, true, None, Some(auth.clone())).await;

        let ctx = context();
        let capability = Arc::new(tool.prepare_invocation(&ctx).unwrap());

        // 1. First call via capability succeeds
        let res1 = tool
            .call_with_invocation(&ctx, &capability, &Map::new(), &CancellationToken::new())
            .await;
        assert!(res1.is_ok());
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 1);
        assert_eq!(auth.calls.load(Ordering::SeqCst), 1);

        // 2. Second call with same capability must fail closed before entering authorization
        let cancel = CancellationToken::new();
        let cancel_task = cancel.clone();
        let entered_second = auth.entered_second.notified();
        let second_tool = tool.clone();
        let second_ctx = ctx.clone();
        let cap_clone = capability.clone();
        let second = tokio::spawn(async move {
            second_tool
                .call_with_invocation(&second_ctx, &cap_clone, &Map::new(), &cancel)
                .await
        });

        tokio::select! {
            res = second => {
                let err = res.unwrap().unwrap_err();
                assert_unknown_module_error(err, "result_unknown");
            }
            () = entered_second => {
                cancel_task.cancel();
                panic!("second call must be rejected before entering authorization!");
            }
        }
        assert_eq!(
            auth.calls.load(Ordering::SeqCst),
            1,
            "authorization must not be entered for reused capability"
        );

        // And interrupted settlement must NOT recover A's Success
        let settlement = tool
            .audit_interrupted_with_invocation(&ctx, &capability)
            .await
            .unwrap()
            .unwrap();
        assert_unknown_module_error(settlement.unwrap_err(), "result_unknown");
    }

    #[tokio::test]
    async fn concurrent_reuse_of_capability_allows_only_one_call_to_enter() {
        let (_registry, runtime, _, store, tool) =
            fixture_using(RiskClass::Read, true, true, true, None, None).await;

        let ctx = context();
        let capability = Arc::new(tool.prepare_invocation(&ctx).unwrap());

        let t1 = tool.clone();
        let c1 = ctx.clone();
        let cap1 = capability.clone();
        let h1 = tokio::spawn(async move {
            t1.call_with_invocation(&c1, &cap1, &Map::new(), &CancellationToken::new())
                .await
        });

        let t2 = tool.clone();
        let c2 = ctx.clone();
        let cap2 = capability.clone();
        let h2 = tokio::spawn(async move {
            t2.call_with_invocation(&c2, &cap2, &Map::new(), &CancellationToken::new())
                .await
        });

        let (r1, r2) = tokio::join!(h1, h2);
        let res1 = r1.unwrap();
        let res2 = r2.unwrap();

        // Exactly one must succeed, and the other must fail closed
        let (succeeded, failed) = match (res1.is_ok(), res2.is_ok()) {
            (true, false) => (res1.unwrap(), res2.unwrap_err()),
            (false, true) => (res2.unwrap(), res1.unwrap_err()),
            (true, true) => panic!(
                "both concurrent calls succeeded, capability must not be concurrently reusable"
            ),
            (false, false) => panic!("both concurrent calls failed: {res1:?}, {res2:?}"),
        };
        assert!(succeeded.contains("\"kind\":\"completed\""));
        assert_unknown_module_error(failed, "result_unknown");

        // Side effect / terminal must not be duplicated: exactly 1 runtime call, exactly 2 audit records
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 1);
        let rows = store.list_audit().await.unwrap();
        assert_eq!(
            rows.len(),
            2,
            "must not produce duplicate PreDispatch or Terminal rows"
        );
        assert_eq!(rows[0].action, "k1.module_tool.pre_dispatch");
        assert_eq!(rows[1].detail["terminal"]["result"], "success");
        store.verify_audit_chain().await.unwrap();
    }

    #[tokio::test]
    async fn settlement_while_call_still_active_is_rejected() {
        let (_registry, runtime, _, _store, tool) =
            fixture_using(RiskClass::Read, true, true, true, None, None).await;
        runtime.wait_for_cancel.store(true, Ordering::SeqCst);

        let ctx = context();
        let capability = Arc::new(tool.prepare_invocation(&ctx).unwrap());
        let cancel = CancellationToken::new();
        let entered = runtime.entered.notified();

        let t = tool.clone();
        let c = ctx.clone();
        let cap = capability.clone();
        let call_handle =
            tokio::spawn(
                async move { t.call_with_invocation(&c, &cap, &Map::new(), &cancel).await },
            );

        // Wait until the call is actively executing
        entered.await;

        // While active, settlement must be rejected!
        let active_settlement = tool
            .audit_interrupted_with_invocation(&ctx, &capability)
            .await
            .unwrap()
            .unwrap();
        assert_unknown_module_error(active_settlement.unwrap_err(), "result_unknown");

        // Now abort the call to drop its future while running
        call_handle.abort();
        let _ = call_handle.await;

        // After cancellation/drop, the capability transitioned to recoverable state
        let post_cancel_settlement = tool
            .audit_interrupted_with_invocation(&ctx, &capability)
            .await
            .unwrap()
            .unwrap();
        assert_unknown_module_error(post_cancel_settlement.unwrap_err(), "result_unknown");
    }

    #[tokio::test]
    async fn two_module_tools_with_identical_strings_do_not_accept_each_others_capability() {
        let store = Store::open_memory().await.unwrap();
        let (_reg1, _rt1, _, _, tool_a) =
            fixture_using(RiskClass::Read, true, true, true, Some(store.clone()), None).await;
        let (_reg2, _rt2, _, _, tool_b) =
            fixture_using(RiskClass::Read, true, true, true, Some(store.clone()), None).await;

        let ctx = context();
        let cap_a = tool_a.prepare_invocation(&ctx).unwrap();

        // tool_b must reject cap_a for call
        let call_res = tool_b
            .call_with_invocation(&ctx, &cap_a, &Map::new(), &CancellationToken::new())
            .await;
        let err = call_res.unwrap_err();
        assert_unknown_module_error(err, "result_unknown");

        // tool_b must reject cap_a for settlement
        let settle_res = tool_b
            .audit_interrupted_with_invocation(&ctx, &cap_a)
            .await
            .unwrap()
            .unwrap();
        assert_unknown_module_error(settle_res.unwrap_err(), "result_unknown");
    }

    #[tokio::test]
    async fn unready_outbound_gate_is_not_advertised() {
        let (registry, _, _, _) = fixture_with_outbound(RiskClass::Read, true, true, false).await;
        assert!(registry.live_adverts().await.is_empty());
    }

    #[tokio::test]
    async fn restricted_module_tool_is_denied_when_egress_gate_denies() {
        let (registry, runtime, _, _) = fixture(RiskClass::Read, true, true).await;
        let restricted = context().with_egress_policy(
            <dyn EgressGate>::deny_all(),
            vec![EgressResource::local_only(
                "selected_material:private",
                "rev-1",
            )],
            0,
        );
        let error = registry
            .dispatch(
                "fake_module.write_local",
                &restricted,
                &Map::new(),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(matches!(error, ToolError::Denied(_)));
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 0);
    }

    /// K1-6a-G §4 case 1: a withheld tool called by name, on either
    /// dispatch path, is refused before the module and audited as denied.
    #[tokio::test]
    async fn a_withheld_tool_called_by_name_is_refused_and_audited() {
        let (registry, runtime, _, store) = fixture(RiskClass::Read, true, true).await;
        runtime.remote_tier.store(true, Ordering::SeqCst);
        assert!(registry.live_adverts().await.is_empty());
        let by_dispatch = registry
            .dispatch(
                "fake_module.write_local",
                &context(),
                &Map::new(),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        let preapproved = registry
            .execute_preapproved(
                "fake_module.write_local",
                &context(),
                &Map::new(),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        for error in [by_dispatch, preapproved] {
            assert!(error.to_string().contains("permission_denied"), "{error}");
        }
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 0);
        let rows = store.list_audit().await.unwrap();
        assert_eq!(rows.len(), 4, "a pre-dispatch and a terminal per call");
        for pair in rows.chunks(2) {
            assert_eq!(pair[0].action, "k1.module_tool.pre_dispatch");
            assert_eq!(
                pair[0].detail["pre_dispatch"]["authorization_ref"],
                "denied:remote_tier"
            );
            assert_eq!(pair[1].detail["terminal"]["result"], "denied");
        }
    }

    /// K1-6a-G §2.4: the guard comes before the authorization lookup; an
    /// authorization that never answers is never reached.
    #[tokio::test]
    async fn the_guard_refuses_before_authorization_is_looked_up() {
        let authorization = Arc::new(PausingAuthorization {
            entered: tokio::sync::Notify::new(),
        });
        let (registry, runtime, _, _, _tool) =
            fixture_using(RiskClass::Read, true, true, true, None, Some(authorization)).await;
        runtime.remote_tier.store(true, Ordering::SeqCst);
        let refused = tokio::time::timeout(
            Duration::from_secs(1),
            registry.dispatch(
                "fake_module.write_local",
                &context(),
                &Map::new(),
                &CancellationToken::new(),
            ),
        )
        .await
        .expect("the guard answers without waiting on authorization")
        .unwrap_err();
        assert!(refused.to_string().contains("permission_denied"));
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 0);
    }

    /// K1-6a-G §4 case 2: a remote tier that appears while the daemon runs
    /// refuses the next call; nothing is read once at startup.
    #[tokio::test]
    async fn a_remote_tier_added_while_running_refuses_the_next_call() {
        let (registry, runtime, _, _) = fixture(RiskClass::Read, true, true).await;
        let (ctx, input, cancel) = (context(), Map::new(), CancellationToken::new());
        let call = || registry.dispatch("fake_module.write_local", &ctx, &input, &cancel);
        call().await.unwrap();
        runtime.remote_tier.store(true, Ordering::SeqCst);
        let refused = call().await.unwrap_err();
        assert!(refused.to_string().contains("permission_denied"));
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 1);
    }

    /// The production view reads its probe on every check, and an unreadable
    /// tier configuration counts as a remote tier; only the document-class
    /// module is withheld.
    #[tokio::test]
    async fn the_production_view_reads_the_tier_on_every_check() {
        let tier = Arc::new(std::sync::Mutex::new(Some(false)));
        let probe = {
            let tier = Arc::clone(&tier);
            Arc::new(move || *tier.lock().unwrap())
        };
        let view = AgentModuleToolAdvertView::new(
            AgentModuleToolRuntime::new(HashSet::new()),
            HashMap::new(),
            probe,
        );
        assert!(!view.blocked_by_remote_tier_guard("documents", "get").await);
        *tier.lock().unwrap() = Some(true);
        assert!(view.blocked_by_remote_tier_guard("documents", "get").await);
        *tier.lock().unwrap() = None;
        assert!(view.blocked_by_remote_tier_guard("documents", "get").await);
        assert!(!view.blocked_by_remote_tier_guard("sin90", "get").await);
        *tier.lock().unwrap() = Some(false);
        assert!(!view.blocked_by_remote_tier_guard("documents", "get").await);
    }

    /// A grant whose feed, currency and validity a test drives directly
    /// (K1-6a-G §2.5): `current` decides every lookup; `one_shot` makes
    /// every lookup after the first fail, as a revocation the feed never
    /// announced would.
    struct FeedAuthorization {
        feed: tokio::sync::broadcast::Sender<agent24_store::ModuleConsentChange>,
        current: AtomicBool,
        one_shot: bool,
        valid_for: Option<Duration>,
        lookups: AtomicUsize,
        /// The first lookup answers, then its grant is revoked and
        /// announced at once: a change already waiting before dispatch.
        revoke_on_first: AtomicBool,
        /// Every later lookup (a re-check) is slow, or never answers.
        slow_rechecks: AtomicBool,
        hang_rechecks: AtomicBool,
        /// The first lookup answers, then a change to this tool is
        /// announced; the grant stays current.
        announce_on_first: AtomicBool,
    }

    impl FeedAuthorization {
        fn new(capacity: usize, one_shot: bool, valid_for: Option<Duration>) -> Arc<Self> {
            Arc::new(Self {
                feed: tokio::sync::broadcast::channel(capacity).0,
                current: AtomicBool::new(true),
                one_shot,
                valid_for,
                lookups: AtomicUsize::new(0),
                revoke_on_first: AtomicBool::new(false),
                slow_rechecks: AtomicBool::new(false),
                hang_rechecks: AtomicBool::new(false),
                announce_on_first: AtomicBool::new(false),
            })
        }

        fn announce(&self, op: &str) {
            let _ = self.feed.send(agent24_store::ModuleConsentChange {
                module: "fake_module".to_owned(),
                op: Some(op.to_owned()),
            });
        }
    }

    #[async_trait]
    impl ModuleToolAuthorization for FeedAuthorization {
        async fn authorize(
            &self,
            _: &str,
            _: &str,
            _: &ToolContext,
        ) -> Result<agent24_tools::ModuleToolGrantContext, ModuleToolCallError> {
            let first = self.lookups.fetch_add(1, Ordering::SeqCst) == 0;
            if !first && self.hang_rechecks.load(Ordering::SeqCst) {
                std::future::pending::<()>().await;
            }
            if !first && self.slow_rechecks.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            if !first && self.one_shot {
                self.current.store(false, Ordering::SeqCst);
            }
            if first && self.announce_on_first.load(Ordering::SeqCst) {
                let granted = self.grant();
                self.announce("write_local");
                return granted;
            }
            if first && self.revoke_on_first.load(Ordering::SeqCst) {
                let granted = self.grant();
                self.current.store(false, Ordering::SeqCst);
                self.announce("write_local");
                return granted;
            }
            self.grant()
        }

        fn consent_changes(
            &self,
        ) -> Option<tokio::sync::broadcast::Receiver<agent24_store::ModuleConsentChange>> {
            Some(self.feed.subscribe())
        }
    }

    impl FeedAuthorization {
        fn grant(&self) -> Result<agent24_tools::ModuleToolGrantContext, ModuleToolCallError> {
            if self.current.load(Ordering::SeqCst) {
                Ok(agent24_tools::ModuleToolGrantContext {
                    authorized_resources: vec![],
                    authorization_ref: "grant:feed".to_owned(),
                    per_call_approval: false,
                    valid_for: self.valid_for,
                    artifact: None,
                })
            } else {
                Err(ModuleToolCallError::Module {
                    code: agent24_domain::tool::ModuleToolErrorCode::PermissionDenied,
                    retryable: false,
                    details: None,
                    unknown_code: None,
                })
            }
        }
    }

    /// Starts one call that waits inside the module until it is cancelled,
    /// and returns once the module has it.
    async fn call_in_module(
        registry: ToolRegistry,
        runtime: &Arc<FakeModule>,
    ) -> tokio::task::JoinHandle<Result<String, ToolError>> {
        runtime.wait_for_cancel.store(true, Ordering::SeqCst);
        let entered = runtime.entered.notified();
        let call = tokio::spawn(async move {
            registry
                .dispatch(
                    "fake_module.write_local",
                    &context(),
                    &Map::new(),
                    &CancellationToken::new(),
                )
                .await
        });
        entered.await;
        call
    }

    async fn stopped_within(call: tokio::task::JoinHandle<Result<String, ToolError>>) -> ToolError {
        tokio::time::timeout(Duration::from_secs(1), call)
            .await
            .expect("the call stops when its grant ends")
            .unwrap()
            .unwrap_err()
    }

    /// The daemon's consent clock keeps milliseconds (#1134 PR-Daemon M10).
    #[test]
    fn the_consent_clock_reads_milliseconds() {
        let now = super::consent_clock();
        assert_eq!((now.len(), &now[19..20]), (24, "."), "{now}");
    }

    /// K1-6a-G §4 case 3: a revocation while the module runs stops the call
    /// as `result_unknown` (it may have been sent), and the next is refused.
    #[tokio::test]
    async fn a_revocation_while_the_module_runs_stops_the_call() {
        let (registry, runtime, _, store, tool) =
            fixture_using(RiskClass::Read, true, true, true, None, None).await;
        let call = call_in_module(registry, &runtime).await;
        store
            .revoke_module_consent(
                "fake_module",
                Some("write_local"),
                "2026-10-08T00:00:00.500Z",
            )
            .await
            .unwrap();
        assert_unknown_module_error(stopped_within(call).await, "result_unknown");
        let rows = store.list_audit().await.unwrap();
        assert_eq!(
            rows.last().unwrap().detail["terminal"]["result"],
            "result_unknown"
        );
        runtime.wait_for_cancel.store(false, Ordering::SeqCst);
        let refused = tool
            .call(&context(), &Map::new(), &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(refused.to_string().contains("permission_denied"));
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 1);
    }

    /// K1-6a-G §4 case 3: stopped before anything was sent, the call is
    /// `cancelled` and the model sees `permission_denied`.
    #[tokio::test]
    async fn a_revocation_before_dispatch_cancels_without_reaching_the_module() {
        let (registry, runtime, _, store) = fixture(RiskClass::Read, true, true).await;
        runtime.slow_check.store(true, Ordering::SeqCst);
        let checking = runtime.check_entered.notified();
        let started = std::time::Instant::now();
        let call = tokio::spawn(async move {
            registry
                .dispatch(
                    "fake_module.write_local",
                    &context(),
                    &Map::new(),
                    &CancellationToken::new(),
                )
                .await
        });
        checking.await;
        store
            .revoke_module_consent("fake_module", None, "2026-10-08T00:00:00.500Z")
            .await
            .unwrap();
        let error = stopped_within(call).await;
        assert!(error.to_string().contains("permission_denied"), "{error}");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "it did not wait out the check"
        );
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 0);
        let rows = store.list_audit().await.unwrap();
        assert_eq!(
            rows.last().unwrap().detail["terminal"]["result"],
            "cancelled"
        );
    }

    /// K1-6a-G §4 case 3a: a replacing grant, a refusal and a revocation of
    /// the whole module each end the reference the call started under.
    #[tokio::test]
    async fn every_change_that_ends_the_grant_stops_the_call() {
        for change in ["regrant", "deny", "revoke_module"] {
            let (registry, runtime, _, store) = fixture(RiskClass::Read, true, true).await;
            let call = call_in_module(registry, &runtime).await;
            match change {
                "regrant" => {
                    store
                        .grant_module_consent_scoped(
                            &summary(),
                            &fixture_scope(),
                            "2026-10-08T00:00:00.700Z",
                            "2026-11-08T00:00:00Z",
                        )
                        .await
                        .unwrap();
                }
                "deny" => {
                    store
                        .deny_module_consent(
                            &summary(),
                            "2026-10-08T00:00:00.700Z",
                            "2026-11-08T00:00:00Z",
                        )
                        .await
                        .unwrap();
                }
                _ => store
                    .revoke_module_consent("fake_module", None, "2026-10-08T00:00:00.700Z")
                    .await
                    .unwrap(),
            }
            assert_unknown_module_error(stopped_within(call).await, "result_unknown");
        }
    }

    /// The control: a change to another tool, or one that leaves this
    /// call's reference current, does not stop it.
    #[tokio::test]
    async fn a_change_that_leaves_the_grant_current_does_not_stop_the_call() {
        let (registry, runtime, _, store) = fixture(RiskClass::Read, true, true).await;
        let mut call = call_in_module(registry, &runtime).await;
        store
            .revoke_module_consent("fake_module", Some("other_op"), "2026-10-08T00:00:00.500Z")
            .await
            .unwrap();
        store
            .revoke_module_consent("other_module", None, "2026-10-08T00:00:00.500Z")
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(300), &mut call)
                .await
                .is_err(),
            "a change to another tool stopped the call"
        );
        call.abort();
    }

    /// A grant valid for longer than an `Instant` can represent runs the
    /// call instead of panicking on the deadline.
    #[tokio::test]
    async fn a_grant_too_long_for_a_deadline_still_runs_the_call() {
        let authorization = FeedAuthorization::new(1, false, Some(Duration::MAX));
        let (registry, runtime, _, _, _tool) = fixture_using(
            RiskClass::Read,
            false,
            true,
            true,
            None,
            Some(authorization),
        )
        .await;
        registry
            .dispatch(
                "fake_module.write_local",
                &context(),
                &Map::new(),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 1);
    }

    /// K1-6a-G §4 case 3b: a watcher that fell behind re-reads the grant;
    /// the call stops if it is no longer current, and only then.
    #[tokio::test]
    async fn a_lagged_watcher_rechecks_and_stops_only_an_ended_grant() {
        for still_current in [true, false] {
            let authorization = FeedAuthorization::new(1, false, None);
            let (registry, runtime, _, _, _tool) = fixture_using(
                RiskClass::Read,
                false,
                true,
                true,
                None,
                Some(authorization.clone()),
            )
            .await;
            let mut call = call_in_module(registry, &runtime).await;
            // `false` makes the re-check's lookup fail (`Err`): a failed
            // re-check stops the call like an ended grant (§4 case 3b).
            authorization.current.store(still_current, Ordering::SeqCst);
            // Unrelated to this tool, and more than the channel holds.
            for _ in 0..3 {
                authorization.announce("other_op");
            }
            let stopped = tokio::time::timeout(Duration::from_millis(500), &mut call).await;
            if still_current {
                assert!(
                    stopped.is_err(),
                    "a lag with a current grant stopped the call"
                );
                call.abort();
            } else {
                let error = stopped
                    .expect("the lag re-check stops it")
                    .unwrap()
                    .unwrap_err();
                assert_unknown_module_error(error, "result_unknown");
            }
        }
    }

    /// K1-6a-G §4 case 3 (review C1/G2): a change taken while the module was
    /// still being checked, its re-check not yet answered, stops the call
    /// before dispatch; the instant check must not swallow it.
    #[tokio::test]
    async fn a_change_taken_before_dispatch_still_stops_the_call() {
        let authorization = FeedAuthorization::new(8, false, None);
        authorization.revoke_on_first.store(true, Ordering::SeqCst);
        authorization.slow_rechecks.store(true, Ordering::SeqCst);
        let (registry, runtime, _, store, _tool) = fixture_using(
            RiskClass::Read,
            false,
            true,
            true,
            None,
            Some(authorization),
        )
        .await;
        let error = registry
            .dispatch(
                "fake_module.write_local",
                &context(),
                &Map::new(),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("permission_denied"), "{error}");
        assert_eq!(
            runtime.calls.load(Ordering::SeqCst),
            0,
            "dispatched after the revocation"
        );
        let rows = store.list_audit().await.unwrap();
        assert_eq!(
            rows.last().unwrap().detail["terminal"]["result"],
            "cancelled"
        );
    }

    /// K1-6a-G §4 case 3 (review round 2, G1): a change taken before
    /// dispatch is settled even when the module turns out unavailable: the
    /// call ends as a revocation, not as the module's own error.
    #[tokio::test]
    async fn a_change_taken_before_dispatch_wins_over_an_unavailable_module() {
        let authorization = FeedAuthorization::new(8, false, None);
        authorization.revoke_on_first.store(true, Ordering::SeqCst);
        authorization.slow_rechecks.store(true, Ordering::SeqCst);
        let (registry, runtime, _, store, _tool) = fixture_using(
            RiskClass::Read,
            false,
            false,
            true,
            None,
            Some(authorization),
        )
        .await;
        let error = registry
            .dispatch(
                "fake_module.write_local",
                &context(),
                &Map::new(),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("permission_denied"), "{error}");
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 0);
        let rows = store.list_audit().await.unwrap();
        assert_eq!(
            rows.last().unwrap().detail["terminal"]["result"],
            "cancelled"
        );
    }

    /// K1-6a-G §2.5 (review round 3, C2): a cancellation while dispatch
    /// waits on a re-check ends the call at once as `cancelled`, nothing
    /// sent, instead of waiting out the re-check.
    #[tokio::test]
    async fn a_cancellation_during_a_recheck_before_dispatch_is_prompt() {
        let authorization = FeedAuthorization::new(8, false, None);
        authorization
            .announce_on_first
            .store(true, Ordering::SeqCst);
        authorization.hang_rechecks.store(true, Ordering::SeqCst);
        let (registry, runtime, _, store, _tool) = fixture_using(
            RiskClass::Read,
            false,
            true,
            true,
            None,
            Some(authorization),
        )
        .await;
        let cancel = CancellationToken::new();
        let stop = cancel.clone();
        let call = tokio::spawn(async move {
            registry
                .dispatch("fake_module.write_local", &context(), &Map::new(), &cancel)
                .await
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        let started = std::time::Instant::now();
        stop.cancel();
        let error = tokio::time::timeout(Duration::from_secs(1), call)
            .await
            .expect("the cancellation is prompt")
            .unwrap()
            .unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(1));
        let agent24_tools::ToolError::Failed(message) = error else {
            panic!("expected a structured module error, got {error}");
        };
        let json: Value = serde_json::from_str(&message).unwrap();
        assert_eq!(json["error"]["code"], "cancelled", "{message}");
        assert_eq!(
            json["error"]["result_unknown"], false,
            "nothing was sent: {message}"
        );
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 0);
        let rows = store.list_audit().await.unwrap();
        assert_eq!(
            rows.last().unwrap().detail["terminal"]["result"],
            "cancelled"
        );
    }

    /// K1-6a-G §2.5 (review C3): a re-check that never answers does not hold
    /// the call past its grant's deadline.
    #[tokio::test]
    async fn a_hung_recheck_still_ends_at_the_deadline() {
        let authorization = FeedAuthorization::new(8, false, Some(Duration::from_millis(300)));
        authorization.hang_rechecks.store(true, Ordering::SeqCst);
        let (registry, runtime, _, _, _tool) = fixture_using(
            RiskClass::Read,
            false,
            true,
            true,
            None,
            Some(authorization.clone()),
        )
        .await;
        let started = std::time::Instant::now();
        let call = call_in_module(registry, &runtime).await;
        authorization.announce("write_local");
        assert_unknown_module_error(stopped_within(call).await, "result_unknown");
        assert!(
            started.elapsed() < Duration::from_millis(700),
            "held past the deadline until the inline wait: {:?}",
            started.elapsed()
        );
    }

    /// K1-6a-G §2.5 (review C6/G5): nothing the module said, an error
    /// included, reaches the model under a grant that has ended.
    #[tokio::test]
    async fn an_error_under_an_ended_grant_is_not_passed_on() {
        let authorization = FeedAuthorization::new(8, true, None);
        let (registry, runtime, _, _, _tool) = fixture_using(
            RiskClass::Read,
            false,
            true,
            true,
            None,
            Some(authorization),
        )
        .await;
        *runtime.failure.lock().unwrap() = Some(ModuleToolCallError::Module {
            code: agent24_domain::tool::ModuleToolErrorCode::ModuleError,
            retryable: true,
            details: None,
            unknown_code: Some("module_says_this".to_owned()),
        });
        let error = registry
            .dispatch(
                "fake_module.write_local",
                &context(),
                &Map::new(),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("permission_denied"), "{error}");
        assert!(!error.contains("module_says_this"), "{error}");
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 1);
    }

    /// K1-6a-G §4 case 4: expiry in the middle of a call stops it, with no
    /// event at all.
    #[tokio::test]
    async fn a_grant_that_expires_mid_call_stops_it() {
        let authorization = FeedAuthorization::new(8, false, Some(Duration::from_millis(300)));
        let (registry, runtime, _, _, _tool) = fixture_using(
            RiskClass::Read,
            false,
            true,
            true,
            None,
            Some(authorization),
        )
        .await;
        let started = std::time::Instant::now();
        let call = call_in_module(registry, &runtime).await;
        assert_unknown_module_error(stopped_within(call).await, "result_unknown");
        assert!(started.elapsed() >= Duration::from_millis(250));
    }

    /// #1139 PR-Daemon B1: a watcher that fell behind between the wait for
    /// the module and dispatch re-reads the grant there, and a grant ended
    /// meanwhile is not dispatched. The lag lands inside the availability
    /// check's one poll, so only the check right before dispatch sees it.
    #[tokio::test]
    async fn a_lag_just_before_dispatch_rechecks_and_stops_an_ended_grant() {
        let authorization = FeedAuthorization::new(1, false, None);
        let (registry, runtime, _, store, _tool) = fixture_using(
            RiskClass::Read,
            false,
            true,
            true,
            None,
            Some(authorization.clone()),
        )
        .await;
        hook(&runtime.on_check, move || {
            // Ended, with its notice among those lost to the lag.
            authorization.current.store(false, Ordering::SeqCst);
            for _ in 0..3 {
                authorization.announce("other_op");
            }
        });
        let error = registry
            .dispatch(
                "fake_module.write_local",
                &context(),
                &Map::new(),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("permission_denied"), "{error}");
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 0, "dispatched");
        let rows = store.list_audit().await.unwrap();
        assert_eq!(
            rows.last().unwrap().detail["terminal"]["result"],
            "cancelled"
        );
    }

    /// #1139 PR-Daemon B1: a grant whose deadline passed while the module
    /// was being checked is not dispatched, with no change announced.
    #[tokio::test]
    async fn a_grant_past_its_deadline_at_dispatch_is_not_dispatched() {
        let authorization = FeedAuthorization::new(8, false, Some(Duration::from_millis(30)));
        let (registry, runtime, _, store, _tool) = fixture_using(
            RiskClass::Read,
            false,
            true,
            true,
            None,
            Some(authorization),
        )
        .await;
        hook(&runtime.on_check, || {
            std::thread::sleep(Duration::from_millis(80));
        });
        let error = registry
            .dispatch(
                "fake_module.write_local",
                &context(),
                &Map::new(),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("permission_denied"), "{error}");
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 0, "dispatched");
        let rows = store.list_audit().await.unwrap();
        assert_eq!(
            rows.last().unwrap().detail["terminal"]["result"],
            "cancelled"
        );
    }

    /// #1139 PR-Daemon B1: an answer that comes back after the grant's
    /// deadline is discarded even though the lookup still says current.
    #[tokio::test]
    async fn a_result_after_the_deadline_is_discarded_whatever_the_lookup_says() {
        let authorization = FeedAuthorization::new(8, false, Some(Duration::from_millis(60)));
        let (registry, runtime, _, store, _tool) = fixture_using(
            RiskClass::Read,
            false,
            true,
            true,
            None,
            Some(authorization.clone()),
        )
        .await;
        hook(&runtime.on_invoke, || {
            std::thread::sleep(Duration::from_millis(120));
        });
        let error = registry
            .dispatch(
                "fake_module.write_local",
                &context(),
                &Map::new(),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("permission_denied"), "{error}");
        assert!(
            !error.to_string().contains("\"ok\""),
            "the payload leaked: {error}"
        );
        assert!(authorization.current.load(Ordering::SeqCst));
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 1);
        let rows = store.list_audit().await.unwrap();
        assert_eq!(rows.last().unwrap().detail["terminal"]["result"], "success");
    }

    /// #1139 PR-Daemon (Low): a pending job carries nothing of the
    /// module's, so under an ended grant it is still reported as unknown,
    /// not as refused.
    #[tokio::test]
    async fn a_pending_job_under_an_ended_grant_is_still_unknown() {
        let authorization = FeedAuthorization::new(8, true, None);
        let (registry, runtime, _, _, _tool) = fixture_using(
            RiskClass::Read,
            false,
            true,
            true,
            None,
            Some(authorization),
        )
        .await;
        runtime.pending.store(true, Ordering::SeqCst);
        let error = registry
            .dispatch(
                "fake_module.write_local",
                &context(),
                &Map::new(),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert_unknown_module_error(error, "result_unknown");
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 1);
    }

    /// The deadline is the stored grant's own expiry, measured by the
    /// authorization's clock (here 00:00:01 against an expiry at 00:00:03),
    /// not the wall clock.
    #[tokio::test]
    async fn a_stored_grant_carries_its_remaining_validity() {
        let summary = summary();
        let store = Store::open_memory().await.unwrap();
        store
            .grant_module_consent_scoped(
                &summary,
                &fixture_scope(),
                "2026-10-08T00:00:00Z",
                "2026-10-08T00:00:03Z",
            )
            .await
            .unwrap();
        let gate: Arc<dyn ConsentGate> = Arc::new(StoreConsentGate::new(store.clone()));
        let authorization = ModuleConsentAuthorization::new(
            gate,
            ConsentGateRequest {
                module: summary.module.clone(),
                op: summary.op.clone(),
                module_version: summary.module_version.clone(),
                current_scope_fingerprint: summary.scope_fingerprint(),
                risk: RiskClass::Read,
                org_restricted: false,
                source_restricted: false,
                reversible_draft_declared: false,
                draft_verification: None,
                current_artifact: None,
            },
            Arc::new(|| "2026-10-08T00:00:01Z".to_owned()),
        )
        .with_artifact_source(fixture_package());
        let grant = authorization
            .authorize("fake_module", "write_local", &context())
            .await
            .unwrap();
        assert_eq!(grant.valid_for, Some(Duration::from_secs(2)));
    }

    /// K1-6a-G §4 case 5: a grant that ended while the module answered —
    /// with no announcement — keeps the answer from the model; the audit
    /// keeps what the module did.
    #[tokio::test]
    async fn a_result_under_an_ended_grant_is_discarded() {
        let authorization = FeedAuthorization::new(8, true, None);
        let (registry, runtime, _, store, _tool) = fixture_using(
            RiskClass::Read,
            false,
            true,
            true,
            None,
            Some(authorization),
        )
        .await;
        let error = registry
            .dispatch(
                "fake_module.write_local",
                &context(),
                &Map::new(),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("permission_denied"), "{error}");
        assert!(
            !error.to_string().contains("\"ok\""),
            "the payload leaked: {error}"
        );
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 1);
        let rows = store.list_audit().await.unwrap();
        assert_eq!(rows.last().unwrap().detail["terminal"]["result"], "success");
    }

    #[tokio::test]
    async fn agent_module_tool_advert_view_outbound_gate_ready_only_for_registered_operations() {
        let mut ops = HashSet::new();
        ops.insert(("doc_mod".to_owned(), "read_op".to_owned()));
        let runtime = AgentModuleToolRuntime::new(ops);
        let view =
            AgentModuleToolAdvertView::new(runtime, HashMap::new(), Arc::new(|| Some(false)));
        assert!(view.outbound_gate_ready("doc_mod", "read_op").await);
        assert!(!view.outbound_gate_ready("doc_mod", "unknown_op").await);
    }

    /// A grant whose `authorization_ref` is `grant:<epoch>`.
    struct RebindingAuthorization {
        epoch: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ModuleToolAuthorization for RebindingAuthorization {
        async fn authorize(
            &self,
            _: &str,
            _: &str,
            _: &ToolContext,
        ) -> Result<agent24_tools::ModuleToolGrantContext, ModuleToolCallError> {
            Ok(agent24_tools::ModuleToolGrantContext {
                authorized_resources: vec![],
                authorization_ref: format!("grant:{}", self.epoch.load(Ordering::SeqCst)),
                per_call_approval: false,
                valid_for: None,
                artifact: None,
            })
        }
    }

    /// Approves every card; the first `rebinds` approvals each replace the
    /// grant just after the human said yes.
    struct RebindingApproval {
        calls: AtomicUsize,
        epoch: Arc<AtomicUsize>,
        rebinds: usize,
    }

    #[async_trait]
    impl ApprovalGate for RebindingApproval {
        async fn check(
            &self,
            _: &ToolInfo,
            _: &ToolContext,
            _: &Map<String, Value>,
            _: Option<&str>,
            _: &CancellationToken,
        ) -> GateDecision {
            if self.calls.fetch_add(1, Ordering::SeqCst) < self.rebinds {
                self.epoch.fetch_add(1, Ordering::SeqCst);
            }
            GateDecision::Allow
        }
    }

    async fn rebinding_fixture(
        rebinds: usize,
    ) -> (ToolRegistry, Arc<FakeModule>, Arc<RebindingApproval>, Store) {
        let epoch = Arc::new(AtomicUsize::new(0));
        let authorization = Arc::new(RebindingAuthorization {
            epoch: Arc::clone(&epoch),
        });
        let (_, runtime, _, store, tool) = fixture_using(
            RiskClass::WriteLocal,
            true,
            true,
            true,
            None,
            Some(authorization),
        )
        .await;
        let approvals = Arc::new(RebindingApproval {
            calls: AtomicUsize::new(0),
            epoch,
            rebinds,
        });
        let mut registry = ToolRegistry::new()
            .with_risk_overrides(Arc::new(RelaxModuleRisk))
            .with_gate(approvals.clone());
        registry.register_module_tool(tool).unwrap();
        (registry, runtime, approvals, store)
    }

    async fn dispatch_write_local(registry: &ToolRegistry) -> Result<String, ToolError> {
        registry
            .dispatch(
                "fake_module.write_local",
                &context(),
                &Map::new(),
                &CancellationToken::new(),
            )
            .await
    }

    /// K1-6a-G §4 case 17: the grant changed between approval and dispatch,
    /// so that approval is void and the human is asked again; the module
    /// runs once, under the grant the second card was shown for.
    #[tokio::test]
    async fn a_grant_replaced_after_approval_asks_again() {
        let (registry, runtime, approvals, store) = rebinding_fixture(1).await;
        dispatch_write_local(&registry).await.unwrap();
        assert_eq!(approvals.calls.load(Ordering::SeqCst), 2);
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 1);
        let rows = store.list_audit().await.unwrap();
        let results: Vec<_> = rows
            .iter()
            .filter_map(|row| row.detail["terminal"]["result"].as_str())
            .collect();
        assert_eq!(results, ["cancelled", "success"]);
        assert_eq!(
            rows[2].detail["pre_dispatch"]["authorization_ref"],
            "grant:1"
        );
    }

    /// A grant that keeps changing is not asked about forever.
    #[tokio::test]
    async fn a_grant_that_keeps_changing_is_refused_after_three_cards() {
        let (registry, runtime, approvals, _) = rebinding_fixture(usize::MAX).await;
        let error = dispatch_write_local(&registry).await.unwrap_err();
        assert!(matches!(error, ToolError::Denied(_)), "{error}");
        assert_eq!(approvals.calls.load(Ordering::SeqCst), 3);
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 0);
    }

    /// #1130 L3: a call that could never run shows no card. Withheld by the
    /// gate-5 guard, or with no grant, it is refused and audited before the
    /// approval gate is asked.
    #[tokio::test]
    async fn a_call_bound_to_be_refused_shows_no_card() {
        let (registry, runtime, approvals, store) =
            fixture(RiskClass::WriteLocal, true, true).await;
        runtime.remote_tier.store(true, Ordering::SeqCst);
        let withheld = dispatch_write_local(&registry).await.unwrap_err();
        let (registry, ungranted_runtime, ungranted_approvals, ungranted_store) =
            fixture(RiskClass::WriteLocal, false, true).await;
        let ungranted = dispatch_write_local(&registry).await.unwrap_err();
        for error in [withheld, ungranted] {
            assert!(error.to_string().contains("permission_denied"), "{error}");
        }
        for (approvals, runtime, store, authorization_ref) in [
            (approvals, runtime, store, "denied:remote_tier"),
            (
                ungranted_approvals,
                ungranted_runtime,
                ungranted_store,
                "denied:no_grant",
            ),
        ] {
            assert_eq!(approvals.calls.load(Ordering::SeqCst), 0);
            assert_eq!(runtime.calls.load(Ordering::SeqCst), 0);
            let rows = store.list_audit().await.unwrap();
            assert_eq!(rows.len(), 2);
            assert_eq!(
                rows[0].detail["pre_dispatch"]["authorization_ref"],
                authorization_ref
            );
            assert_eq!(rows[1].detail["terminal"]["result"], "denied");
        }
    }

    async fn preapprove_write_local(registry: &ToolRegistry) -> Result<String, ToolError> {
        registry
            .execute_preapproved(
                "fake_module.write_local",
                &context(),
                &Map::new(),
                &CancellationToken::new(),
            )
            .await
    }

    /// An approval given out of band (durable resume) does not record which
    /// grant the human saw, so it is not taken as covering this one: the
    /// call stops before the module, for the caller to ask again.
    #[tokio::test]
    async fn an_approval_from_a_resume_stops_before_the_module() {
        let (registry, runtime, approvals, store) =
            fixture(RiskClass::WriteLocal, true, true).await;
        let error = preapprove_write_local(&registry).await.unwrap_err();
        assert!(matches!(error, ToolError::ApprovalStale), "{error}");
        assert_eq!(approvals.calls.load(Ordering::SeqCst), 0);
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 0);
        let rows = store.list_audit().await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].detail["terminal"]["result"], "cancelled");
    }

    /// Grants every lookup and keeps the last context it was handed.
    struct KeepingAuthorization {
        kept: std::sync::Mutex<Option<ToolContext>>,
    }

    #[async_trait]
    impl ModuleToolAuthorization for KeepingAuthorization {
        async fn authorize(
            &self,
            _: &str,
            _: &str,
            ctx: &ToolContext,
        ) -> Result<agent24_tools::ModuleToolGrantContext, ModuleToolCallError> {
            *self.kept.lock().unwrap() = Some(ctx.clone());
            Ok(agent24_tools::ModuleToolGrantContext {
                authorized_resources: vec![],
                authorization_ref: "grant:kept".to_owned(),
                per_call_approval: false,
                valid_for: None,
                artifact: None,
            })
        }
    }

    /// No approval travels on a `ToolContext`: the one an adapter kept from
    /// inside an approved call does not let a direct call reach the module.
    #[tokio::test]
    async fn a_context_kept_from_an_approved_call_carries_no_approval() {
        let authorization = Arc::new(KeepingAuthorization {
            kept: std::sync::Mutex::new(None),
        });
        let (registry, runtime, approvals, _, tool) = fixture_using(
            RiskClass::WriteLocal,
            true,
            true,
            true,
            None,
            Some(authorization.clone()),
        )
        .await;
        dispatch_write_local(&registry).await.unwrap();
        let kept = authorization.kept.lock().unwrap().take().unwrap();
        let direct = tool
            .call(&kept, &Map::new(), &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(matches!(direct, ToolError::ApprovalStale), "{direct}");
        assert_eq!(approvals.calls.load(Ordering::SeqCst), 1);
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 1);
    }

    struct TightenModuleRisk;

    impl RiskOverrides for TightenModuleRisk {
        fn resolve(&self, _: &str) -> Option<RiskClass> {
            Some(RiskClass::WriteLocal)
        }
    }

    /// A tool declared `read` that the user tightened to ask every call:
    /// an approval from a resume names no grant for it either.
    #[tokio::test]
    async fn a_tightened_tool_approved_out_of_band_stops_before_the_module() {
        let (_, runtime, approvals, _, tool) =
            fixture_using(RiskClass::Read, true, true, true, None, None).await;
        let mut registry = ToolRegistry::new()
            .with_risk_overrides(Arc::new(TightenModuleRisk))
            .with_gate(approvals.clone());
        registry.register_module_tool(tool).unwrap();
        let error = preapprove_write_local(&registry).await.unwrap_err();
        assert!(matches!(error, ToolError::ApprovalStale), "{error}");
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 0);
        dispatch_write_local(&registry).await.unwrap();
        assert_eq!(approvals.calls.load(Ordering::SeqCst), 1);
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 1);
    }

    /// Hands out `grant:a` until the call's own lookup, which replaces it
    /// with `grant:b` and announces the change.
    struct ReplacingAuthorization {
        lookups: AtomicUsize,
        feed: tokio::sync::broadcast::Sender<agent24_store::ModuleConsentChange>,
    }

    #[async_trait]
    impl ModuleToolAuthorization for ReplacingAuthorization {
        async fn authorize(
            &self,
            _: &str,
            _: &str,
            _: &ToolContext,
        ) -> Result<agent24_tools::ModuleToolGrantContext, ModuleToolCallError> {
            let lookup = self.lookups.fetch_add(1, Ordering::SeqCst) + 1;
            if lookup == 2 {
                let _ = self.feed.send(agent24_store::ModuleConsentChange {
                    module: "fake_module".to_owned(),
                    op: Some("write_local".to_owned()),
                });
            }
            Ok(agent24_tools::ModuleToolGrantContext {
                authorized_resources: vec![],
                authorization_ref: if lookup <= 2 { "grant:a" } else { "grant:b" }.to_owned(),
                per_call_approval: false,
                valid_for: None,
                artifact: None,
            })
        }

        fn consent_changes(
            &self,
        ) -> Option<tokio::sync::broadcast::Receiver<agent24_store::ModuleConsentChange>> {
            Some(self.feed.subscribe())
        }
    }

    /// The grant is replaced after the call matched its approval but before
    /// the module has it: still the window §2.3 covers, so the approval is
    /// void and the human is asked again, not told `permission_denied`.
    #[tokio::test]
    async fn a_grant_replaced_just_before_dispatch_asks_again() {
        let authorization = Arc::new(ReplacingAuthorization {
            lookups: AtomicUsize::new(0),
            feed: tokio::sync::broadcast::channel(4).0,
        });
        let (registry, runtime, approvals, _, _) = fixture_using(
            RiskClass::WriteLocal,
            true,
            true,
            true,
            None,
            Some(authorization),
        )
        .await;
        dispatch_write_local(&registry).await.unwrap();
        assert_eq!(approvals.calls.load(Ordering::SeqCst), 2);
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 1);
    }

    /// The lookup before the card is cancellable: a run cancelled while it
    /// hangs stops promptly, and no card is shown.
    #[tokio::test]
    async fn a_lookup_before_the_card_that_hangs_is_cancellable() {
        let authorization = Arc::new(PausingAuthorization {
            entered: tokio::sync::Notify::new(),
        });
        let (registry, runtime, approvals, _, _) = fixture_using(
            RiskClass::WriteLocal,
            true,
            true,
            true,
            None,
            Some(authorization.clone()),
        )
        .await;
        let cancel = CancellationToken::new();
        let entered = authorization.entered.notified();
        let call = tokio::spawn({
            let cancel = cancel.clone();
            async move {
                registry
                    .dispatch("fake_module.write_local", &context(), &Map::new(), &cancel)
                    .await
            }
        });
        entered.await;
        cancel.cancel();
        let error = tokio::time::timeout(Duration::from_millis(500), call)
            .await
            .expect("cancellation reaches the lookup")
            .unwrap()
            .unwrap_err();
        assert!(matches!(error, ToolError::Cancelled), "{error}");
        assert_eq!(approvals.calls.load(Ordering::SeqCst), 0);
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 0);
    }
}
