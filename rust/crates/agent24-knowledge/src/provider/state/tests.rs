#![allow(clippy::unwrap_used)]

use super::*;
use DataOperation::{Read, Search};
use LocalReason as R;

fn ready_response(live: bool, search: &str, read: &str) -> ReadyResponse {
    serde_json::from_value(serde_json::json!({
        "protocol_version": 1, "request_id": "r1", "generation": 3, "live": live,
        "operations": {
            "search": { "state": search }, "read": { "state": read },
            "ingest": { "state": "disabled" }
        }
    }))
    .unwrap()
}

fn answered(ready: &ReadyResponse) -> ConnectionPhase {
    judge_connection(Probe::answered(ready))
}

fn bound(phase: ConnectionPhase) -> Binding {
    Binding {
        retired: false,
        activated: true,
        phase,
        persist_pending: false,
        reverify_pending: false,
    }
}

fn on() -> Switches {
    Switches::default()
}

const OFF: Switches = Switches {
    off: true,
    profile_disabled: false,
};
const PROFILE_DISABLED: Switches = Switches {
    off: false,
    profile_disabled: true,
};
const PLACEHOLDER: Target = Target::Placeholder {
    failure: None,
    unsaved_settings: false,
};

fn refused(reason: LocalReason, retryable: bool) -> Result<(), Refusal> {
    Err(Refusal { reason, retryable })
}

#[test]
fn the_connection_phase_follows_capabilities_liveness_and_search_and_read_only() {
    use ConnectionPhase::{Degraded, Unavailable};
    use UnavailableCause as U;
    assert_eq!(
        answered(&ready_response(true, "ready", "ready")),
        ConnectionPhase::Ready
    );
    assert_eq!(
        answered(&ready_response(true, "ready", "not_ready")),
        Degraded { servable: Search }
    );
    assert_eq!(
        answered(&ready_response(true, "disabled", "ready")),
        Degraded { servable: Read }
    );
    assert_eq!(
        answered(&ready_response(true, "not_ready", "disabled")),
        Unavailable(U::NotReady)
    );
    // Not live is a dependency, whatever the operations say.
    assert_eq!(
        answered(&ready_response(false, "ready", "ready")),
        Unavailable(U::Dependency)
    );
    // Incompatible is known before any readiness answer (R9.4).
    assert_eq!(
        judge_connection(Probe::Incompatible),
        Unavailable(U::Incompatible)
    );
    assert_eq!(
        judge_connection(Probe::NoAnswer),
        Unavailable(U::Dependency)
    );
    // A state the decoder did not know arrives as not ready (R6.2).
    let unknown_read = Probe::Answered {
        live: true,
        search_ready: true,
        read_ready: false,
    };
    assert_eq!(
        judge_connection(unknown_read),
        Degraded { servable: Search }
    );
    // `ingest` and the per-KB readiness are not read.
    let mut ready = ready_response(true, "ready", "ready");
    ready.knowledge_bases = Some(
        serde_json::from_value(serde_json::json!([
            { "knowledge_base_id": "kb1", "score_space": "s1",
              "search": { "state": "not_ready", "reason": "index_building" },
              "read": { "state": "not_ready", "reason": "index_building" } }
        ]))
        .unwrap(),
    );
    assert_eq!(answered(&ready), ConnectionPhase::Ready);
}

#[test]
fn only_dependency_and_not_ready_retry_by_themselves() {
    use UnavailableCause as U;
    for (cause, retries) in [
        (U::Dependency, true),
        (U::NotReady, true),
        (U::Incompatible, false),
        (U::InstanceUnidentified, false),
        (U::CredentialRejected, false),
        (U::AccessRevoked, false),
        (U::LocalStorage, false),
    ] {
        assert_eq!(cause.retries_automatically(), retries, "{cause:?}");
    }
}

#[test]
fn the_state_is_disabled_then_registered_then_the_connection_phase() {
    let ready = Target::Binding(bound(ConnectionPhase::Ready));
    assert_eq!(
        derive(on(), ready),
        Some(ServiceState::Connected(ConnectionPhase::Ready))
    );
    // Either switch disables, both too, and they win over everything else.
    for switches in [
        OFF,
        PROFILE_DISABLED,
        Switches {
            off: true,
            profile_disabled: true,
        },
    ] {
        assert_eq!(derive(switches, ready), Some(ServiceState::Disabled));
        assert_eq!(derive(switches, PLACEHOLDER), Some(ServiceState::Disabled));
    }
    // Not activated (awaiting confirmation, not selected) and the placeholder
    // are registered.
    let idle = Target::Binding(Binding {
        activated: false,
        ..bound(ConnectionPhase::Ready)
    });
    assert_eq!(derive(on(), idle), Some(ServiceState::Registered));
    assert_eq!(derive(on(), PLACEHOLDER), Some(ServiceState::Registered));
    // A retired binding has no service state, switches or not.
    let retired = Target::Binding(Binding {
        retired: true,
        ..bound(ConnectionPhase::Ready)
    });
    assert_eq!(derive(on(), retired), None);
    assert_eq!(derive(OFF, retired), None);
    // `persist_pending` takes no part in the derivation (S1.7).
    let pending = Target::Binding(Binding {
        persist_pending: true,
        ..bound(ConnectionPhase::Loading)
    });
    assert_eq!(
        derive(on(), pending),
        Some(ServiceState::Connected(ConnectionPhase::Loading))
    );
}

