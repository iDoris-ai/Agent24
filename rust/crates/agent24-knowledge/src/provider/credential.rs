//! The credential part of one binding (CONTRACT-STATE-v1 S3.1): the current
//! credential, the issuances owned by `loading` rounds, and what a revocation
//! does to the [`Connection`]. Expiry and the invalid class come next; pending
//! re-verification and S5.10's narrowing with the evidence slice.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use super::connection::{Connection, Round};
use super::gate::Invalidation;
use super::state::{ConnectionPhase, UnavailableCause};

/// S3.1's window: the third event within ten minutes.
const WINDOW: Duration = Duration::from_secs(10 * 60);
const WINDOW_LIMIT: usize = 3;

/// A credential the binding adopted. Each request records the one it was
/// sent with (S3.1, credential ownership).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CredentialId(u64);

/// An issuance the caller is about to send. Its answer counts only
/// within the round it was sent in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Issuance {
    round: Round,
    seq: u64,
}

/// What the issuer answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Issued {
    /// A credential for the binding's principal. `narrowed`: its scope lacks a
    /// KB or an operation the binding's last granted scope had, even with
    /// others added; the caller keeps that scope, narrowed grants included.
    Granted { narrowed: bool },
    /// A credential for another principal: S3.5, in the identity slice.
    OtherPrincipal,
    /// Issuance refused, re-issuing included.
    Refused,
    /// The issuing authority says the credential is revoked.
    Revoked,
    /// The issuer could not be reached.
    Unreachable,
}

/// The credential class of an accepted READONLY response (R6.2); the other
/// classes come in the next slice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    /// `unauthorized/credential_revoked`.
    Revoked,
}

/// What an event asks of the caller.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Effects {
    /// The event invalidated: abort these calls and persist this value (S4.5).
    pub invalidation: Option<Invalidation>,
    /// Advance the evidence epoch and record a placeholder rule covering every
    /// KB (S5.6).
    pub placeholder: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Done(Effects),
    /// From an ended round or not the issuance in flight: nothing adopted,
    /// nothing moves.
    Ignored,
    /// Granted for another principal: nothing adopted; S3.5 for the caller.
    OtherPrincipal,
    /// A revocation signal on a discarded credential: the result becomes a
    /// local `not_ready`, and pending re-verification is due (evidence slice).
    Recheck,
}

#[derive(Clone, Copy, Debug)]
struct Current {
    id: CredentialId,
    /// The round it took effect in.
    round: Round,
}

#[derive(Debug, Default)]
struct Window(VecDeque<Instant>);

