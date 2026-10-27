//! The credential part of one binding (CONTRACT-STATE-v1 S3.1): the current
//! credential, the issuances owned by `loading` rounds, and what a revocation
//! does to the [`Connection`], with expiry and the invalid class. Pending
//! re-verification and S5.10's narrowing come with the evidence slice.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use super::connection::{Connection, Round};
use super::gate::Invalidation;
use super::state::{ConnectionPhase, UnavailableCause};

/// S3.1's windows: the third event of a kind within ten minutes.
const WINDOW: Duration = Duration::from_secs(10 * 60);
const WINDOW_LIMIT: usize = 3;

/// A credential the binding adopted. Each request records the one it was
/// sent with (S3.1, credential ownership).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CredentialId(u64);

/// An issuance or renewal the caller is about to send. Its answer counts only
/// within the round it was sent in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Issuance {
    round: Round,
    seq: u64,
    /// The renewal after an invalid-class response: refused, it fails too.
    after_invalid: bool,
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
    /// Issuance or renewal refused.
    Refused,
    /// The issuing authority says the credential is revoked.
    Revoked,
    /// The issuer could not be reached.
    Unreachable,
}

/// The credential class of an accepted READONLY response (R6.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    /// `unauthorized/credential_revoked`.
    Revoked,
    /// `credential_expired`.
    Expired,
    /// `unauthorized` with `credential_invalid`, `credential_missing` or a
    /// reason not known.
    Invalid,
}

/// What an event asks of the caller.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Effects {
    /// The event invalidated: abort these calls and persist this value (S4.5).
    pub invalidation: Option<Invalidation>,
    /// Advance the evidence epoch, add a placeholder rule for every KB (S5.6).
    pub placeholder: bool,
    /// A renewal to send now.
    pub renew: Option<Issuance>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Done(Effects),
    /// From an ended round, or not the issuance in flight: nothing moves.
    Ignored,
    /// Granted for another principal: nothing adopted; S3.5 for the caller.
    OtherPrincipal,
    /// Sent with a discarded credential: no transition, no placeholder, no
    /// narrowing judged; the result becomes a local `not_ready` (§6).
    Stale,
    /// A revocation signal on a discarded credential: the result becomes a
    /// local `not_ready`, and pending re-verification is due (evidence slice).
    Recheck,
}

#[derive(Debug, Default)]
struct Window(VecDeque<Instant>);

