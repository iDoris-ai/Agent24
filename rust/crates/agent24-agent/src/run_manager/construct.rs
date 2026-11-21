use std::collections::HashMap;
use std::sync::Arc;
#[cfg(any(test, feature = "test-support"))]
use std::sync::atomic::{AtomicU32, Ordering};

use tokio::sync::Mutex;

use agent24_core::util::ulid;
use agent24_models::router::{ModelRouter, TaskProfile};
use agent24_protocol::Run;
use agent24_store::{RunAdmission, Store, WorkspaceInstant, WorkspaceLeaseId, WorkspaceStoreError};
use agent24_tools::{ToolContext, ToolRegistry};
use agent24_workspace::WorkspaceService;
use tokio_util::sync::CancellationToken;

use super::RunManager;
use crate::skill_inject;
use crate::{AgentError, EventSink, SessionMemory, merge_source_policy};

/// Refuses to run a tool for a run whose pinned skill has lost its label.
struct SkillLabelGuard {
    store: Store,
    run_id: String,
}

#[async_trait::async_trait]
impl agent24_tools::ExecutionGuard for SkillLabelGuard {
    async fn permit(&self) -> Result<(), String> {
        if skill_inject::label_missing(&self.store, &self.run_id).await {
            Err("skill source label unavailable".to_owned())
        } else {
            Ok(())
        }
    }
}

impl RunManager {
    // Keep the invalid version case explicit: mapping it to zero makes the
    // source fail `EgressRequest::authorize` instead of widening access.
    #[allow(clippy::manual_unwrap_or, clippy::manual_unwrap_or_default)]
    pub(crate) fn egress_policy_version(policy_version: i64) -> u64 {
        match u64::try_from(policy_version) {
            Ok(version) => version,
            Err(_) => 0,
        }
    }

    pub async fn task_profile_for_run(&self, run_id: &str, base: TaskProfile) -> TaskProfile {
        let mode = match self.store.run_policy_snapshot(run_id).await {
            Ok(snapshot) => snapshot.effective_mode,
            Err(err) => {
                tracing::warn!(
                    run_id,
                    error = %err,
                    "run source policy lookup failed; restricting model call to LocalOnly"
                );
                agent24_store::SourceMode::LocalOnly
            }
        };
        merge_source_policy(base, mode)
    }

    pub(crate) async fn persist_new_run(&self, run: &Run) -> Result<(), AgentError> {
        let Some(_) = run.workspace_id.as_ref() else {
            self.store.insert_run(run).await?;
            return Ok(());
        };
        let lease_id = WorkspaceLeaseId::parse(&format!("wl_{}", ulid()))?;
        let acquired_at = WorkspaceInstant::parse(&run.created_at)?;
        match self
            .store
            .insert_run_with_workspace_admission(run, Some(lease_id), &acquired_at)
            .await?
        {
            RunAdmission::Admitted { lease_id: Some(_) } => Ok(()),
            RunAdmission::Admitted { lease_id: None } => Err(WorkspaceStoreError::CorruptRow {
                table: "workspace_leases",
                field: "row",
            }
            .into()),
            RunAdmission::Denied(denial) => Err(AgentError::WorkspaceAdmission(denial)),
        }
    }

    pub fn new(
        store: Store,
        router: Arc<ModelRouter>,
        tools: Arc<ToolRegistry>,
        sink: Arc<dyn EventSink>,
        shutdown: CancellationToken,
    ) -> Arc<Self> {
        Self::with_memory_and_workspace(store, router, tools, sink, shutdown, None, None)
    }

    /// Build with optional per-session conversation memory (D1).
    pub fn with_memory(
        store: Store,
        router: Arc<ModelRouter>,
        tools: Arc<ToolRegistry>,
        sink: Arc<dyn EventSink>,
        shutdown: CancellationToken,
        memory: Option<SessionMemory>,
    ) -> Arc<Self> {
        Self::with_memory_and_workspace(store, router, tools, sink, shutdown, memory, None)
    }