#[test]
fn only_a_servable_binding_sends_content_requests_and_some_send_nothing() {
    use ConnectionPhase::{Degraded, Loading, Ready, Unavailable};
    use Outgoing::{Data, Probe};
    use UnavailableCause as U;
    let send =
        |switches, phase, outgoing| may_send(switches, Target::Binding(bound(phase)), outgoing);
    assert!(send(on(), Ready, Data(Search)));
    assert!(send(on(), Ready, Data(Read)));
    assert!(send(on(), Ready, Probe));
    assert!(send(on(), Degraded { servable: Read }, Data(Read)));
    assert!(!send(on(), Degraded { servable: Read }, Data(Search)));
    // `loading` and a retrying `unavailable` only probe.
    for phase in [
        Loading,
        Unavailable(U::Dependency),
        Unavailable(U::NotReady),
    ] {
        assert!(send(on(), phase, Probe), "{phase:?}");
        assert!(!send(on(), phase, Data(Search)), "{phase:?}");
    }
    // A reason that waits for the user, and `local_storage`, send nothing.
    for cause in [
        U::Incompatible,
        U::InstanceUnidentified,
        U::CredentialRejected,
        U::AccessRevoked,
        U::LocalStorage,
    ] {
        assert!(!send(on(), Unavailable(cause), Probe), "{cause:?}");
    }
    // Nor does a binding with `persist_pending`, a disabled or registered one,
    // a retired one, or the placeholder.
    let pending = Binding {
        persist_pending: true,
        ..bound(Ready)
    };
    assert!(!may_send(on(), Target::Binding(pending), Probe));
    assert!(!may_send(on(), Target::Binding(pending), Data(Search)));
    assert!(!send(OFF, Ready, Probe));
    assert!(!send(PROFILE_DISABLED, Ready, Data(Read)));
    let idle = Binding {
        activated: false,
        ..bound(Ready)
    };
    assert!(!may_send(on(), Target::Binding(idle), Probe));
    let retired = Binding {
        retired: true,
        ..bound(Ready)
    };
    assert!(!may_send(on(), Target::Binding(retired), Probe));
    assert!(!may_send(on(), PLACEHOLDER, Probe));
}

