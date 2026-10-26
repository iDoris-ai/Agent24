//! Replay (CONTRACT-STATE-v1 S1.14 normal recovery; appendix A.1, A.3 as
//! reference): the log's records, in order, give back what was persisted.
//! Pure: the host reads, checks and truncates the file first (A.3); whether
//! the result may be trusted is the restart decision's (`recover`, next).

use std::collections::HashMap;

use super::evidence::{Kbs, Pending, Saved};
use super::profile::Confirmation;
use super::record::{Change, Ids, Of, StuckCause};
use super::state::PlaceholderFailure;

/// What one sequence number holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry<I: Ids> {
    /// Changes persisted in one atomic write (S1.13, S3.5).
    Changes(Vec<Change<I>>),
    /// The number given up after a write with an unknown outcome (A.1).
    Void,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record<I: Ids> {
    pub seq: u64,
    pub entry: Entry<I>,
}

/// Numbers out of order or missing, or one number holding two different
/// writes: the persisted state is damaged (S1.8, A.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Damaged;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProfileState {
    pub disabled: bool,
    pub selected: bool,
    pub removed: bool,
    pub awaiting: Option<Confirmation>,
    pub failure: Option<PlaceholderFailure>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bound<I: Ids> {
    pub of: Of<I>,
    pub identity: I::Identity,
    /// The highest value persisted (S4.4).
    pub generation: u64,
    pub retired: bool,
    pub servable: bool,
    pub logged_out: bool,
    pub revoked: bool,
    pub stuck: Option<StuckCause>,
    /// To restore its [`Evidence`](super::evidence::Evidence) from.
    pub evidence: Saved<I::Kb>,
}

/// A run's binding set and candidates (S4.7); `None` once unbound.
pub type RunSet<I> = Option<(Vec<Of<I>>, Vec<(<I as Ids>::Profile, <I as Ids>::Identity)>)>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Replayed<I: Ids> {
    /// Off until an On is persisted (S5.2).
    pub off: bool,
    /// The global tightening's prompt still awaits the user (S1.14).
    pub prompt: bool,
    pub profiles: HashMap<I::Profile, ProfileState>,
    /// Each profile's last confirmed principal (S1.13).
    pub confirmed: HashMap<I::Profile, I::Principal>,
    pub bindings: Vec<Bound<I>>,
    pub runs: Vec<(I::Run, RunSet<I>)>,
    /// The highest generation in the log, for any binding: retired ones, and
    /// new ones whose write was given up. New values go past it (S4.4).
    pub highest: u64,
    /// The last record is a clean shutdown (A.4).
    pub shut_down: bool,
    /// The number the next record takes: `start` for an empty log.
    pub next: u64,
}

/// Replays `records` in the order read. The first number is `start`, the
/// one the log's header or baseline gives (A.3), so a log missing its head
/// is damage; each number follows the one before. A number may hold a write
/// and its void, in either order, each possibly twice (a retry). Of a write
/// found with its void only the changes that outlive it count
/// ([`Change::outlives_void`]): an atomic write is split, the stricter part
/// kept.
pub fn replay<I: Ids>(
    start: u64,
    records: impl IntoIterator<Item = Record<I>>,
) -> Result<Replayed<I>, Damaged> {
    let mut out = Replayed {
        off: true,
        prompt: false,
        profiles: HashMap::new(),
        confirmed: HashMap::new(),
        bindings: Vec::new(),
        runs: Vec::new(),
        highest: 0,
        shut_down: false,
        next: start,
    };
    let mut slot: Option<Slot<I>> = None;
    for Record { seq, entry } in records {
        match &mut slot {
            Some(s) if s.seq == seq => match entry {
                Entry::Void => s.void = true,
                Entry::Changes(c) if s.write.is_none() => s.write = Some(c),
                Entry::Changes(c) if s.write.as_ref() == Some(&c) => {}
                Entry::Changes(_) => return Err(Damaged),
            },
            _ => {
                let expected = match &slot {
                    Some(s) => s.seq.checked_add(1).ok_or(Damaged)?,
                    None => start,
                };
                if seq != expected {
                    return Err(Damaged);
                }
                if let Some(done) = slot.take() {
                    settle(&mut out, done)?;
                }
                let (write, void) = match entry {
                    Entry::Changes(c) => (Some(c), false),
                    Entry::Void => (None, true),
                };
                slot = Some(Slot { seq, write, void });
            }
        }
    }
    if let Some(done) = slot {
        out.next = done.seq.checked_add(1).ok_or(Damaged)?;
        settle(&mut out, done)?;
    }
    Ok(out)
}

