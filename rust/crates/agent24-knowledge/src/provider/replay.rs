//! Replay (CONTRACT-STATE-v1 S1.14 normal recovery; appendix A.1, A.3 as
//! reference): the log's records, in order, give back what was persisted.
//! Pure: the host reads, checks and truncates the file first (A.3); whether
//! the result may be trusted is the restart decision's (`recover`, next).

use std::collections::HashMap;

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileState<I: Ids> {
    pub disabled: bool,
    pub selected: bool,
    pub removed: bool,
    pub awaiting: Option<Confirmation>,
    pub confirmed: Option<I::Principal>,
    pub failure: Option<PlaceholderFailure>,
}

impl<I: Ids> Default for ProfileState<I> {
    fn default() -> Self {
        Self {
            disabled: false,
            selected: false,
            removed: false,
            awaiting: None,
            confirmed: None,
            failure: None,
        }
    }
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
}

/// A run's binding set and candidates (S4.7); `None` once unbound.
pub type RunSet<I> = Option<(Vec<Of<I>>, Vec<(<I as Ids>::Profile, <I as Ids>::Identity)>)>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Replayed<I: Ids> {
    /// Off until an On is persisted (S5.2).
    pub off: bool,
    /// The global tightening's prompt still awaits the user (S1.14).
    pub prompt: bool,
    pub profiles: HashMap<I::Profile, ProfileState<I>>,
    pub bindings: Vec<Bound<I>>,
    pub runs: Vec<(I::Run, RunSet<I>)>,
    /// The highest generation in the log, for any binding: retired ones, and
    /// new ones whose write was given up. New values go past it (S4.4).
    pub highest: u64,
    /// The last record is a clean shutdown (A.4).
    pub shut_down: bool,
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
        bindings: Vec::new(),
        runs: Vec::new(),
        highest: 0,
        shut_down: false,
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
                    settle(&mut out, done);
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
        settle(&mut out, done);
    }
    Ok(out)
}

/// What one number was found to hold.
struct Slot<I: Ids> {
    seq: u64,
    write: Option<Vec<Change<I>>>,
    void: bool,
}

fn settle<I: Ids>(out: &mut Replayed<I>, Slot { write, void, .. }: Slot<I>) {
    out.shut_down = false;
    for change in write.into_iter().flatten() {
        if !void || change.outlives_void() {
            apply(out, change);
        } else if let Change::Bind { generation, .. } = change {
            // The binding is not there, but its first value may have been
            // used: new values still go past it (S4.4).
            out.highest = out.highest.max(generation);
        }
    }
}

fn apply<I: Ids>(out: &mut Replayed<I>, change: Change<I>) {
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
            let state = profile(out, p);
            if state.awaiting == Some(awaited) {
                state.awaiting = None;
                state.confirmed = Some(principal);
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
            out.highest = out.highest.max(generation);
            if let Some(principal) = confirmed {
                profile(out, of.profile.clone()).confirmed = Some(principal);
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
}

fn profile<I: Ids>(out: &mut Replayed<I>, p: I::Profile) -> &mut ProfileState<I> {
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

fn run_set<I: Ids>(out: &mut Replayed<I>, run: I::Run) -> &mut RunSet<I> {
    let at = match out.runs.iter().position(|(r, _)| *r == run) {
        Some(at) => at,
        None => {
            out.runs.push((run, None));
            out.runs.len() - 1
        }
    };
    &mut out.runs[at].1
}

#[cfg(test)]
mod tests;
