//! One profile as the host holds it (KB-C1-04 6c): the profile's state with
//! its binding's id and its disabled flag, what a user or configuration event
//! on it persists and aborts (CONTRACT-STATE-v1 S3.3, S4.5, S5.6), and its
//! binding connecting again. Pure: the caller writes what it is given and
//! reports a generation back once it is confirmed persisted.

use super::binding::{Leave, WithRule};
use super::connection::Round;
use super::gate::CallId;
use super::profile::{Profile, Slot};
use super::record::{Change, Ids, Of};
use super::state::Switches;

#[derive(Debug)]
pub struct Member<I: Ids> {
    id: I::Profile,
    profile: Profile<I::Kb>,
    /// The id of the profile's binding, while it has one.
    binding: Option<I::Binding>,
    disabled: bool,
}

/// What events on members persist and abort; one write's worth.
#[derive(Debug, PartialEq, Eq)]
pub struct Left<I: Ids> {
    pub changes: Vec<Change<I>>,
    /// The generation values among `changes`: each takes effect once they
    /// are confirmed persisted (S4.4).
    pub generations: Vec<(Of<I>, u64)>,
    /// Abort these calls and answer them `cancelled` (S4.5).
    pub aborted: Vec<CallId>,
}

impl<I: Ids> Left<I> {
    /// Starting with the event's own change.
    pub fn new(change: Change<I>) -> Self {
        Self {
            changes: vec![change],
            generations: Vec::new(),
            aborted: Vec::new(),
        }
    }
}

impl<I: Ids> Member<I> {
    /// As restored or configured; `binding` names the bound slot's binding.
    pub fn new(
        id: I::Profile,
        profile: Profile<I::Kb>,
        binding: Option<I::Binding>,
        disabled: bool,
    ) -> Self {
        debug_assert_eq!(binding.is_some(), matches!(profile.slot(), Slot::Bound(_)));
        Self {
            id,
            profile,
            binding,
            disabled,
        }
    }

    pub fn id(&self) -> &I::Profile {
        &self.id
    }

    pub fn profile(&self) -> &Profile<I::Kb> {
        &self.profile
    }

    pub fn profile_mut(&mut self) -> &mut Profile<I::Kb> {
        &mut self.profile
    }

    pub fn disabled(&self) -> bool {
        self.disabled
    }

    pub fn set_disabled(&mut self, disabled: bool) {
        self.disabled = disabled;
    }

    /// S3.3: the event on the profile; its binding's placeholder rule and the
    /// generation it invalidated to go into `left` (S5.6, S4.5).
    pub fn leave(&mut self, event: Leave, left: &mut Left<I>) {
        let WithRule {
            value: effects,
            rule,
        } = self.profile.leave(event);
        let Some(binding) = &self.binding else {
            return;
        };
        let of = Of {
            profile: self.id.clone(),
            binding: binding.clone(),
        };
        if let Some(rule) = rule {
            let of = of.clone();
            left.changes.push(Change::Placed { of, rule });
        }
        if let Some(invalidation) = effects.invalidation {
            left.aborted.extend(invalidation.aborted);
            if let Some(value) = invalidation.to_persist {
                let generation = Change::Generation {
                    of: of.clone(),
                    value,
                };
                left.changes.push(generation);
                left.generations.push((of, value));
            }
        }
    }

    /// The binding enters `loading` if the switches and its state let it.
    pub fn connect(&mut self, off: bool) -> Option<Round> {
        let switches = Switches {
            off,
            profile_disabled: self.disabled,
        };
        match self.profile.slot_mut() {
            Slot::Bound(binding) => binding.connect(switches),
            Slot::Placeholder(_) => None,
        }
    }

    /// The generation `value` written for `of` is confirmed persisted. Not
    /// this member's binding any more: nothing moves.
    pub fn confirm_generation(&mut self, of: &Of<I>, value: u64) -> Option<Round> {
        if of.profile != self.id || self.binding.as_ref() != Some(&of.binding) {
            return None;
        }
        match self.profile.slot_mut() {
            Slot::Bound(binding) => binding.connection_mut().confirm_generation(value),
            Slot::Placeholder(_) => None,
        }
    }
}

#[cfg(test)]
mod tests;
