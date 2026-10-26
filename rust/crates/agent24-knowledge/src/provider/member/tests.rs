#![allow(clippy::unwrap_used)]

use super::*;
use crate::provider::binding::BindingState;
use crate::provider::connection::Connection;
use crate::provider::credential::Credentials;
use crate::provider::evidence::{Kbs, Rule};
use crate::provider::gate::Gate;
use crate::provider::placeholder::Placeholder;
use crate::provider::state::{ConnectionPhase, UnavailableCause};

#[derive(Clone, Debug, PartialEq, Eq)]
struct T;

impl Ids for T {
    type Profile = &'static str;
    type Binding = u32;
    type Run = u32;
    type Principal = &'static str;
    type Identity = &'static str;
    type Kb = &'static str;
}

type M = Member<T>;
type C = Change<T>;

const OF: Of<T> = Of {
    profile: "a",
    binding: 7,
};

/// `of`, selected, bound at `generation` and `loading`.
fn bound(of: &Of<T>, generation: u64) -> M {
    let binding = BindingState::new(
        Connection::new(Gate::new(generation), true),
        Credentials::new(false),
        true,
    );
    let profile = Profile::new(Slot::Bound(Box::new(binding)), true, None);
    let mut member = M::new(of.profile, profile, Some(of.binding), false);
    assert!(member.connect(false).is_some());
    member
}

/// "a", bound to binding 7 at generation 1 and `loading`.
fn loading() -> M {
    bound(&OF, 1)
}

fn call(member: &mut M) -> CallId {
    binding(member).connection_mut().open_call().unwrap().0
}

fn binding(member: &mut M) -> &mut BindingState<&'static str> {
    match member.profile_mut().slot_mut() {
        Slot::Bound(binding) => binding,
        Slot::Placeholder(_) => panic!("a placeholder"),
    }
}

fn generation(member: &mut M) -> u64 {
    binding(member).connection().gate().generation()
}

/// S3.3, S4.5: leaving the connection phase invalidates; the calls in flight
/// are aborted and the new generation is written. Off places no rule (S5.6).
#[test]
fn off_aborts_the_calls_and_writes_the_generation_without_a_rule() {
    let mut member = loading();
    let call = call(&mut member);
    let mut left = Left::new(C::Off);
    member.leave(Leave::Off, &mut left);
    assert_eq!(left.changes, [C::Off, C::Generation { of: OF, value: 2 }]);
    assert_eq!(left.generations, [(OF, 2)]);
    assert_eq!(left.aborted, [(OF, call)]);
    assert_eq!(binding(&mut member).connection().phase(), None);
}

/// S5.6: every event but Off advances the evidence epoch with a rule over
/// every KB, written with it; out of the connection phase, no generation.
#[test]
fn unselect_places_a_rule_over_every_kb() {
    let mut member = loading();
    let mut left = Left::new(C::Off);
    member.leave(Leave::Off, &mut left);
    let mut left = Left::new(C::Unselect("a"));
    member.leave(Leave::Unselect, &mut left);
    let rule = Rule {
        epoch: 1,
        kbs: Kbs::All,
    };
    assert_eq!(left.changes, [C::Unselect("a"), C::Placed { of: OF, rule }]);
    assert!(left.generations.is_empty() && left.aborted.is_empty());
    assert!(!member.profile().selected());
}

#[test]
fn a_placeholder_writes_only_the_event() {
    let placeholder = Slot::Placeholder(Placeholder::new(None));
    let mut member = M::new("b", Profile::new(placeholder, true, None), None, false);
    let mut left = Left::new(C::Unselect("b"));
    member.leave(Leave::Unselect, &mut left);
    assert_eq!(left, Left::new(C::Unselect("b")));
    assert!(!member.profile().selected(), "remembered all the same");
    assert_eq!(member.connect(false), None);
}

/// S1.2: it connects again only with neither switch on.
#[test]
fn it_connects_with_neither_switch_on() {
    let mut member = loading();
    member.leave(Leave::Off, &mut Left::new(C::Off));
    member.confirm_generation(&OF, 2);
    assert_eq!(member.connect(true), None, "Off");
    member.set_disabled(true);
    assert_eq!(member.connect(false), None, "disabled");
    member.set_disabled(false);
    assert!(member.connect(false).is_some());
    assert_eq!(
        binding(&mut member).connection().phase(),
        Some(ConnectionPhase::Loading)
    );
}

