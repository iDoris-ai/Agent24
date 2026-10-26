//! The records changes are persisted as (CONTRACT-STATE-v1 S1.10; appendix
//! A.1, A.4, A.5 as reference): the class of each, the tightening that offsets
//! an abandoned loosening, and whether a record landing late on a sequence
//! number given up still counts. Pure data: the host numbers and writes them
//! (KB-C1-04 slice 6); `replay` reads them back. Evidence epochs, placeholder
//! rules, tombstones, revalidation and the visible set join with slice 5.

use std::fmt::Debug;
use std::hash::Hash;

use super::ledger::Scope;
use super::profile::Confirmation;
use super::state::{PlaceholderFailure, UnavailableCause};

/// A reason that does not retry by itself and holds across restarts (S1.4).
/// `dependency` and `not_ready` back off instead; `local_storage` follows
/// `persist_pending`, which a restart derives anew.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StuckCause {
    Incompatible,
    InstanceUnidentified,
    CredentialRejected,
    AccessRevoked,
}

impl From<StuckCause> for UnavailableCause {
    fn from(cause: StuckCause) -> Self {
        match cause {
            StuckCause::Incompatible => Self::Incompatible,
            StuckCause::InstanceUnidentified => Self::InstanceUnidentified,
            StuckCause::CredentialRejected => Self::CredentialRejected,
            StuckCause::AccessRevoked => Self::AccessRevoked,
        }
    }
}

/// The host's identifiers and values: the crate only compares them. The
/// implementing marker derives what the records do.
pub trait Ids: Clone + Debug + Eq {
    type Profile: Clone + Debug + Eq + Hash;
    type Binding: Clone + Debug + Eq;
    type Run: Clone + Debug + Eq;
    /// A principal recorded as confirmed (S1.13).
    type Principal: Clone + Debug + Eq;
    /// An identity tuple: a binding's, or a run's candidate (S4.7).
    type Identity: Clone + Debug + Eq;
}

/// A binding with the profile its changes count for (S4.9, I1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Of<I: Ids> {
    pub profile: I::Profile,
    pub binding: I::Binding,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// I2: in effect at once, committed once a trace of it lands.
    Tightening,
    /// I1: in effect once confirmed persisted.
    Loosening,
    /// S4.4: a generation value, in effect once confirmed persisted. It comes
    /// with an invalidation or a start, not through I1.
    Generation,
    /// A.4: a Provider run starting, or shutting down cleanly.
    Lifecycle,
}

/// One persisted change. Pairs are listed tightening first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change<I: Ids> {
    Off,
    On,
    /// The global tightening's prompt awaits the user (S1.14).
    Prompt,
    /// The user confirmed it: host-level confirmation.
    PromptConfirmed,
    Disable(I::Profile),
    Enable(I::Profile),
    Unselect(I::Profile),
    Select(I::Profile),
    /// A confirmation awaited (S1.13).
    Await {
        profile: I::Profile,
        awaited: Confirmation,
    },
    /// The user's confirmation, with what it answered. Only an answer to
    /// `principal_changed` confirms `principal` (S1.13).
    Confirm {
        profile: I::Profile,
        principal: I::Principal,
        awaited: Confirmation,
    },
    /// A failure the placeholder recorded (S2.5).
    PlaceholderFailed {
        profile: I::Profile,
        failure: PlaceholderFailure,
    },
    PlaceholderCleared {
        profile: I::Profile,
        failure: PlaceholderFailure,
    },
    /// The profile removed; its binding's retirement is its own record.
    Remove(I::Profile),
    Retire(Of<I>),
    /// A new binding with its identity and first value (S4.4). `confirmed`
    /// is its principal when none was awaited: recorded as confirmed too
    /// (S1.13).
    Bind {
        of: Of<I>,
        identity: I::Identity,
        generation: u64,
        confirmed: Option<I::Principal>,
    },
    Generation {
        of: Of<I>,
        value: u64,
    },
    /// Servable for the first time (S1.12): never taken back. It closes S3.1's
    /// exception, so it is the one loosening that outlives a void.
    Servable(Of<I>),
    /// Logged out (S3.3).
    Logout(Of<I>),
    /// The same principal logged in again. Not listed in S1.10; a loosening
    /// (I1). One written before a tightening of its profile commits is for
    /// the wiring to recheck (slice 6).
    Relogin(Of<I>),
    /// A revocation-class event since the binding was created: S3.1's
    /// exception is over for good. Not listed in S1.10; a tightening, as
    /// losing it would let the exception apply again.
    Revoked(Of<I>),
    /// Into a reason that does not retry by itself (S1.4).
    Stuck {
        of: Of<I>,
        cause: StuckCause,
    },
    /// Out of it, with the reason left.
    Unstuck {
        of: Of<I>,
        cause: StuckCause,
    },
    /// A run left with no binding, over the profiles of what bound it.
    RunUnbound {
        run: I::Run,
        profiles: Vec<I::Profile>,
    },
    /// A run's binding set and candidate identities (S4.7).
    RunBound {
        run: I::Run,
        bindings: Vec<Of<I>>,
        candidates: Vec<(I::Profile, I::Identity)>,
    },
    /// The one binding an empty run set records at its first dispatch.
    RunBackfilled {
        run: I::Run,
        of: Of<I>,
    },
    Started,
    Shutdown,
}

