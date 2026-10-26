//! One binding as a whole (CONTRACT-STATE-v1 S3.3): its connection,
//! credential and evidence parts, whether it is activated, whether it is
//! retired, and the user and configuration events that take it out of the
//! connection phase or back in. Credential events go through it, so each one
//! that calls for a placeholder places its rule (S5.6). Pure state, like its
//! parts. Persisting what an event asks for,
//! and refusing a loosening while a tightening is not yet persisted (I1), are
//! the persistence slice's; identity changes and the placeholder are in
//! `profile`.

use std::collections::BTreeSet;
use std::time::Instant;

use super::connection::{Connection, ProbeTicket, Round};
use super::credential::{CredentialId, Credentials, Effects, Issuance, Issued, Outcome, Signal};
use super::evidence::{Evidence, Kbs, Observed, Rule, Scope};
use super::state::{self, ConnectionPhase, Switches, UnavailableCause};

/// An event that takes the binding out of the connection phase (S3.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Leave {
    /// Knowledge Off, for every binding.
    Off,
    /// The binding's profile disabled.
    Disable,
    /// The profile no longer selected.
    Unselect,
    /// Logged out (remote only).
    Logout,
    /// The profile removed: disabled first, then `retired`.
    Remove,
    /// The identity changed (S3.5): `retired`.
    Retire,
}

/// How the same binding is activated again (S3.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activation {
    /// The profile selected again.
    Reselect,
    /// The same principal logged in again.
    Relogin,
}

/// The sidecar's state: an input to a local-managed binding, not a service
/// state (S2.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sidecar {
    Starting,
    Ready,
    Healthy,
    Degraded,
    Failed,
    Stopping,
    Stopped,
}

/// What a backoff tick in `dependency` or `not_ready` sends (S1.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retry {
    /// No credential to probe with: a new `loading` round issues one first
    /// (#1228 note 1).
    Issue(Issuance),
    Probe(ProbeTicket),
}

/// What an event did, with the placeholder rule it placed (S5.6): the
/// caller persists the rule as part of the event's tightening.
#[derive(Debug, PartialEq, Eq)]
#[must_use = "a rule placed is persisted with the event"]
pub struct WithRule<T, K> {
    pub value: T,
    pub rule: Option<Rule<K>>,
}

#[derive(Debug)]
pub struct BindingState<K> {
    connection: Connection,
    credentials: Credentials,
    evidence: Evidence<K>,
    /// Selected, principal bound, no confirmation pending; Off and the
    /// disabled flag are [`Switches`], not part of this.
    activated: bool,
    retired: bool,
}

impl<K: Ord + Clone> BindingState<K> {
    pub fn new(connection: Connection, credentials: Credentials, activated: bool) -> Self {
        Self {
            connection,
            credentials,
            evidence: Evidence::default(),
            activated,
            retired: false,
        }
    }

    /// With the evidence state restored at restart (S1.14).
    pub fn with_evidence(self, evidence: Evidence<K>) -> Self {
        Self { evidence, ..self }
    }

    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    pub fn credentials(&self) -> &Credentials {
        &self.credentials
    }

    /// For probes; credential events go through the binding.
    pub fn connection_mut(&mut self) -> &mut Connection {
        &mut self.connection
    }

    pub fn evidence(&self) -> &Evidence<K> {
        &self.evidence
    }

    /// For evidence delivered, rechecks and deletions. A response's scope
    /// goes through [`BindingState::observe`], a registration through
    /// [`BindingState::response`]: both come with a credential event.
    pub fn evidence_mut(&mut self) -> &mut Evidence<K> {
        &mut self.evidence
    }

    pub fn retired(&self) -> bool {
        self.retired
    }

    /// The facts [`state::derive`] and [`state::admit`] take. Out of the
    /// connection phase it reports not activated, so it derives as
    /// `registered` or `disabled` until [`BindingState::connect`] runs.
    pub fn facts(&self) -> state::Binding {
        let phase = self.connection.phase();
        state::Binding {
            retired: self.retired,
            activated: self.activated && phase.is_some(),
            phase: phase.unwrap_or(ConnectionPhase::Loading),
            persist_pending: self.connection.persist_pending(),
            reverify_pending: self.evidence.reverify_pending(),
        }
    }

