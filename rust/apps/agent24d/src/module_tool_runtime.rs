use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use crate::domain::Supervisors;
use agent24_domain::tool::{
    ModuleToolAdvertView, ModuleToolCallError, ModuleToolErrorCode, ModuleToolResult,
};
use agent24_os_proto::drain::{DrainState, Generation};
use agent24_os_proto::kernel_call::{
    KernelLimits, KernelRequest, KernelRequestIds, send_module_tool_request,
};
use agent24_tools::{ModuleToolAuthorization, ModuleToolContext, ModuleToolRuntime};
use async_trait::async_trait;
use axum::body::Bytes;
use tokio_util::sync::CancellationToken;

const MAX_TOOL_RESPONSE_BYTES: usize = 64 * 1024;

pub struct AgentModuleToolRuntime {
    supervisors: Arc<OnceLock<Arc<Supervisors>>>,
    operations: Arc<HashSet<(String, String)>>,
    request_ids: KernelRequestIds,
}

impl AgentModuleToolRuntime {
    pub fn new(operations: HashSet<(String, String)>) -> Arc<Self> {
        Arc::new(Self {
            supervisors: Arc::new(OnceLock::new()),
            operations: Arc::new(operations),
            request_ids: KernelRequestIds::new_random(),
        })
    }

    pub fn supervisors_slot(&self) -> Arc<OnceLock<Arc<Supervisors>>> {
        Arc::clone(&self.supervisors)
    }

    fn generation(&self, module: &str) -> Result<Arc<Generation>, ModuleToolCallError> {
        let supervisors = self
            .supervisors
            .get()
            .ok_or(ModuleToolCallError::ModuleUnavailable)?;
        let current = supervisors
            .running_slot(module)
            .ok_or(ModuleToolCallError::ModuleUnavailable)?;
        let generation = current.get();
        if generation.state() != DrainState::Running {
            return Err(ModuleToolCallError::ModuleUnavailable);
        }
        Ok(generation)
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

pub struct AgentModuleToolAdvertView {
    runtime: Arc<AgentModuleToolRuntime>,
    authorizations: HashMap<(String, String), Arc<dyn ModuleToolAuthorization>>,
    remote_tier_present: bool,
}

impl AgentModuleToolAdvertView {
    pub fn new(
        runtime: Arc<AgentModuleToolRuntime>,
        authorizations: HashMap<(String, String), Arc<dyn ModuleToolAuthorization>>,
        remote_tier_present: bool,
    ) -> Arc<Self> {
        Arc::new(Self {
            runtime,
            authorizations,
            remote_tier_present,
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

    async fn blocked_by_remote_tier_guard(&self, module: &str, _operation: &str) -> bool {
        self.remote_tier_present && module == "documenting"
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use agent24_domain::tool::{ModuleToolCallError, ModuleToolResult};
    use agent24_policy::consent_gate::{
        ConsentGate, ConsentGateRequest, ModuleConsentAuthorization, StoreConsentGate,
    };
    use agent24_protocol::{RiskClass, ToolInfo};
    use agent24_store::{ConsentSource, HostRiskLevel, Store, ToolPermissionSummary};
    use agent24_tools::{
        ApprovalGate, GateDecision, ModuleTool, ModuleToolAuthorization, RiskOverrides,
        ToolContext, ToolRegistry,
    };
    use async_trait::async_trait;
    use serde_json::{Map, Value};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct FakeModule {
        running: AtomicBool,
        calls: AtomicUsize,
    }

    #[async_trait]
    impl ModuleToolRuntime for FakeModule {
        async fn check_available(&self, _: &str, _: &str) -> Result<(), ModuleToolCallError> {
            if self.running.load(Ordering::SeqCst) {
                Ok(())
            } else {
                Err(ModuleToolCallError::ModuleUnavailable)
            }
        }

        async fn invoke(
            &self,
            _: ModuleToolContext,
            _: Map<String, Value>,
            _: Duration,
            _: CancellationToken,
        ) -> Result<ModuleToolResult, ModuleToolCallError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(ModuleToolResult::Completed {
                payload: serde_json::json!({"ok": true}),
                replayed: false,
            })
        }
    }

    struct TestAdvertView {
        authorization: Arc<dyn ModuleToolAuthorization>,
        running: Arc<FakeModule>,
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
        async fn blocked_by_remote_tier_guard(&self, _: &str, _: &str) -> bool {
            false
        }
    }

    struct CountingApproval(AtomicUsize);

    struct RelaxModuleRisk;

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
            self.0.fetch_add(1, Ordering::SeqCst);
            GateDecision::Allow
        }
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
    ) -> (ToolRegistry, Arc<FakeModule>, Arc<CountingApproval>) {
        let store = Store::open_memory().await.unwrap();
        let summary = summary();
        if granted {
            store
                .grant_module_consent(&summary, "2026-10-08T00:00:00Z", "2026-11-08T00:00:00Z")
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
        };
        let gate: Arc<dyn ConsentGate> = Arc::new(StoreConsentGate::new(store));
        let authorization: Arc<dyn ModuleToolAuthorization> =
            Arc::new(ModuleConsentAuthorization::new(
                gate,
                request,
                Arc::new(|| "2026-10-08T00:00:01Z".to_owned()),
            ));
        let runtime = Arc::new(FakeModule {
            running: AtomicBool::new(running),
            calls: AtomicUsize::new(0),
        });
        let view: Arc<dyn ModuleToolAdvertView> = Arc::new(TestAdvertView {
            authorization: Arc::clone(&authorization),
            running: Arc::clone(&runtime),
        });
        let tool = ModuleTool::new(
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
        )
        .unwrap();
        let approvals = Arc::new(CountingApproval(AtomicUsize::new(0)));
        let mut registry = ToolRegistry::new().with_risk_overrides(Arc::new(RelaxModuleRisk));
        if risk.requires_approval() {
            registry = registry.with_gate(approvals.clone());
        }
        registry = registry.with_module_tool(Arc::new(tool));
        (registry, runtime, approvals)
    }

    fn context() -> ToolContext {
        ToolContext::legacy("run", None, None, "tool-call")
    }

    #[tokio::test]
    async fn granted_fake_module_is_advertised_and_callable() {
        let (registry, runtime, _) = fixture(RiskClass::Read, true, true).await;
        assert_eq!(registry.live_adverts().await.len(), 1);
        let result = registry
            .dispatch(
                "fake_module.write_local",
                &context(),
                &Map::new(),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.contains("\"ok\":true"));
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn missing_consent_is_not_advertised_and_never_reaches_fake_module() {
        let (registry, runtime, _) = fixture(RiskClass::Read, false, true).await;
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
    }

    #[tokio::test]
    async fn write_local_module_tool_asks_existing_approval_gate_every_call() {
        let (registry, runtime, approvals) = fixture(RiskClass::WriteLocal, true, true).await;
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
        assert_eq!(approvals.0.load(Ordering::SeqCst), 2);
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn stopped_module_tool_returns_module_unavailable_without_dispatch() {
        let (registry, runtime, _) = fixture(RiskClass::Read, true, false).await;
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
}