impl<I: Ids> Change<I> {
    pub fn class(&self) -> Class {
        match self {
            Self::Off
            | Self::Prompt
            | Self::Disable(_)
            | Self::Unselect(_)
            | Self::Await { .. }
            | Self::PlaceholderFailed { .. }
            | Self::Remove(_)
            | Self::Retire(_)
            | Self::Logout(_)
            | Self::Revoked(_)
            | Self::Stuck { .. }
            | Self::RunUnbound { .. } => Class::Tightening,
            Self::On
            | Self::PromptConfirmed
            | Self::Enable(_)
            | Self::Select(_)
            | Self::Confirm { .. }
            | Self::PlaceholderCleared { .. }
            | Self::Bind { .. }
            | Self::Servable(_)
            | Self::Relogin(_)
            | Self::Unstuck { .. }
            | Self::RunBound { .. }
            | Self::RunBackfilled { .. } => Class::Loosening,
            Self::Generation { .. } => Class::Generation,
            Self::Started | Self::Shutdown => Class::Lifecycle,
        }
    }

    /// What [`Ledger`](super::ledger::Ledger) blocks and is blocked by (I1).
    /// `None` outside its classes. A run with no profile at all is host level:
    /// stricter, never empty.
    pub fn scope(&self) -> Option<Scope<I::Profile>> {
        let profiles = match self {
            Self::Off | Self::On | Self::Prompt | Self::PromptConfirmed => {
                return Some(Scope::Host);
            }
            Self::Disable(p)
            | Self::Enable(p)
            | Self::Unselect(p)
            | Self::Select(p)
            | Self::Remove(p)
            | Self::Await { profile: p, .. }
            | Self::Confirm { profile: p, .. }
            | Self::PlaceholderFailed { profile: p, .. }
            | Self::PlaceholderCleared { profile: p, .. } => vec![p.clone()],
            Self::Retire(of)
            | Self::Servable(of)
            | Self::Logout(of)
            | Self::Relogin(of)
            | Self::Revoked(of)
            | Self::Bind { of, .. }
            | Self::Stuck { of, .. }
            | Self::Unstuck { of, .. }
            | Self::RunBackfilled { of, .. } => vec![of.profile.clone()],
            Self::RunUnbound { profiles, .. } => profiles.clone(),
            Self::RunBound {
                bindings,
                candidates,
                ..
            } => bound_profiles(bindings, candidates),
            Self::Generation { .. } | Self::Started | Self::Shutdown => return None,
        };
        Some(if profiles.is_empty() {
            Scope::Host
        } else {
            Scope::Profiles(profiles)
        })
    }

    /// A.5: the tightening an abandoned loosening is offset with, over the
    /// same scope. `None` outside loosenings, and for "once servable": it errs
    /// on the strict side either way, so the host never abandons a pending
    /// one. Unwritten, the binding stays `local_storage` (S1.12); landing
    /// late, it outlives the void.
    pub fn offset(&self) -> Option<Self> {
        Some(match self {
            Self::On => Self::Off,
            Self::PromptConfirmed => Self::Prompt,
            Self::Enable(p) => Self::Disable(p.clone()),
            Self::Select(p) => Self::Unselect(p.clone()),
            Self::Confirm {
                profile, awaited, ..
            } => Self::Await {
                profile: profile.clone(),
                awaited: *awaited,
            },
            Self::PlaceholderCleared { profile, failure } => Self::PlaceholderFailed {
                profile: profile.clone(),
                failure: *failure,
            },
            Self::Bind { of, .. } => Self::Retire(of.clone()),
            Self::Relogin(of) => Self::Logout(of.clone()),
            Self::Unstuck { of, cause } => Self::Stuck {
                of: of.clone(),
                cause: *cause,
            },
            Self::RunBound {
                run,
                bindings,
                candidates,
            } => Self::RunUnbound {
                run: run.clone(),
                profiles: bound_profiles(bindings, candidates),
            },
            Self::RunBackfilled { run, of } => Self::RunUnbound {
                run: run.clone(),
                profiles: vec![of.profile.clone()],
            },
            // Listed, not `_`: a new loosening must say what offsets it.
            Self::Servable(_)
            | Self::Off
            | Self::Prompt
            | Self::Disable(_)
            | Self::Unselect(_)
            | Self::Await { .. }
            | Self::PlaceholderFailed { .. }
            | Self::Remove(_)
            | Self::Retire(_)
            | Self::Generation { .. }
            | Self::Logout(_)
            | Self::Revoked(_)
            | Self::Stuck { .. }
            | Self::RunUnbound { .. }
            | Self::Started
            | Self::Shutdown => return None,
        })
    }

    /// A.1: this record landed late on a sequence number already given up.
    /// The stricter of the two stands: a loosening is ignored (I1, like the
    /// late confirmation of an abandoned one), a tightening kept (I2), a
    /// generation value kept (only ever higher values follow, S4.4). "Once
    /// servable" is kept: dropping it would bring S3.1's exception back, and
    /// when unsure the binding counts as servable (S1.12). It admits nothing:
    /// after a restart a binding waits for its raised generation (S1.14). A
    /// clean shutdown is ignored: it would let the next start's increment be
    /// the small one (A.4).
    pub fn outlives_void(&self) -> bool {
        match self.class() {
            Class::Loosening => matches!(self, Self::Servable(_)),
            Class::Lifecycle => !matches!(self, Self::Shutdown),
            Class::Tightening | Class::Generation => true,
        }
    }
}

fn bound_profiles<I: Ids>(
    bindings: &[Of<I>],
    candidates: &[(I::Profile, I::Identity)],
) -> Vec<I::Profile> {
    let mut profiles: Vec<I::Profile> = Vec::new();
    let named = bindings.iter().map(|of| &of.profile);
    for p in named.chain(candidates.iter().map(|(p, _)| p)) {
        if !profiles.contains(p) {
            profiles.push(p.clone());
        }
    }
    profiles
}

#[cfg(test)]
mod tests;
