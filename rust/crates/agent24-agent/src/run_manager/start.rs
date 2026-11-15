use std::sync::Arc;

use agent24_core::util::{now_iso8601, ulid};
use agent24_protocol::{Run, RunCreate, RunInput, RunStatus};
use agent24_store::StoreError;

use super::{InboundOrigin, InboundRunStart, PersistOutcome, RunManager, RunStart};
use crate::SkillProviderSource;
use crate::skill_inject;
use crate::{AgentError, SkillAdmissionError, workspace_now, zero_usage};

impl RunManager {
    pub async fn start_run(self: &Arc<Self>, create: RunCreate) -> Result<Run, AgentError> {
        self.start_run_with_schedule(create, None).await
    }

    /// As [`start_run`], but tags the run with the schedule that fired it
    /// (the scheduler uses this so every run traces back to its trigger).
    pub async fn start_run_with_schedule(
        self: &Arc<Self>,
        create: RunCreate,
        schedule_id: Option<String>,
    ) -> Result<Run, AgentError> {
        self.ready_run(
            self.start_run_inner(create, schedule_id, |_| {}, None, None)
                .await?,
        )
    }

    /// As [`start_run`], with `before_execute` called once the run is stored
    /// and before it starts executing, so whatever it records about the run
    /// is there before the run's first step.
    pub async fn start_run_then(
        self: &Arc<Self>,
        create: RunCreate,
        before_execute: impl FnOnce(&Run),
    ) -> Result<Run, AgentError> {
        self.start_run_then_with(create, before_execute, None).await
    }

    /// As [`start_run_then`], with the lookup used to resolve the providers of
    /// the capabilities a skill requires (`create.skill_id`). Without one, a
    /// skill that requires any capability is refused as missing it.
    pub async fn start_run_then_with(
        self: &Arc<Self>,
        create: RunCreate,
        before_execute: impl FnOnce(&Run),
        providers: Option<&dyn SkillProviderSource>,
    ) -> Result<Run, AgentError> {
        self.ready_run(
            self.start_run_inner(create, None, before_execute, None, providers)
                .await?,
        )
    }

    /// Start a run for one inbound event. `origin` is not part of [`RunCreate`].
    ///
    /// The session, the run, and `inbound_runs` commit in one transaction
    /// ([`agent24_store::Store::insert_session_run_for_inbound`]).
    /// `persist_new_run` commits on its own, so this path does not call it.
    /// `execute` tags the user message [`agent24_store::SourceRef::inbound_external`].
    /// A second call for the same event links nothing and starts nothing.
    pub async fn start_run_inbound(
        self: &Arc<Self>,
        create: RunCreate,
        origin: InboundOrigin,
    ) -> Result<InboundRunStart, AgentError> {
        #[cfg(any(test, feature = "test-support"))]
        if self.take_inbound_start_fault() {
            return Err(AgentError::Store(StoreError::Conflict(
                "injected inbound start failure".to_owned(),
            )));
        }
        match self
            .start_run_inner(create, None, |_| {}, Some(origin), None)
            .await?
        {
            RunStart::Ready(run) => Ok(InboundRunStart::Started(run)),
            RunStart::InboundAlreadyLinked => Ok(InboundRunStart::AlreadyLinked),
            RunStart::InboundNotStarting => Ok(InboundRunStart::NotStarting),
        }
    }

    pub(crate) fn ready_run(&self, started: RunStart) -> Result<Run, AgentError> {
        match started {
            RunStart::Ready(run) => Ok(run),
            RunStart::InboundAlreadyLinked => Err(AgentError::Store(StoreError::Conflict(
                "ordinary start linked an inbound event".to_owned(),
            ))),
            RunStart::InboundNotStarting => Err(AgentError::Store(StoreError::Conflict(
                "ordinary start saw an inbound event that is not starting".to_owned(),
            ))),
        }
    }

