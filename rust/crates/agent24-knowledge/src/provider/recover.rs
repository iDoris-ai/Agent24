//! The restart decision (CONTRACT-STATE-v1 S1.14, I3; appendix A.2–A.4 as
//! reference): from what was read back, the seal and the host's first-install
//! mark, which of the four cases holds and the tightenings it persists. Pure:
//! the host reads, truncates and writes (A.3, KB-C1-04 slice 6). The value
//! each binding starts at (A.4) is the host's; placeholder rules and evidence
//! epochs join with slice 5.

use super::profile::Confirmation;
use super::record::{Change, Ids};
use super::replay::{Bound, Damaged, Replayed};

/// The seal as found at start (A.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Seal {
    /// Read whole. `next` is the number the log's next record took when the
    /// seal was rebuilt.
    Present { next: u64 },
    /// Missing or unreadable.
    Missing,
}

/// A binding's identity against what is configured now (S1.14, S2.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Found {
    /// The profile is no longer configured, or its endpoint, tenant or
    /// configuration revision changed.
    Gone,
    /// Configured as it was, with no long-term login (logged out) or one of
    /// the binding's principal.
    Same,
    /// Configured as it was, with a long-term login of another principal.
    OtherPrincipal,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Recovery<I: Ids> {
    /// No persisted state, no profile configured, and the host confirms a
    /// first install (no Knowledge evidence in the session history, FI-3f):
    /// Off, no prompt, nothing tightened. The host creates the seal (A.3).
    FirstInstall,
    /// The persisted state stands. `retire` holds the bindings whose identity
    /// no longer holds, to persist as tightenings before any admission. A
    /// prompt still pending (`state.prompt`) is shown again (FI-8d).
    Normal {
        state: Replayed<I>,
        retire: Vec<Change<I>>,
    },
    /// It cannot be told that every committed tightening was persisted: the
    /// state read, and the global tightening to persist over it. The host also
    /// voids every long-term login, outside the log, so a tightening of its
    /// own (FI-7c).
    Tightened {
        state: Replayed<I>,
        writes: Vec<Change<I>>,
    },
    /// Damaged, or missing without a confirmed first install (S1.8, FI-3b,
    /// FI-3f): the global tightening, and every profile an unselected,
    /// disabled placeholder. No old binding carries over: their evidence
    /// becomes placeholders (I5), and new first values go past any used
    /// before (S4.4, A.4). The host writes these as a new log's baseline once
    /// the user confirms (A.3).
    Damaged { writes: Vec<Change<I>> },
}

/// `log` is `None` when there is no persisted state at all. `profiles` are
/// the ones configured now; `found` tells each binding's identity against
/// them.
pub fn recover<I: Ids>(
    log: Option<Result<Replayed<I>, Damaged>>,
    seal: Seal,
    first_install: bool,
    profiles: &[I::Profile],
    found: impl Fn(&Bound<I>) -> Found,
) -> Recovery<I> {
    let state = match log {
        None if first_install && profiles.is_empty() && seal == Seal::Missing => {
            return Recovery::FirstInstall;
        }
        None | Some(Err(Damaged)) => {
            let mut writes = global();
            for p in profiles {
                writes.extend([Change::Unselect(p.clone()), Change::Disable(p.clone())]);
                writes.push(unsaved(p));
            }
            return Recovery::Damaged { writes };
        }
        Some(Ok(state)) => state,
    };
    match seal {
        // A seal past the log's end: records it had seen are gone (A.2).
        Seal::Present { next } if next <= state.next => {
            let retire = retired(&state, found);
            Recovery::Normal { state, retire }
        }
        Seal::Present { .. } | Seal::Missing => {
            let mut writes = global();
            writes.extend(live(&state).map(|b| Change::Retire(b.of.clone())));
            writes.extend(named(&state, profiles).iter().map(unsaved));
            Recovery::Tightened { state, writes }
        }
    }
}

/// Off and the prompt the user confirms before On (S1.14).
fn global<I: Ids>() -> Vec<Change<I>> {
    vec![Change::Off, Change::Prompt]
}

/// It takes the place of `principal_changed` (FI-8i).
fn unsaved<I: Ids>(profile: &I::Profile) -> Change<I> {
    Change::Await {
        profile: profile.clone(),
        awaited: Confirmation::UnsavedSettings,
    }
}

/// Configured now, or named in the state: one removed before its removal
/// was persisted still awaits the user (FI-6d).
fn named<I: Ids>(state: &Replayed<I>, profiles: &[I::Profile]) -> Vec<I::Profile> {
    let mut all = profiles.to_vec();
    let bound = state.bindings.iter().map(|b| &b.of.profile);
    for p in bound.chain(state.profiles.keys()) {
        if !all.contains(p) {
            all.push(p.clone());
        }
    }
    all
}

fn live<I: Ids>(state: &Replayed<I>) -> impl Iterator<Item = &Bound<I>> {
    state.bindings.iter().filter(|b| !b.retired)
}

/// S1.6, S1.14: retired once its profile is removed or no longer configured
/// as it was, or its long-term login is another principal's. A profile
/// awaiting `principal_changed` has the principal checked at confirmation
/// instead; a logged-out binding has none to check (FI-6g).
fn retired<I: Ids>(state: &Replayed<I>, found: impl Fn(&Bound<I>) -> Found) -> Vec<Change<I>> {
    let at = |b: &Bound<I>| state.profiles.get(&b.of.profile);
    live(state)
        .filter(|b| {
            let removed = at(b).is_some_and(|p| p.removed);
            let awaited = at(b).and_then(|p| p.awaiting);
            removed
                || match found(b) {
                    Found::Gone => true,
                    Found::OtherPrincipal => awaited != Some(Confirmation::PrincipalChanged),
                    Found::Same => false,
                }
        })
        .map(|b| Change::Retire(b.of.clone()))
        .collect()
}

#[cfg(test)]
mod tests;