    /// S3.3: in the connection phase it invalidates; in any state the short
    /// lived credential goes, and every event but Off advances the evidence
    /// epoch (S5.6, decision 3). A retired binding takes no event.
    pub fn leave(&mut self, event: Leave) -> WithRule<Effects, K> {
        if self.retired {
            return self.ruled(Effects::default());
        }
        let invalidation = self
            .connection
            .phase()
            .map(|_| self.connection.invalidate());
        self.connection.leave();
        self.credentials.discard();
        match event {
            Leave::Unselect | Leave::Logout => self.activated = false,
            Leave::Remove | Leave::Retire => self.retired = true,
            Leave::Off | Leave::Disable => {}
        }
        self.ruled(Effects {
            invalidation,
            placeholder: event != Leave::Off,
            ..Effects::default()
        })
    }

    /// A `loading` round's issuance, as [`Credentials::issue`].
    pub fn issue(&mut self) -> Option<Issuance> {
        self.credentials.issue(&self.connection)
    }

    /// The issuer's answer, as [`Credentials::issued`].
    pub fn issued(
        &mut self,
        ticket: Issuance,
        answer: Issued,
        now: Instant,
    ) -> WithRule<Outcome, K> {
        let outcome = self
            .credentials
            .issued(&mut self.connection, ticket, answer, now);
        self.ruled_outcome(outcome)
    }

    /// A revocation signal from outside a response, as [`Credentials::revoke`].
    pub fn revoke(&mut self, now: Instant) -> WithRule<Outcome, K> {
        let outcome = self.credentials.revoke(&mut self.connection, now);
        self.ruled_outcome(outcome)
    }

    /// The credential class of an accepted response, as
    /// [`Credentials::response`]. `asked` is the request's KBs, or for one
    /// without KBs (an issuance) its credential's granted scope, never empty
    /// (S3.1): a revocation signal on a discarded credential registers them
    /// for re-verification, admitting nothing until it is released (§6).
    pub fn response(
        &mut self,
        credential: CredentialId,
        signal: Signal,
        asked: impl IntoIterator<Item = K>,
        now: Instant,
    ) -> WithRule<Outcome, K> {
        let outcome = self
            .credentials
            .response(&mut self.connection, credential, signal, now);
        if outcome == Outcome::Recheck {
            let registered = self.evidence.register(asked);
            debug_assert!(registered, "S3.1: Q is never empty");
        }
        self.ruled_outcome(outcome)
    }

    /// The recheck, sent with `credential`, came back with a revocation
    /// signal: a revocation as any (S3.1), and what was held back becomes
    /// placeholders. If the credential was discarded meanwhile, it asks for
    /// another recheck instead.
    pub fn recheck_revoked(
        &mut self,
        credential: CredentialId,
        now: Instant,
    ) -> WithRule<Outcome, K> {
        let outcome =
            self.credentials
                .response(&mut self.connection, credential, Signal::Revoked, now);
        let rule = (outcome != Outcome::Recheck).then(|| self.evidence.revoked());
        WithRule {
            value: outcome,
            rule,
        }
    }

    /// An accepted response's scope (S5.10), and if it removed a KB the
    /// narrowing that is, in one step: no other response may come between
    /// (#1297 G-B). The caller has already dropped responses invalidated
    /// meanwhile (S4.6).
    pub fn observe(
        &mut self,
        asked: &BTreeSet<K>,
        scope: &Scope<K>,
        admitted: u64,
        credential: CredentialId,
        now: Instant,
    ) -> WithRule<(Observed<K>, Option<Outcome>), K> {
        let current = self.credentials.current() == Some(credential);
        let observed = self.evidence.observe(asked, scope, admitted, current);
        if observed.removed.is_empty() {
            return WithRule {
                value: (observed, None),
                rule: None,
            };
        }
        let narrowed = self.credentials.narrow(&mut self.connection, now);
        let WithRule { value, rule } = self.ruled_outcome(narrowed);
        WithRule {
            value: (observed, Some(value)),
            rule,
        }
    }

    fn ruled(&mut self, effects: Effects) -> WithRule<Effects, K> {
        let rule = effects.placeholder.then(|| self.evidence.place(Kbs::All));
        WithRule {
            value: effects,
            rule,
        }
    }