    /// Build with optional session memory and workspace authority service.
    pub fn with_memory_and_workspace(
        store: Store,
        router: Arc<ModelRouter>,
        tools: Arc<ToolRegistry>,
        sink: Arc<dyn EventSink>,
        shutdown: CancellationToken,
        memory: Option<SessionMemory>,
        workspace: Option<Arc<WorkspaceService>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            router,
            tools,
            workspace,
            sink,
            memory,
            #[cfg(test)]
            test_egress_policy: None,
            shutdown,
            cancels: Mutex::new(HashMap::new()),
            inbound_origins: Mutex::new(HashMap::new()),
            knowledge_queries: Mutex::new(HashMap::new()),
            #[cfg(any(test, feature = "test-support"))]
            inbound_start_faults: AtomicU32::new(0),
        })
    }

    /// Fail the next [`Self::start_run_inbound`] before it writes a run.
    ///
    /// The inbound event row is already committed by the caller. This is the
    /// transient start error that marks that row `start_failed`. Not present
    /// in a release build: the method exists only under `cfg(test)` or the
    /// `test-support` feature.
    #[cfg(any(test, feature = "test-support"))]
    pub fn fail_next_inbound_start(&self) {
        self.inbound_start_faults.fetch_add(1, Ordering::AcqRel);
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn take_inbound_start_fault(&self) -> bool {
        let mut current = self.inbound_start_faults.load(Ordering::Acquire);
        while current > 0 {
            match self.inbound_start_faults.compare_exchange(
                current,
                current - 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(observed) => current = observed,
            }
        }
        false
    }

    pub(crate) async fn tool_context_for(
        &self,
        run: &Run,
        tool_call_id: impl Into<String>,
    ) -> Result<ToolContext, AgentError> {
        let tool_call_id = tool_call_id.into();
        let lease_id = self.store.active_workspace_run_lease_id(&run.id).await?;
        let context = if run.workspace_id.is_none() {
            if lease_id.is_some() {
                return Err(WorkspaceStoreError::CorruptRow {
                    table: "runs",
                    field: "workspace_id",
                }
                .into());
            }
            ToolContext::legacy(
                run.id.clone(),
                run.session_id.clone(),
                run.schedule_id.clone(),
                tool_call_id,
            )
        } else {
            let lease_id = lease_id.ok_or(WorkspaceStoreError::CorruptRow {
                table: "workspace_leases",
                field: "row",
            })?;
            let service = self
                .workspace
                .as_ref()
                .ok_or(AgentError::WorkspaceServiceUnavailable)?;
            let authority = service.bind_run_authority(&run.id, &lease_id).await?;
            ToolContext::workspace_bound(
                run.id.clone(),
                run.session_id.clone(),
                run.schedule_id.clone(),
                tool_call_id,
                authority,
            )
        };
        // Asked again right before any tool runs, after an approval wait too.
        let context = context.with_execution_guard(Arc::new(SkillLabelGuard {
            store: self.store.clone(),
            run_id: run.id.clone(),
        }));
        let context = match self.store.run_policy_snapshot(&run.id).await {
            // A pinned skill without its label is never read as Ordinary.
            Ok(_) if self.skill_label_missing(&run.id).await => {
                context.with_egress_policy_unavailable()
            }
            Ok(snapshot) if snapshot.sources.is_empty() => context.with_egress_policy_unavailable(),
            Ok(snapshot) => {
                let resources = snapshot
                    .sources
                    .into_iter()
                    // ADR-K1-02 §2.1.1: only the host-assigned classification
                    // decides whether a source is tool egress material, never
                    // its kind. `Ordinary` sources (user input today, ordinary
                    // recalled memory later) do not restrict tools; Restricted
                    // and Unknown sources, of any kind, keep the strict policy.
                    .filter(|source| {
                        source.classification != agent24_store::SourceClassification::Ordinary
                    })
                    .map(|source| agent24_domain::EgressResource {
                        resource_id: source.source_id,
                        revision: source.revision_digest,
                        local_only: source.mode == agent24_store::SourceMode::LocalOnly,
                        authorization_ref: source.authorization_ref,
                        policy_version: Self::egress_policy_version(source.policy_version),
                    })
                    .collect();
                context.with_egress_resources(resources)
            }
            Err(err) => {
                tracing::warn!(run_id = %run.id, error = %err, "run source policy lookup failed; restricting tool egress");
                context.with_egress_policy_unavailable()
            }
        };
        #[cfg(test)]
        let context = if let Some((gate, resources, generation)) = &self.test_egress_policy {
            context.with_egress_policy(Arc::clone(gate), resources.clone(), *generation)
        } else {
            context
        };
        Ok(context)
    }

    #[cfg(test)]
    pub(crate) fn with_test_egress_policy(
        mut self: Arc<Self>,
        gate: Arc<dyn agent24_domain::EgressGate>,
        resources: Vec<agent24_domain::EgressResource>,
        generation: u64,
    ) -> Arc<Self> {
        if let Some(manager) = Arc::get_mut(&mut self) {
            manager.test_egress_policy = Some((gate, resources, generation));
        }
        self
    }
}
