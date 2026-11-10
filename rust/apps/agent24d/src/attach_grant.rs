//! The daemon-side [`AttachGrantBuilder`]. The attach registry asks this for
//! one admission's `provides` list and `MethodsFor`; it does not know about
//! model callbacks, the approval broker, or the event hub.
//!
//! Two call sites construct it, and they differ only in `models`:
//! `server::AppState::new` passes `None` (no `ModelCallbackDeps` exists yet),
//! and `serve_steps` replaces the registry later with `Some(model_deps)`.
//! `build` is the old `attach_registry::build_grant` body, moved unchanged.

use std::sync::Arc;

use agent24_domain::{Capability, DomainOsManifest, EventBroadcast, EventSink, Grants};

use crate::attach_registry::{AttachGrant, AttachGrantBuilder};

/// A3 P2 (design §1 "本期只用 `events`、`models`"): the only two capabilities
/// an attached module may be granted. Deliberately narrower than
/// `domain::KERNEL_OOP_GRANTS` — `memory`/`scheduler`/`approval` have no
/// attach-side story yet (no partition-allocation pass runs for an attached
/// module, and `host_commands` — A3's own reverse channel — is not one of
/// `agent24_domain::Capability`'s variants), so granting them here would be
/// scope no judgement or design section asks for. A manifest MAY still
/// declare them (`kernel_capabilities` is an open list at the manifest
/// layer); `Grants::granting`'s intersection with this list is what refuses
/// them, the same "asking gains nothing" rule every other grant list in this
/// daemon already documents.
const ATTACH_OOP_GRANTS: &[Capability] = &[Capability::Events, Capability::Models];

/// Daemon handles one admission needs. Dropping the registry's `AttachDeps`
/// drops this value, and with it the `ModelCallbackDeps` it owns — the same
/// shutdown schedule the registry used to get by holding those handles itself.
pub(crate) struct DaemonAttachGrantBuilder {
    pub(crate) scheduler: Arc<agent24_scheduler::Scheduler>,
    /// `None` when this daemon has no model deps to hand out — same meaning
    /// as `CallbackDeps.models`: an attach registration requesting `models`
    /// while this is `None` gets no grant, same outcome as not holding the
    /// capability.
    pub(crate) models: Option<crate::model_callback::ModelCallbackDeps>,
    pub(crate) approval_broker: Arc<crate::module_approval_broker::ModuleApprovalBroker>,
    pub(crate) events: crate::events::EventsHub,
}