    fn ruled_outcome(&mut self, outcome: Outcome) -> WithRule<Outcome, K> {
        let placeholder = matches!(&outcome, Outcome::Done(e) if e.placeholder);
        let rule = placeholder.then(|| self.evidence.place(Kbs::All));
        WithRule {
            value: outcome,
            rule,
        }
    }

    /// The same binding again, no increment (S3.3, S4.3); the caller has
    /// persisted the loosening first (I1). Logging in again also retries
    /// `credential_rejected` and `access_revoked` (S1.4), logged out first or
    /// not; [`BindingState::connect`] then enters `loading`. A retired binding
    /// still never connects.
    pub fn activate(&mut self, how: Activation) {
        self.activated = true;
        if how == Activation::Relogin {
            self.relogin();
        }
    }

    /// The same principal logged in again, without activating: a login only
    /// remembered while unselected or awaiting confirmation (S3.3) still
    /// lets `credential_rejected` and `access_revoked` be retried later.
    pub fn relogin(&mut self) {
        use UnavailableCause::{AccessRevoked, CredentialRejected};
        // Still in that reason: out first, so `connect` enters `loading`.
        let phase = self.connection.phase();
        if let Some(ConnectionPhase::Unavailable(AccessRevoked | CredentialRejected)) = phase {
            self.connection.leave();
        }
        self.connection.release(CredentialRejected);
        self.connection.release(AccessRevoked);
    }

    /// Enters `loading` in a new round once activated with neither switch on
    /// (S1.2); `local_storage` instead while a change is pending (S1.4).
    /// `None` when it does not enter `loading` now.
    pub fn connect(&mut self, switches: Switches) -> Option<Round> {
        let blocked = switches.off || switches.profile_disabled;
        if self.retired || !self.activated || blocked || self.connection.phase().is_some() {
            return None;
        }
        self.connection.start_loading()
    }

    /// A probe goes out only with a current credential, so no probe moves
    /// the binding to a servable phase without one (#1228 note 1).
    pub fn probe(&mut self) -> Option<ProbeTicket> {
        self.credentials.current()?;
        self.connection.probe()
    }

    /// The backoff of `dependency` and `not_ready`: a probe with the current
    /// credential, or without one a new `loading` round and its issuance.
    pub fn backoff(&mut self) -> Option<Retry> {
        match self.connection.phase()? {
            ConnectionPhase::Unavailable(cause) if cause.retries_automatically() => {}
            _ => return None,
        }
        if self.credentials.current().is_some() {
            return self.probe().map(Retry::Probe);
        }
        self.connection.start_loading()?;
        self.credentials.issue(&self.connection).map(Retry::Issue)
    }

    /// A manual retry, a configuration change, a sidecar restart or a new
    /// handshake of the same instance (S1.4): every kept reason is let go,
    /// and an `unavailable` binding enters `loading` in a new round. Leaving
    /// a reason that does not retry by itself is a loosening the caller has
    /// persisted first (S1.10, I1). A retired binding is out of the
    /// connection phase for good, so it never enters `loading` here.
    pub fn retry(&mut self) -> Option<Round> {
        self.connection.release_all();
        match self.connection.phase()? {
            ConnectionPhase::Unavailable(cause) if cause != UnavailableCause::LocalStorage => {
                self.connection.start_loading()
            }
            _ => None,
        }
    }

    /// S2.2: `starting` retries as [`BindingState::retry`]; a sidecar going
    /// down makes the binding `unavailable(dependency)` in the connection
    /// phase, no increment. A reason that does not retry by itself, or
    /// `local_storage`, stays: `dependency` would retry it by backoff.
    pub fn sidecar(&mut self, sidecar: Sidecar) -> Option<Round> {
        let down = match sidecar {
            Sidecar::Starting => return self.retry(),
            Sidecar::Ready | Sidecar::Healthy => false,
            Sidecar::Degraded | Sidecar::Failed | Sidecar::Stopping | Sidecar::Stopped => true,
        };
        let movable = match self.connection.phase() {
            Some(ConnectionPhase::Unavailable(cause)) => cause.retries_automatically(),
            Some(_) => true,
            None => false,
        };
        if down && movable {
            self.connection.fail(UnavailableCause::Dependency);
        }
        None
    }
}

#[cfg(test)]
mod tests;