impl Window {
    /// Records an event at `now`: whether it is the third within the window.
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
    /// With the round it took effect in.
    current: Option<(CredentialId, Round)>,
    issuing: Option<Issuance>,
    next_issuance: u64,
    next_credential: u64,
    /// A revocation-class event since creation ends S3.1's exception for good.
    revoked_before: bool,
    /// The round renewing after an invalid-class response, the only one in
    /// which "the renewal fails too" is judged. A round lasts until the next
    /// `loading` or failure, through `ready` too (S3.4); rounds never come back.
    renewal: Option<Round>,
    revocations: Window,
    invalids: Window,
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
            renewal: None,
            revocations: Window::default(),
            invalids: Window::default(),
        }
    }

    /// Whether a revocation-class event ended S3.1's exception for good.
    pub fn revoked_before(&self) -> bool {
        self.revoked_before
    }

    pub fn current(&self) -> Option<CredentialId> {
        self.current.map(|(id, _)| id)
    }

    /// A `loading` round's issuance: no credential, nothing pending. One at a time.
    pub fn issue(&mut self, connection: &Connection) -> Option<Issuance> {
        let due = connection.phase() == Some(ConnectionPhase::Loading) && self.current.is_none();
        if !due || connection.persist_pending() || self.issuing_in(connection) {
            return None;
        }
        let after_invalid = self.renewing(connection);
        Some(self.ticket(connection, after_invalid))
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
            Issued::Refused if ticket.after_invalid => self.renewal_failed(connection),
            Issued::Refused if self.excepted(connection) => self.rejected(connection),
            Issued::Refused | Issued::Revoked => {
                self.revocation(connection, Revocation::Signal, now)
            }
            // A probe may have moved it within the round (S1.4, S4.6).
            Issued::Unreachable => {
                self.current = None;
                if active(connection) {
                    connection.fail(UnavailableCause::Dependency);
                }
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
        let Some((_, round)) = self.current.filter(|(id, _)| *id == credential) else {
            return match signal {
                Signal::Revoked => Outcome::Recheck,
                Signal::Expired | Signal::Invalid => Outcome::Stale,
            };
        };
        match signal {
            Signal::Revoked if self.excepted(connection) && connection.is_current(round) => {
                self.rejected(connection)
            }
            Signal::Revoked => self.revocation(connection, Revocation::Signal, now),
            Signal::Expired => {
                if !active(connection) || self.issuing_in(connection) {
                    return Outcome::Done(Effects::default());
                }
                Outcome::Done(Effects {
                    renew: Some(self.ticket(connection, false)),
                    ..Effects::default()
                })
            }
            Signal::Invalid => self.invalid(connection, now),
        }
    }

    /// The binding left the connection phase: the credential goes (S3.3).
    pub fn discard(&mut self) {
        self.current = None;
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

    fn ticket(&mut self, connection: &Connection, after_invalid: bool) -> Issuance {
        let ticket = Issuance {
            round: connection.round(),
            seq: self.next_issuance,
            after_invalid,
        };
        self.next_issuance += 1;
        self.issuing = Some(ticket);
        ticket
    }

    /// A renewal granted in an `unavailable` that retries by itself goes back
    /// to `loading`; another reason stays.
    fn adopt(&mut self, connection: &mut Connection) {
        let unavailable = matches!(connection.phase(), Some(ConnectionPhase::Unavailable(_)));
        if unavailable && active(connection) {
            connection.start_loading();
        }
        self.current = Some((CredentialId(self.next_credential), connection.round()));
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

    /// Only in the connection phase, and in `unavailable` only for a reason
    /// that retries by itself; elsewhere nothing changes. The first one is no
    /// revocation: the credential goes and a new round renews once.
    fn invalid(&mut self, connection: &mut Connection, now: Instant) -> Outcome {
        if !active(connection) {
            return Outcome::Done(Effects::default());
        }
        let third = self.invalids.record(now);
        if third || self.renewing(connection) {
            return self.renewal_failed(connection);
        }
        self.current = None;
        self.renewal = connection.start_loading();
        Outcome::Done(Effects::default())
    }

    /// Still in the round renewing after an invalid-class response.
    fn renewing(&self, connection: &Connection) -> bool {
        self.renewal.is_some_and(|r| connection.is_current(r))
    }

    /// The renewal fails too: no increment, a placeholder for every KB; a
    /// reason a probe gave meanwhile that does not retry by itself stays.
    fn renewal_failed(&mut self, connection: &mut Connection) -> Outcome {
        self.current = None;
        if active(connection) {
            connection.fail(UnavailableCause::CredentialRejected);
        }
        Outcome::Done(Effects {
            placeholder: true,
            ..Effects::default()
        })
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
        // An issuance sent before does not bring a credential back.
        self.issuing = None;
        let third = self.revocations.record(now);
        let Some(phase) = connection.phase() else {
            return Outcome::Done(Effects {
                placeholder: true,
                ..Effects::default()
            });
        };
        let invalidation = Some(connection.invalidate());
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
        Outcome::Done(Effects {
            invalidation,
            placeholder: true,
            ..Effects::default()
        })
    }
}

/// The connection phase, and in `unavailable` only a reason that retries by
/// itself.
fn active(connection: &Connection) -> bool {
    match connection.phase() {
        Some(ConnectionPhase::Unavailable(cause)) => cause.retries_automatically(),
        Some(_) => true,
        None => false,
    }
}

#[cfg(test)]
mod tests;
