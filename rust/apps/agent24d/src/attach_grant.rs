//! The daemon-side [`AttachGrantBuilder`]. The attach registry asks this for
//! one admission's `provides` list and `MethodsFor`; it does not know about
//! model callbacks, the approval broker, or the event hub.
//!
//! Two call sites construct it, and they differ only in `models`:
//! `server::AppState::new` passes `None` (no `ModelCallbackDeps` exists yet),
//! and `serve_steps` replaces the registry later with `Some(model_deps)`.
//!
//! `DaemonAttachGrantBuilder::build` holds the admission body. The registry
//! still has `build_grant`: a thin wrapper that calls this trait and stores
//! the result as the handshake `Offer` plus the `MethodsFor` reused across
//! reconnects. That wrapper did not move with the body.

use std::sync::Arc;

use agent24_domain::{Capability, DomainOsManifest, EventBroadcast, EventSink};

use agent24_attach::attach_registry::{AttachGrant, AttachGrantBuilder};

use crate::capabilities::requires::{self, Consent, GrantForm, RequiresSpec};

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
        let granted = requires::resolve(
            &RequiresSpec::from_manifest(manifest),
            GrantForm::Attach.host_cap(),
            Consent::FirstPartyEnabled,
        )
        .granted;
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
    use agent24_os_proto::rpc::{Dispatch, ErrorKind, Methods, RpcError, code};

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
    /// construction paths install this set; it is not an authorization check
    /// (`build_methods_for` registers all eleven whether or not the capability
    /// was granted). Compared as a sorted list against every name actually registered,
    /// so one extra method fails the assertion. The frame assertions below
    /// are the authorization check.
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
        (grant.methods_for)(&generation).names()
    }

    /// Send one frame through the grant's real `methods_for` and run the
    /// handler `dispatch` selects. Same shape as the frame+`dispatch` calls
    /// in `domain`'s scheduler/model restart tests.
    async fn call_granted(
        grant: &AttachGrant,
        id: &str,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, RpcError> {
        let generation = Generation::attached();
        let methods = (grant.methods_for)(&generation);
        let frame = serde_json::to_vec(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))
        .unwrap();
        match agent24_os_proto::rpc::dispatch(&frame, &methods, &|_| false) {
            Dispatch::Call {
                handler, params, ..
            } => handler.call(params).await,
            Dispatch::Respond(resp) => resp.outcome,
            Dispatch::Cancel { .. } | Dispatch::Ignore => {
                panic!("unexpected dispatch outcome for {method}")
            }
        }
    }

    /// Unauthorized is application `-32000` / `forbidden`. A missing
    /// registration would be `METHOD_NOT_FOUND` (`-32601`) instead — these
    /// methods are registered unconditionally, and the refusal lives in the
    /// handler.
    fn assert_forbidden(method: &str, err: &RpcError) {
        assert_eq!(
            (err.code, err.kind),
            (code::APPLICATION, Some(ErrorKind::Forbidden)),
            "{method} must be refused as unauthorized, not METHOD_NOT_FOUND ({code}): {err:?}",
            code = code::METHOD_NOT_FOUND,
        );
    }

    /// Locks this implementation's authorization semantics for both production
    /// constructions (`models: None` in `AppState::new`, `models: Some` in
    /// `serve_steps`): `provides` order, the `ATTACH_OOP_GRANTS` filter, and
    /// the refusal a real frame gets from `methods_for`.
    #[tokio::test]
    async fn both_construction_paths_lock_this_implementations_authorization() {
        let bare = builder(false).await;
        let wired = builder(true).await;
        let events_and_models = manifest("events, models");
        let also_unbacked = manifest("events, models, memory, scheduler, approval");
        let events_only = manifest("events");
        let models_only = manifest("models");
        let neither = manifest("");

        // `ATTACH_OOP_GRANTS` drops memory/scheduler/approval, so asking for
        // them changes nothing `provides` records.
        assert_eq!(
            bare.build("agentear", &also_unbacked).provides,
            bare.build("agentear", &events_and_models).provides,
        );
        assert_eq!(
            wired.build("agentear", &also_unbacked).provides,
            wired.build("agentear", &events_and_models).provides,
        );

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

        let emit_params = serde_json::json!({"kind": "task.transitioned", "payload": {}});
        for (label, grant) in [
            ("bare", bare.build("agentear", &neither)),
            ("wired", wired.build("agentear", &neither)),
        ] {
            let err = call_granted(&grant, "e1", "_a24/events/emit", emit_params.clone())
                .await
                .expect_err("events was not granted");
            assert_forbidden(&format!("{label} _a24/events/emit"), &err);
        }

        let complete_params = serde_json::json!({"messages": [{"role": "user", "content": "hi"}]});
        let err = call_granted(
            &bare.build("agentear", &events_and_models),
            "m1",
            "_a24/model/complete",
            complete_params,
        )
        .await
        .expect_err("models: None grants no model access");
        assert_forbidden("bare _a24/model/complete", &err);

        let memory_calls = [
            (
                "_a24/memory/private/remember",
                serde_json::json!({"kind": "note", "body": {}}),
            ),
            (
                "_a24/memory/private/recall",
                serde_json::json!({"query": "note", "page_size": 1}),
            ),
            (
                "_a24/memory/private/recent",
                serde_json::json!({"page_size": 1}),
            ),
        ];
        for (label, grant) in [
            ("bare", bare.build("agentear", &also_unbacked)),
            ("wired", wired.build("agentear", &also_unbacked)),
        ] {
            for (method, params) in &memory_calls {
                let err = call_granted(&grant, "mem", method, params.clone())
                    .await
                    .expect_err("MemoryEntitlement::NONE");
                assert_forbidden(&format!("{label} {method}"), &err);
            }
        }
    }

    /// FU-118: `build` never writes `_a24/scheduler/` or `_a24/approval/` into
    /// `provides`, so deleting the `ATTACH_OOP_GRANTS` intersection leaves the
    /// `provides` equality in
    /// `both_construction_paths_lock_this_implementations_authorization` green.
    /// The refusal is the handler `methods_for` actually runs. A manifest that
    /// declares `scheduler` and `approval` must still get `forbidden` here.
    /// Granting the requested set with no intersection makes this red: the
    /// handler then gets past the capability check.
    #[tokio::test]
    async fn declared_scheduler_and_approval_are_refused_through_methods_for() {
        let declared = manifest("events, models, memory, scheduler, approval");
        let calls = [
            ("_a24/scheduler/list", serde_json::json!({})),
            (
                "_a24/scheduler/delete",
                serde_json::json!({"key": "probe.key"}),
            ),
            (
                "_a24/scheduler/upsert",
                serde_json::json!({"key": "probe.key", "spec": {"type": "every", "secs": 3600}}),
            ),
            (
                "_a24/approval/status",
                serde_json::json!({"approval_id": "appr_missing"}),
            ),
            (
                "_a24/approval/advise",
                serde_json::json!({
                    "action": "note",
                    "payload": {},
                    "request_id": "req-1",
                    "approval_token": "tok",
                }),
            ),
        ];
        for (label, with_models) in [("bare", false), ("wired", true)] {
            let grant = builder(with_models).await.build("agentear", &declared);
            for (method, params) in &calls {
                let err = call_granted(&grant, "fu118", method, params.clone())
                    .await
                    .expect_err("ATTACH_OOP_GRANTS drops scheduler and approval");
                assert_forbidden(&format!("{label} {method}"), &err);
            }
        }
    }
}
