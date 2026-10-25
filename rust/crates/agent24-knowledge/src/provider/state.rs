//! A binding's service state and the local result of a new call
//! (CONTRACT-STATE-v1 §1, §6), as pure judgements over facts the Provider
//! keeps elsewhere. Nothing here changes state, persists or sends: the event
//! handling, generation and persistence slices feed these facts in.

use crate::ready::{OperationStatus, ReadyResponse};

/// The two operations that carry Knowledge content. `ingest` takes no part in
/// the connection phase (S1.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DataOperation {
    Search,
    Read,
}

/// Why a binding is `unavailable` (S1.4). It always has one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnavailableCause {
    Dependency,
    NotReady,
    Incompatible,
    InstanceUnidentified,
    CredentialRejected,
    AccessRevoked,
    LocalStorage,
}

impl UnavailableCause {
    /// `dependency` and `not_ready` are retried with backoff. The others wait
    /// for a manual retry, a configuration change, a sidecar restart, a new
    /// handshake or a new login; `local_storage` goes back to `loading` once
    /// `persist_pending` clears.
    pub fn retries_automatically(self) -> bool {
        matches!(self, Self::Dependency | Self::NotReady)
    }
}

/// `loading`, `ready`, `degraded` or `unavailable` (the connection phase).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionPhase {
    Loading,
    Ready,
    /// Exactly one operation is `ready`, and only that one is admitted.
    Degraded {
        servable: DataOperation,
    },
    Unavailable(UnavailableCause),
}

impl ConnectionPhase {
    /// `ready` or `degraded`.
    pub fn is_servable(self) -> bool {
        matches!(self, Self::Ready | Self::Degraded { .. })
    }

    fn serves(self, op: DataOperation) -> bool {
        match self {
            Self::Ready => true,
            Self::Degraded { servable } => servable == op,
            Self::Loading | Self::Unavailable(_) => false,
        }
    }
}

/// What capability discovery and an unscoped `knowledge.ready` gave (S1.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Probe {
    /// Unreachable, or no answer in time.
    NoAnswer,
    /// Capability discovery judged the server incompatible (R9.4), so no
    /// readiness probe was sent.
    Incompatible,
    /// The readiness answer. `search_ready` and `read_ready` say whether that
    /// operation's `state` is `ready`; any other value, known or not, is not
    /// (R6.2). `ingest` and the per-KB readiness take no part.
    Answered {
        live: bool,
        search_ready: bool,
        read_ready: bool,
    },
}

impl Probe {
    /// The readiness answer of a compatible server.
    pub fn answered(ready: &ReadyResponse) -> Self {
        Self::Answered {
            live: ready.live,
            search_ready: ready.operations.search.state == OperationStatus::Ready,
            read_ready: ready.operations.read.state == OperationStatus::Ready,
        }
    }
}

/// The connection phase a probe leads to (S1.3).
pub fn judge_connection(probe: Probe) -> ConnectionPhase {
    use UnavailableCause as U;
    let (search, read) = match probe {
        Probe::NoAnswer => return ConnectionPhase::Unavailable(U::Dependency),
        Probe::Incompatible => return ConnectionPhase::Unavailable(U::Incompatible),
        Probe::Answered { live: false, .. } => return ConnectionPhase::Unavailable(U::Dependency),
        Probe::Answered {
            search_ready,
            read_ready,
            ..
        } => (search_ready, read_ready),
    };
    match (search, read) {
        (true, true) => ConnectionPhase::Ready,
        (true, false) => ConnectionPhase::Degraded {
            servable: DataOperation::Search,
        },
        (false, true) => ConnectionPhase::Degraded {
            servable: DataOperation::Read,
        },
        (false, false) => ConnectionPhase::Unavailable(U::NotReady),
    }
}

/// The six service states (S1.1); `retired` and `persist_pending` are not
/// among them and travel next to it in [`Binding`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceState {
    Registered,
    Disabled,
    Connected(ConnectionPhase),
}

/// The two host-held switches. They are independent and may both be on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Switches {
    /// Knowledge Off, for every binding.
    pub off: bool,
    /// This profile is disabled.
    pub profile_disabled: bool,
}

/// Why a login or handshake on the unbound placeholder failed (S2.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaceholderFailure {
    Dependency,
    CredentialRejected,
    InstanceUnidentified,
}

/// What a profile points at: the unbound placeholder, or a binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// No binding that is not `retired` (S2.5).
    Placeholder {
        failure: Option<PlaceholderFailure>,
        /// The profile carries `unsaved_settings` (S1.13).
        unsaved_settings: bool,
    },
    Binding(Binding),
}