/// S4.4: the generation takes effect once confirmed, and only for the
/// binding and the value it was written for.
#[test]
fn a_generation_takes_effect_for_its_own_binding_only() {
    let mut member = loading();
    member.leave(Leave::Off, &mut Left::new(C::Off));
    for other in [Of { binding: 8, ..OF }, Of { profile: "b", ..OF }] {
        member.confirm_generation(&other, 2);
        assert_eq!(generation(&mut member), 1, "{other:?}");
    }
    assert_eq!(member.confirm_generation(&OF, 3), None);
    assert_eq!(generation(&mut member), 1, "not the value written");
    member.confirm_generation(&OF, 2);
    assert_eq!(generation(&mut member), 2);
}

/// Every event but Off in the connection phase: the rule and the generation
/// go into the same write, and the calls in flight are aborted (S3.3).
#[test]
fn an_event_in_the_connection_phase_writes_its_rule_and_its_generation() {
    for event in [
        Leave::Disable,
        Leave::Unselect,
        Leave::Logout,
        Leave::Remove,
        Leave::Retire,
    ] {
        let mut member = loading();
        let call = call(&mut member);
        let mut left = Left::new(C::Disable("a"));
        member.leave(event, &mut left);
        let rule = Rule {
            epoch: 1,
            kbs: Kbs::All,
        };
        let changes = [
            C::Disable("a"),
            C::Placed { of: OF, rule },
            C::Generation { of: OF, value: 2 },
        ];
        assert_eq!(left.changes, changes, "{event:?}");
        assert_eq!(left.aborted, [(OF, call)], "{event:?}");
    }
}

/// S4.5: an invalidation takes effect even with no greater value left; the
/// calls are aborted all the same, and no value is written.
#[test]
fn an_exhausted_generation_still_aborts_the_calls() {
    let mut member = bound(&OF, u64::MAX);
    let call = call(&mut member);
    let mut left = Left::new(C::Off);
    member.leave(Leave::Off, &mut left);
    assert_eq!(left.changes, [C::Off]);
    assert!(left.generations.is_empty());
    assert_eq!(left.aborted, [(OF, call)]);
}

/// Call ids are each binding's own: two bindings' first calls share one, and
/// the binding tells them apart.
#[test]
fn the_calls_of_two_bindings_stay_apart() {
    let b = Of {
        profile: "b",
        binding: 8,
    };
    let (mut a, mut other) = (loading(), bound(&b, 1));
    let (first, second) = (call(&mut a), call(&mut other));
    assert_eq!(first, second);
    let mut left = Left::new(C::Off);
    a.leave(Leave::Off, &mut left);
    other.leave(Leave::Off, &mut left);
    assert_eq!(left.aborted, [(OF, first), (b, second)]);
}

/// S1.4: connecting while the generation is pending holds the binding in
/// `local_storage`; the confirmation brings it back to `loading`, once.
#[test]
fn a_confirmed_generation_brings_local_storage_back_to_loading() {
    let mut member = loading();
    member.leave(Leave::Off, &mut Left::new(C::Off));
    assert_eq!(member.connect(false), None);
    let local = ConnectionPhase::Unavailable(UnavailableCause::LocalStorage);
    assert_eq!(binding(&mut member).connection().phase(), Some(local));
    assert!(member.confirm_generation(&OF, 2).is_some());
    let phase = binding(&mut member).connection().phase();
    assert_eq!(phase, Some(ConnectionPhase::Loading));
    assert_eq!(member.confirm_generation(&OF, 2), None, "once");
}

#[test]
#[should_panic]
fn a_bound_slot_needs_its_binding_id() {
    let profile = loading().profile;
    let _ = M::new("a", profile, None, false);
}

#[test]
#[should_panic]
fn a_placeholder_has_no_binding_id() {
    let placeholder = Slot::Placeholder(Placeholder::new(None));
    let _ = M::new("b", Profile::new(placeholder, true, None), Some(8), false);
}