    pub(crate) async fn start_run_inner(
        self: &Arc<Self>,
        create: RunCreate,
        schedule_id: Option<String>,
        before_execute: impl FnOnce(&Run),
        inbound: Option<InboundOrigin>,
        providers: Option<&dyn SkillProviderSource>,
    ) -> Result<RunStart, AgentError> {
        // Skill admission comes first and writes nothing. Inbound runs,
        // self-wake runs and workspace-bound runs cannot carry a skill.
        let skill_pin = match create.skill_id.as_deref() {
            None => None,
            Some(skill_id) => {
                let reason = if inbound.is_some() {
                    Some("inbound runs cannot carry skill_id")
                } else if schedule_id.is_some() {
                    Some("scheduled runs cannot carry skill_id")
                } else if create.workspace_id.is_some() {
                    Some("workspace-bound runs cannot carry skill_id in v1")
                } else if let Some(session_id) = create.session_id.as_deref()
                    && self
                        .store
                        .get_session(session_id)
                        .await?
                        .is_some_and(|session| session.workspace_id.is_some())
                {
                    Some("workspace-bound sessions cannot carry skill_id in v1")
                } else {
                    None
                };
                if let Some(reason) = reason {
                    return Err(SkillAdmissionError::NotAdmitted { reason }.into());
                }
                let admitted = skill_inject::admit(&self.store, providers, skill_id).await?;
                Some(agent24_store::SkillPinRequest {
                    skill_id: skill_id.to_owned(),
                    expected_body_revision_id: admitted.body_revision_id,
                    providers_json: admitted.providers_json,
                })
            }
        };
        // Inbound creates the session in the same transaction as the run.
        // Checking first would always miss it.
        if inbound.is_none()
            && let Some(session_id) = &create.session_id
            && self.store.get_session(session_id).await?.is_none()
        {
            return Err(AgentError::SessionNotFound(session_id.clone()));
        }

        let created_at = if create.workspace_id.is_some() {
            workspace_now()?
        } else {
            now_iso8601()
        };
        let run = Run {
            id: format!("run_{}", ulid()),
            session_id: create.session_id.clone(),
            workspace_id: create.workspace_id.clone(),
            status: RunStatus::Queued,
            input: RunInput {
                prompt: create.prompt,
                workspace_id: create.workspace_id,
                model_override: create.model_override,
                mode: create.mode,
                knowledge_binding_id: create.knowledge_binding_id,
            },
            output: None,
            error: None,
            usage: zero_usage(),
            schedule_id,
            created_at,
            started_at: None,
            ended_at: None,
        };
        // Token registered BEFORE the row becomes discoverable — a client
        // racing list_runs+cancel can never observe a token-less live run
        let token = self.shutdown.child_token();
        self.cancels
            .lock()
            .await
            .insert(run.id.clone(), token.clone());
        let persisted = match self
            .persist_created_run(&run, inbound.as_ref(), skill_pin.as_ref())
            .await
        {
            Ok(outcome) => outcome,
            Err(err) => {
                self.cancels.lock().await.remove(&run.id);
                return Err(err);
            }
        };
        match persisted {
            PersistOutcome::Stored => {}
            PersistOutcome::InboundAlreadyLinked => {
                self.cancels.lock().await.remove(&run.id);
                return Ok(RunStart::InboundAlreadyLinked);
            }
            PersistOutcome::InboundNotStarting => {
                self.cancels.lock().await.remove(&run.id);
                return Ok(RunStart::InboundNotStarting);
            }
            PersistOutcome::SkillNotActive => {
                self.cancels.lock().await.remove(&run.id);
                return Err(SkillAdmissionError::NotActive.into());
            }
            PersistOutcome::SkillMoved => {
                self.cancels.lock().await.remove(&run.id);
                return Err(AgentError::Store(StoreError::SkillPointerConflict));
            }
        }
        if let Some(origin) = &inbound {
            // Before spawn, so execute cannot finish (and try to mark done)
            // while the row is still `starting`.
            self.inbound_origins
                .lock()
                .await
                .insert(run.id.clone(), origin.event_id.clone());
            match self.store.mark_inbound_started(&origin.event_id).await {
                Ok(true) => {}
                Ok(false) => tracing::warn!(
                    event_id = %origin.event_id,
                    "inbound mark started matched no starting row"
                ),
                Err(err) => tracing::warn!(
                    event_id = %origin.event_id,
                    error = %err,
                    "inbound mark started failed"
                ),
            }
        }
        before_execute(&run);

        // Supervised execution: execute() runs in its OWN task whose join
        // result is observed. A panic anywhere in the execution path (the
        // ModelProvider trait object is effectively arbitrary code) must
        // still land the run in a terminal state and clean the cancels map —
        // otherwise the run is wedged non-terminal for the process lifetime
        // and cancel_run cannot recover it (review #36).
        let manager = Arc::clone(self);
        let run_id = run.id.clone();
        tokio::spawn(async move {
            let task = tokio::spawn({
                let manager = Arc::clone(&manager);
                let run_id = run_id.clone();
                async move {
                    manager
                        .execute(run_id, token, create.idoris_headroom_confirmed)
                        .await
                }
            });
            if let Err(err) = task.await
                && err.is_panic()
            {
                tracing::error!("run {run_id}: execution task panicked");
                manager
                    .finish_failed(&run_id, "internal", "run execution panicked")
                    .await;
            }
            manager.cancels.lock().await.remove(&run_id);
        });

        Ok(RunStart::Ready(run))
    }

    pub(crate) async fn persist_created_run(
        &self,
        run: &Run,
        inbound: Option<&InboundOrigin>,
        skill_pin: Option<&agent24_store::SkillPinRequest>,
    ) -> Result<PersistOutcome, AgentError> {
        if let Some(pin) = skill_pin {
            // The pin is derived from the pointer in the same transaction as
            // the run row. Admission already refused inbound.
            return Ok(
                match self.store.insert_run_with_skill_pin(run, pin).await? {
                    agent24_store::SkillPinOutcome::Pinned(_) => PersistOutcome::Stored,
                    agent24_store::SkillPinOutcome::NotActive => PersistOutcome::SkillNotActive,
                    agent24_store::SkillPinOutcome::PointerMoved => PersistOutcome::SkillMoved,
                },
            );
        }
        let Some(origin) = inbound else {
            self.persist_new_run(run).await?;
            return Ok(PersistOutcome::Stored);
        };
        let session_id = run.session_id.clone().ok_or_else(|| {
            AgentError::Store(StoreError::Conflict(
                "inbound run requires a session".to_owned(),
            ))
        })?;
        if origin.channel.is_empty() || origin.event_id.is_empty() {
            return Err(AgentError::Store(StoreError::Conflict(
                "inbound origin is empty".to_owned(),
            )));
        }
        let session = agent24_protocol::Session {
            id: session_id,
            title: String::new(),
            channel: origin.channel.clone(),
            workspace_id: None,
            created_at: run.created_at.clone(),
            updated_at: run.created_at.clone(),
        };
        match self
            .store
            .insert_session_run_for_inbound(&session, run, &origin.event_id)
            .await?
        {
            agent24_store::InboundRunLink::Linked => Ok(PersistOutcome::Stored),
            agent24_store::InboundRunLink::AlreadyLinked => {
                Ok(PersistOutcome::InboundAlreadyLinked)
            }
            agent24_store::InboundRunLink::NotStarting => Ok(PersistOutcome::InboundNotStarting),
        }
    }
}