/// What one number was found to hold.
struct Slot<I: Ids> {
    seq: u64,
    write: Option<Vec<Change<I>>>,
    void: bool,
}

fn settle<I: Ids>(out: &mut Replayed<I>, Slot { write, void, .. }: Slot<I>) -> Result<(), Damaged> {
    out.shut_down = false;
    for change in write.into_iter().flatten() {
        if !void || change.outlives_void() {
            apply(out, change)?;
        } else if let Change::Bind { generation, .. } = change {
            // The binding is not there, but its first value may have been
            // used: new values still go past it (S4.4).
            out.highest = out.highest.max(generation);
        }
    }
    Ok(())
}

fn apply<I: Ids>(out: &mut Replayed<I>, change: Change<I>) -> Result<(), Damaged> {
    match change {
        Change::Off => out.off = true,
        Change::On => out.off = false,
        Change::Prompt => out.prompt = true,
        Change::PromptConfirmed => out.prompt = false,
        Change::Disable(p) => profile(out, p).disabled = true,
        Change::Enable(p) => profile(out, p).disabled = false,
        Change::Unselect(p) => profile(out, p).selected = false,
        Change::Select(p) => profile(out, p).selected = true,
        Change::Remove(p) => profile(out, p).removed = true,
        Change::Await {
            profile: p,
            awaited,
        } => {
            let state = profile(out, p);
            // `unsaved_settings` takes the place of `principal_changed`.
            if state.awaiting != Some(Confirmation::UnsavedSettings) {
                state.awaiting = Some(awaited);
            }
        }
        Change::Confirm {
            profile: p,
            principal,
            awaited,
        } => {
            let state = profile(out, p.clone());
            if state.awaiting == Some(awaited) {
                state.awaiting = None;
                // `unsaved_settings` only clears: a new principal awaits its
                // own confirmation (S1.13).
                if awaited == Confirmation::PrincipalChanged {
                    out.confirmed.insert(p, principal);
                }
            }
        }
        Change::PlaceholderFailed {
            profile: p,
            failure,
        } => {
            profile(out, p).failure = Some(failure);
        }
        Change::PlaceholderCleared {
            profile: p,
            failure,
        } => {
            let state = profile(out, p);
            if state.failure == Some(failure) {
                state.failure = None;
            }
        }
        Change::Bind {
            of,
            identity,
            generation,
            confirmed,
        } => {
            // Bound twice, a first value not past one its profile used, or
            // none at all: no sound host wrote this (S2.6, S4.4, S1.8).
            let used = |b: &Bound<I>| b.of.profile == of.profile && b.generation >= generation;
            if generation == 0 || out.bindings.iter().any(|b| b.of == of || used(b)) {
                return Err(Damaged);
            }
            out.highest = out.highest.max(generation);
            if let Some(principal) = confirmed {
                out.confirmed.insert(of.profile.clone(), principal);
            }
            out.bindings.push(Bound {
                of,
                identity,
                generation,
                retired: false,
                servable: false,
                logged_out: false,
                revoked: false,
                stuck: None,
                evidence: Saved::default(),
            });
        }
        Change::Generation { of, value } => {
            out.highest = out.highest.max(value);
            if let Some(b) = bound(out, &of) {
                b.generation = b.generation.max(value);
            }
        }
        Change::Retire(of) => on(out, &of, |b| b.retired = true),
        Change::Servable(of) => on(out, &of, |b| b.servable = true),
        Change::Logout(of) => on(out, &of, |b| b.logged_out = true),
        Change::Relogin(of) => on(out, &of, |b| b.logged_out = false),
        Change::Revoked(of) => on(out, &of, |b| b.revoked = true),
        Change::Stuck { of, cause } => on(out, &of, |b| b.stuck = Some(cause)),
        Change::Unstuck { of, cause } => on(out, &of, |b| {
            if b.stuck == Some(cause) {
                b.stuck = None;
            }
        }),
        Change::Placed { of, rule } => {
            if let Some(e) = bound(out, &of).map(|b| &mut b.evidence) {
                // Epochs only advance (S5.6, S1.8).
                if rule.epoch <= e.epoch {
                    return Err(Damaged);
                }
                e.epoch = rule.epoch;
                if let Kbs::One(kb) = &rule.kbs {
                    e.tombstones.remove(kb);
                }
                e.rules.push(rule);
            }
        }
        Change::Questioned {
            of,
            kbs,
            version,
            timeouts,
        } => evidence(out, &of, |e| {
            // Pending anew after a release that landed though abandoned: the
            // count comes back with it.
            let pending = e.pending.get_or_insert_with(|| Pending {
                kbs: Default::default(),
                timeouts,
            });
            pending.kbs.extend(kbs);
            e.version = e.version.max(version);
        }),
        Change::Released { of, version, .. } => evidence(out, &of, |e| {
            if e.version == version {
                e.pending = None;
            }
        }),
        Change::TimedOut { of, count } => evidence(out, &of, |e| {
            if let Some(p) = &mut e.pending {
                p.timeouts = count;
            }
        }),
        Change::TimeoutsCleared { of, .. } => evidence(out, &of, |e| {
            if let Some(p) = &mut e.pending {
                p.timeouts = 0;
            }
        }),
        Change::Tombstone { of, kb } => evidence(out, &of, |e| {
            e.visible.remove(&kb);
            e.tombstones.insert(kb);
        }),
        Change::TombstoneWithdrawn { of, kb } => evidence(out, &of, |e| {
            e.tombstones.remove(&kb);
        }),
        Change::Seen { of, kbs } => evidence(out, &of, |e| e.visible.extend(kbs)),
        Change::Unseen { of, kbs } => evidence(out, &of, |e| {
            e.visible.retain(|kb| !kbs.contains(kb));
        }),
        Change::RunBound {
            run,
            bindings,
            candidates,
        } => *run_set(out, run) = Some((bindings, candidates)),
        Change::RunBackfilled { run, of } => {
            // Only into a set recorded empty (S4.7).
            let set = out.runs.iter_mut().find(|(r, _)| *r == run);
            if let Some((_, Some((bindings, _)))) =
                set.filter(|(_, s)| s.as_ref().is_some_and(|(b, _)| b.is_empty()))
            {
                bindings.push(of);
            }
        }
        Change::RunUnbound { run, .. } => *run_set(out, run) = None,
        Change::Started => {}
        Change::Shutdown => out.shut_down = true,
    }
    Ok(())
}

fn profile<I: Ids>(out: &mut Replayed<I>, p: I::Profile) -> &mut ProfileState {
    out.profiles.entry(p).or_default()
}

/// A change to a binding whose own record is not there changes nothing: the
/// binding does not exist after the restart.
fn bound<'a, I: Ids>(out: &'a mut Replayed<I>, of: &Of<I>) -> Option<&'a mut Bound<I>> {
    out.bindings.iter_mut().find(|b| b.of == *of)
}

fn on<I: Ids>(out: &mut Replayed<I>, of: &Of<I>, f: impl FnOnce(&mut Bound<I>)) {
    if let Some(b) = bound(out, of) {
        f(b);
    }
}

fn evidence<I: Ids>(out: &mut Replayed<I>, of: &Of<I>, f: impl FnOnce(&mut Saved<I::Kb>)) {
    on(out, of, |b| f(&mut b.evidence));
}

fn run_set<I: Ids>(out: &mut Replayed<I>, run: I::Run) -> &mut RunSet<I> {
    let at = out.runs.iter().position(|(r, _)| *r == run);
    let at = at.unwrap_or_else(|| {
        out.runs.push((run, None));
        out.runs.len() - 1
    });
    &mut out.runs[at].1
}

#[cfg(test)]
mod tests;