impl Window {
    /// Records an event at `now`; whether it is at least the third within
    /// the window.
    fn record(&mut self, now: Instant) -> bool {
        self.0.retain(|t| now.duration_since(*t) < WINDOW);
        self.0.push_back(now);
        self.0.len() >= WINDOW_LIMIT
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Revocation {
    Signal,
    Narrowing,
}

#[derive(Debug)]
pub struct Credentials {
    current: Option<Current>,
    issuing: Option<Issuance>,
    next_issuance: u64,
    next_credential: u64,
    /// A revocation-class event happened since the binding was created; it
    /// ends S3.1's exception for good.
    revoked_before: bool,
    revocations: Window,
}

impl Credentials {
    /// `revoked_before` as persisted; when it cannot be told, the caller
    /// passes `true`, so the exception never applies by mistake.
    pub fn new(revoked_before: bool) -> Self {
        Self {
            current: None,
            issuing: None,
            next_issuance: 0,
            next_credential: 0,
            revoked_before,
            revocations: Window::default(),
        }
    }

    pub fn current(&self) -> Option<CredentialId> {
        self.current.map(|c| c.id)
    }

    /// The issuance of a `loading` round without a credential. One at a time.
    pub fn issue(&mut self, connection: &Connection) -> Option<Issuance> {
        let due = connection.phase() == Some(ConnectionPhase::Loading) && self.current.is_none();
        if !due || self.issuing_in(connection) {
            return None;
        }
        Some(self.ticket(connection))
    }

    /// Applies the issuer's answer to `ticket`.
    pub fn issued(
        &mut self,
        connection: &mut Connection,
        ticket: Issuance,
        answer: Issued,
        now: Instant,
    ) -> Outcome {
        if self.issuing != Some(ticket) || !connection.is_current(ticket.round) {
            return Outcome::Ignored;
        }
        self.issuing = None;
        match answer {
            Issued::Granted { narrowed: false } => {
                self.adopt(connection);
                Outcome::Done(Effects::default())
            }
            Issued::Granted { narrowed: true } => {
                self.revocation(connection, Revocation::Narrowing, now)
            }
            Issued::OtherPrincipal => Outcome::OtherPrincipal,
            Issued::Refused if self.excepted(connection) => self.rejected(connection),
            Issued::Refused | Issued::Revoked => {
                self.revocation(connection, Revocation::Signal, now)
            }
            Issued::Unreachable => {
                connection.fail(UnavailableCause::Dependency);
                Outcome::Done(Effects::default())
            }
        }
    }

    /// Applies the credential class of an accepted response sent with
    /// `credential`.
    pub fn response(
        &mut self,
        connection: &mut Connection,
        credential: CredentialId,
        signal: Signal,
        now: Instant,
    ) -> Outcome {
        let Some(current) = self.current.filter(|c| c.id == credential) else {
            return Outcome::Recheck;
        };
        match signal {
            Signal::Revoked
                if self.excepted(connection) && connection.is_current(current.round) =>
            {
                self.rejected(connection)
            }
            Signal::Revoked => self.revocation(connection, Revocation::Signal, now),
        }
    }

    /// A revocation signal from outside a response: the issuing authority's
    /// notice, or a local-managed rotation of the instance secret.
    pub fn revoke(&mut self, connection: &mut Connection, now: Instant) -> Outcome {
        self.revocation(connection, Revocation::Signal, now)
    }

    /// The server narrowed the scope while the credential stayed (S5.10).
    pub fn narrow(&mut self, connection: &mut Connection, now: Instant) -> Outcome {
        self.revocation(connection, Revocation::Narrowing, now)
    }

    fn issuing_in(&self, connection: &Connection) -> bool {
        self.issuing.is_some_and(|i| connection.is_current(i.round))
    }

    fn ticket(&mut self, connection: &Connection) -> Issuance {
        let ticket = Issuance {
            round: connection.round(),
            seq: self.next_issuance,
        };
        self.next_issuance += 1;
        self.issuing = Some(ticket);
        ticket
    }

    fn adopt(&mut self, connection: &Connection) {
        self.current = Some(Current {
            id: CredentialId(self.next_credential),
            round: connection.round(),
        });
        self.next_credential += 1;
    }

    /// S3.1's exception: never servable and never revoked, still `loading`.
    fn excepted(&self, connection: &Connection) -> bool {
        !self.revoked_before
            && !connection.ever_servable()
            && connection.phase() == Some(ConnectionPhase::Loading)
    }

    /// The exception's outcome: no increment, no placeholder.
    fn rejected(&mut self, connection: &mut Connection) -> Outcome {
        self.current = None;
        connection.fail(UnavailableCause::CredentialRejected);
        Outcome::Done(Effects::default())
    }

    /// A revocation-class event: a placeholder in any state; in the
    /// connection phase also an invalidation and the move S3.1 gives it.
    fn revocation(
        &mut self,
        connection: &mut Connection,
        kind: Revocation,
        now: Instant,
    ) -> Outcome {
        self.revoked_before = true;
        self.current = None;
        let third = self.revocations.record(now);
        let mut effects = Effects {
            placeholder: true,
            ..Effects::default()
        };
        let Some(phase) = connection.phase() else {
            return Outcome::Done(effects);
        };
        effects.invalidation = Some(connection.invalidate());
        match (kind, phase) {
            _ if third => connection.fail(UnavailableCause::AccessRevoked),
            (Revocation::Narrowing, ConnectionPhase::Unavailable(cause))
                if !cause.retries_automatically() => {}
            (Revocation::Signal, ConnectionPhase::Loading | ConnectionPhase::Unavailable(_)) => {
                connection.fail(UnavailableCause::AccessRevoked)
            }
            _ => {
                connection.start_loading();
            }
        }
        Outcome::Done(effects)
    }
}

#[cfg(test)]
mod tests;