#[test]
fn a_new_call_gets_the_first_row_of_the_table_that_holds() {
    use ConnectionPhase::{Degraded, Loading, Ready, Unavailable};
    use UnavailableCause as U;
    let admit_on = |target, op| admit(on(), target, false, op);
    let ready = Target::Binding(bound(Ready));
    assert_eq!(admit_on(ready, Search), Ok(()));
    assert_eq!(admit_on(ready, Read), Ok(()));

    // Row 1: Off, over everything below it.
    let worst = Target::Binding(Binding {
        retired: true,
        persist_pending: true,
        ..bound(Unavailable(U::AccessRevoked))
    });
    assert_eq!(
        admit(OFF, worst, true, Search),
        refused(R::KnowledgeOff, false)
    );
    assert_eq!(
        admit(OFF, ready, false, Search),
        refused(R::KnowledgeOff, false)
    );

    // Row 2: `persist_pending`, or the run's binding not yet persisted; over
    // a disabled profile and a retired binding.
    assert_eq!(
        admit(PROFILE_DISABLED, worst, false, Search),
        refused(R::LocalStorage, false)
    );
    assert_eq!(
        admit(on(), ready, true, Search),
        refused(R::LocalStorage, false)
    );
    assert_eq!(
        admit(PROFILE_DISABLED, PLACEHOLDER, true, Read),
        refused(R::LocalStorage, false)
    );

    // Row 3: only the profile disabled; retired; the run's binding inactive.
    assert_eq!(
        admit(PROFILE_DISABLED, ready, false, Search),
        refused(R::ProfileInactive, false)
    );
    let retired = Target::Binding(Binding {
        retired: true,
        ..bound(Ready)
    });
    assert_eq!(
        admit_on(retired, Search),
        refused(R::ProfileInactive, false)
    );
    let failed = |failure, unsaved_settings| Target::Placeholder {
        failure: Some(failure),
        unsaved_settings,
    };
    assert_eq!(
        admit(
            PROFILE_DISABLED,
            failed(PlaceholderFailure::CredentialRejected, false),
            false,
            Read
        ),
        refused(R::ProfileInactive, false)
    );

    // Row 4: the placeholder's recorded failure.
    assert_eq!(
        admit_on(failed(PlaceholderFailure::Dependency, false), Search),
        refused(R::Dependency, true)
    );
    assert_eq!(
        admit_on(failed(PlaceholderFailure::Dependency, true), Search),
        refused(R::Dependency, false)
    );
    assert_eq!(
        admit_on(
            failed(PlaceholderFailure::CredentialRejected, false),
            Search
        ),
        refused(R::CredentialRejected, false)
    );
    assert_eq!(
        admit_on(
            failed(PlaceholderFailure::InstanceUnidentified, false),
            Read
        ),
        refused(R::InstanceUnidentified, false)
    );

    // Row 5: any other registered: awaiting confirmation, not selected, the
    // placeholder without a failure.
    let idle = Target::Binding(Binding {
        activated: false,
        ..bound(Ready)
    });
    assert_eq!(admit_on(idle, Search), refused(R::ProfileInactive, false));
    assert_eq!(
        admit_on(PLACEHOLDER, Search),
        refused(R::ProfileInactive, false)
    );

    // Row 6: loading; the operation degraded does not serve; not_ready; a
    // re-verification pending, even when ready.
    assert_eq!(
        admit_on(Target::Binding(bound(Loading)), Search),
        refused(R::NotReady, true)
    );
    let degraded = Target::Binding(bound(Degraded { servable: Search }));
    assert_eq!(admit_on(degraded, Search), Ok(()));
    assert_eq!(admit_on(degraded, Read), refused(R::NotReady, true));
    assert_eq!(
        admit_on(Target::Binding(bound(Unavailable(U::NotReady))), Read),
        refused(R::NotReady, true)
    );
    let reverify = Target::Binding(Binding {
        reverify_pending: true,
        ..bound(Ready)
    });
    assert_eq!(admit_on(reverify, Search), refused(R::NotReady, true));

    // Each row on its own, not hidden by a row above (PR-Daemon #1221 G-1):
    // `persist_pending` on a binding that is not retired; the operation a
    // `Degraded { servable: Read }` binding serves; a re-verification over a
    // reason that does not retry; both switches on.
    let pending = Target::Binding(Binding {
        persist_pending: true,
        ..bound(Ready)
    });
    assert_eq!(admit_on(pending, Search), refused(R::LocalStorage, false));
    let read_only = Target::Binding(bound(Degraded { servable: Read }));
    assert_eq!(admit_on(read_only, Read), Ok(()));
    assert_eq!(admit_on(read_only, Search), refused(R::NotReady, true));
    let reverify_revoked = Target::Binding(Binding {
        reverify_pending: true,
        ..bound(Unavailable(U::AccessRevoked))
    });
    assert_eq!(admit_on(reverify_revoked, Read), refused(R::NotReady, true));
    let both = Switches {
        off: true,
        profile_disabled: true,
    };
    assert_eq!(
        admit(both, ready, false, Search),
        refused(R::KnowledgeOff, false)
    );

    // Rows 7 and 8: the binding's own reason.
    for (cause, reason, retryable) in [
        (U::Dependency, R::Dependency, true),
        (U::Incompatible, R::Incompatible, false),
        (U::CredentialRejected, R::CredentialRejected, false),
        (U::AccessRevoked, R::AccessRevoked, false),
        (U::InstanceUnidentified, R::InstanceUnidentified, false),
        (U::LocalStorage, R::LocalStorage, false),
    ] {
        assert_eq!(
            admit_on(Target::Binding(bound(Unavailable(cause))), Search),
            refused(reason, retryable),
            "{cause:?}"
        );
    }
}

#[test]
fn local_reasons_carry_their_wire_names() {
    for (reason, name) in [
        (R::KnowledgeOff, "knowledge_off"),
        (R::LocalStorage, "local_storage"),
        (R::ProfileInactive, "profile_inactive"),
        (R::Incompatible, "incompatible"),
        (R::CredentialRejected, "credential_rejected"),
        (R::AccessRevoked, "access_revoked"),
        (R::InstanceUnidentified, "instance_unidentified"),
        (R::NotReady, "not_ready"),
        (R::Dependency, "dependency"),
    ] {
        assert_eq!(reason.as_str(), name);
    }
}