/// The facts about one binding that its state and admission depend on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
    pub retired: bool,
    /// Selected, principal bound, and the profile awaits no confirmation; Off
    /// and the disabled flag are [`Switches`], not part of this.
    pub activated: bool,
    pub phase: ConnectionPhase,
    pub persist_pending: bool,
    /// A re-verification is registered and not yet cleared (S3.1).
    pub reverify_pending: bool,
}

/// S1.2: the first that holds of disabled, registered, the connection phase.
/// `None` for a `retired` binding, which has no service state.
pub fn derive(switches: Switches, target: Target) -> Option<ServiceState> {
    let activated = match target {
        Target::Binding(binding) if binding.retired => return None,
        Target::Binding(binding) => binding.activated.then_some(binding.phase),
        Target::Placeholder { .. } => None,
    };
    Some(if switches.off || switches.profile_disabled {
        ServiceState::Disabled
    } else {
        match activated {
            None => ServiceState::Registered,
            Some(phase) => ServiceState::Connected(phase),
        }
    })
}

/// The `reason` of a Provider-local `unavailable` (§6, S6.1): the local set
/// plus the two READONLY reasons the table reuses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LocalReason {
    KnowledgeOff,
    LocalStorage,
    ProfileInactive,
    Incompatible,
    CredentialRejected,
    AccessRevoked,
    InstanceUnidentified,
    NotReady,
    Dependency,
}

impl LocalReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::KnowledgeOff => "knowledge_off",
            Self::LocalStorage => "local_storage",
            Self::ProfileInactive => "profile_inactive",
            Self::Incompatible => "incompatible",
            Self::CredentialRejected => "credential_rejected",
            Self::AccessRevoked => "access_revoked",
            Self::InstanceUnidentified => "instance_unidentified",
            Self::NotReady => "not_ready",
            Self::Dependency => "dependency",
        }
    }
}

/// A new call refused locally, always `code: unavailable` (§6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub reason: LocalReason,
    pub retryable: bool,
}

const fn refuse(reason: LocalReason, retryable: bool) -> Result<(), Refusal> {
    Err(Refusal { reason, retryable })
}

/// §6 for a new call on one binding: the first row that holds, top down.
/// `run_binding_unconfirmed` is the calling run's binding (S4.7) not yet
/// confirmed persisted. Calls already sent or accepted never come here.
pub fn admit(
    switches: Switches,
    target: Target,
    run_binding_unconfirmed: bool,
    op: DataOperation,
) -> Result<(), Refusal> {
    use LocalReason as R;
    if switches.off {
        return refuse(R::KnowledgeOff, false);
    }
    let binding = match target {
        Target::Binding(binding) => Some(binding),
        Target::Placeholder { .. } => None,
    };
    if run_binding_unconfirmed || binding.is_some_and(|b| b.persist_pending) {
        return refuse(R::LocalStorage, false);
    }
    if switches.profile_disabled || binding.is_some_and(|b| b.retired) {
        return refuse(R::ProfileInactive, false);
    }
    let binding = match target {
        Target::Placeholder {
            failure: Some(failure),
            unsaved_settings,
        } => {
            return match failure {
                PlaceholderFailure::Dependency => refuse(R::Dependency, !unsaved_settings),
                PlaceholderFailure::CredentialRejected => refuse(R::CredentialRejected, false),
                PlaceholderFailure::InstanceUnidentified => refuse(R::InstanceUnidentified, false),
            };
        }
        Target::Placeholder { failure: None, .. } => return refuse(R::ProfileInactive, false),
        Target::Binding(binding) if !binding.activated => {
            return refuse(R::ProfileInactive, false);
        }
        Target::Binding(binding) => binding,
    };
    if binding.reverify_pending {
        return refuse(R::NotReady, true);
    }
    match binding.phase {
        phase if phase.serves(op) => Ok(()),
        ConnectionPhase::Loading | ConnectionPhase::Ready | ConnectionPhase::Degraded { .. } => {
            refuse(R::NotReady, true)
        }
        ConnectionPhase::Unavailable(cause) => match cause {
            UnavailableCause::NotReady => refuse(R::NotReady, true),
            UnavailableCause::Dependency => refuse(R::Dependency, true),
            UnavailableCause::Incompatible => refuse(R::Incompatible, false),
            UnavailableCause::CredentialRejected => refuse(R::CredentialRejected, false),
            UnavailableCause::AccessRevoked => refuse(R::AccessRevoked, false),
            UnavailableCause::InstanceUnidentified => refuse(R::InstanceUnidentified, false),
            // No row of its own: a loosening not yet persisted, as in row 2.
            UnavailableCause::LocalStorage => refuse(R::LocalStorage, false),
        },
    }
}

#[cfg(test)]
mod tests;