impl AttachGrantBuilder for DaemonAttachGrantBuilder {
    fn build(&self, name: &str, manifest: &DomainOsManifest) -> AttachGrant {
        let granted = Grants::granting(manifest.kernel_capabilities(), ATTACH_OOP_GRANTS);
        let broadcast: Arc<dyn EventBroadcast> =
            Arc::new(crate::domain::HubBroadcast(self.events.clone()));
        let event_sink = granted
            .has(Capability::Events)
            .then(|| Arc::new(EventSink::new(manifest, broadcast)));
        // §4.3 C2's positive control: `_a24/model/` is listed in `provides` only
        // when `model_grant` actually built (deps present AND the capability was
        // granted) — never merely because the manifest asked for `models`.
        let model_grant = crate::model_callback::model_grant(
            name,
            manifest.model_access(),
            &granted,
            self.models.as_ref(),
        );
        let mut provides = Vec::new();
        if granted.has(Capability::Events) {
            provides.push("_a24/events/".to_owned());
        }
        if model_grant.is_some() {
            provides.push("_a24/model/".to_owned());
        }
        let methods_for = crate::domain::build_methods_for(
            name.to_owned(),
            granted,
            event_sink,
            self.approval_broker.clone(),
            // A3 P2 has no memory story (see `ATTACH_OOP_GRANTS`'s doc) — every
            // attached module gets `MemoryEntitlement::NONE`, same as a mounted
            // package that was never granted `Memory` or whose lend failed.
            crate::os_memory::MemoryEntitlement::NONE,
            self.scheduler.clone(),
            model_grant,
        );
        AttachGrant {
            provides,
            methods_for,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::sync::Arc;

    use agent24_os_proto::drain::Generation;
    use agent24_os_proto::rpc::{Dispatch, Methods, code};

    use super::*;

    struct NoopTrigger;

    #[async_trait::async_trait]
    impl agent24_scheduler::RunTrigger for NoopTrigger {
        async fn trigger(
            &self,
            _invocation: &agent24_scheduler::ScheduleInvocation,
        ) -> agent24_scheduler::FireOutcome {
            agent24_scheduler::FireOutcome::Deferred {
                reason: agent24_scheduler::DeferReason::MountPending,
            }
        }
    }

    /// The method names `build_methods_for` registers unconditionally. Both
    /// construction paths (models present / absent) must hand back this set;
    /// capability gating stays inside the handlers.
    const METHOD_NAMES: &[&str] = &[
        "_a24/approval/advise",
        "_a24/approval/gate",
        "_a24/approval/status",
        "_a24/events/emit",
        "_a24/memory/private/recall",
        "_a24/memory/private/recent",
        "_a24/memory/private/remember",
        "_a24/model/complete",
        "_a24/scheduler/delete",
        "_a24/scheduler/list",
        "_a24/scheduler/upsert",
    ];

    fn manifest(caps: &str) -> DomainOsManifest {
        DomainOsManifest::from_yaml(&format!(
            "name: agentear\nversion: \"1\"\nroute_namespace: /api/v1/agentear\n\
             event_module: agentear\ndata_dir: ~/.agent24/os/agentear/\n\
             impl_kind: attached_process\nkernel_capabilities: [{caps}]\n"
        ))
        .unwrap()
    }

    async fn builder(with_models: bool) -> DaemonAttachGrantBuilder {
        let store = agent24_store::Store::open_memory().await.unwrap();
        let events = crate::events::EventsHub::default();
        DaemonAttachGrantBuilder {
            scheduler: agent24_scheduler::Scheduler::new(
                store.clone(),
                Arc::new(NoopTrigger),
                Arc::new(|_| {}),
            ),
            models: with_models.then(|| crate::model_callback::ModelCallbackDeps {
                router: Arc::new(agent24_models::router::ModelRouter::with_defaults(vec![])),
                usage: Arc::new(crate::model_callback::MemoryUsageSink::default()),
                cancel_root: tokio_util::sync::CancellationToken::new(),
                admission: crate::model_callback::ModelAdmission::new(
                    crate::model_callback::MODEL_MAX_IN_FLIGHT_GLOBAL,
                    crate::model_callback::MODEL_MAX_IN_FLIGHT_PER_MODULE,
                ),
                events: events.clone(),
                timings: Arc::new(crate::timing_recorder::MemoryTimingSink::default()),
            }),
            approval_broker: crate::module_approval_broker::ModuleApprovalBroker::new(
                store,
                events.clone(),
            ),
            events,
        }
    }

    fn registered(methods: &Methods, name: &str) -> bool {
        let frame = format!(r#"{{"jsonrpc":"2.0","id":"1","method":"{name}","params":{{}}}}"#);
        match agent24_os_proto::rpc::dispatch(frame.as_bytes(), methods, &|_| false) {
            Dispatch::Call { .. } => true,
            Dispatch::Respond(resp) => !matches!(
                resp.outcome,
                Err(ref e) if e.code == code::METHOD_NOT_FOUND
            ),
            Dispatch::Cancel { .. } | Dispatch::Ignore => false,
        }
    }

    fn method_names(grant: &AttachGrant) -> Vec<String> {
        let generation = Generation::attached();
        let methods = (grant.methods_for)(&generation);
        METHOD_NAMES
            .iter()
            .filter(|name| registered(&methods, name))
            .map(|name| (*name).to_owned())
            .collect()
    }

    /// Locks the two production constructions (`models: None` in
    /// `AppState::new`, `models: Some` in `serve_steps`) to the old
    /// `build_grant` output: `provides` order, and the unconditional method set.
    #[tokio::test]
    async fn both_construction_paths_match_the_previous_grant() {
        let bare = builder(false).await;
        let wired = builder(true).await;
        let events_and_models = manifest("events, models");
        let events_only = manifest("events");
        let models_only = manifest("models");
        let neither = manifest("");

        let bare_both = bare.build("agentear", &events_and_models);
        assert_eq!(bare_both.provides, vec!["_a24/events/".to_owned()]);
        let wired_both = wired.build("agentear", &events_and_models);
        assert_eq!(
            wired_both.provides,
            vec!["_a24/events/".to_owned(), "_a24/model/".to_owned()]
        );

        assert_eq!(
            bare.build("agentear", &events_only).provides,
            vec!["_a24/events/".to_owned()]
        );
        assert_eq!(
            wired.build("agentear", &events_only).provides,
            vec!["_a24/events/".to_owned()]
        );
        assert!(bare.build("agentear", &models_only).provides.is_empty());
        assert_eq!(
            wired.build("agentear", &models_only).provides,
            vec!["_a24/model/".to_owned()]
        );
        assert!(bare.build("agentear", &neither).provides.is_empty());
        assert!(wired.build("agentear", &neither).provides.is_empty());

        let expected: Vec<String> = METHOD_NAMES.iter().map(|s| (*s).to_owned()).collect();
        assert_eq!(method_names(&bare_both), expected);
        assert_eq!(method_names(&wired_both), expected);
        let generation = Generation::attached();
        let methods = (wired_both.methods_for)(&generation);
        assert!(
            !registered(&methods, "_a24/not-a-method"),
            "an unknown method must stay unregistered"
        );
    }
}
