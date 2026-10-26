#![allow(clippy::unwrap_used)]

use super::*;
use crate::provider::binding::BindingState;
use crate::provider::connection::Connection;
use crate::provider::credential::Credentials;
use crate::provider::evidence::{Kbs, Rule};
use crate::provider::gate::Gate;
use crate::provider::placeholder::Placeholder;
use crate::provider::state::ConnectionPhase;

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

/// "a", selected, bound to binding 7 at generation 1 and `loading`.
fn loading() -> M {
    let binding = BindingState::new(
        Connection::new(Gate::new(1), true),
        Credentials::new(false),
        true,
    );
    let profile = Profile::new(Slot::Bound(Box::new(binding)), true, None);
    let mut member = M::new("a", profile, Some(7), false);
    assert!(member.connect(false).is_some());
    member
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
    let (call, _) = binding(&mut member).connection_mut().open_call().unwrap();
    let mut left = Left::new(C::Off);
    member.leave(Leave::Off, &mut left);
    assert_eq!(left.changes, [C::Off, C::Generation { of: OF, value: 2 }]);
    assert_eq!(left.generations, [(OF, 2)]);
    assert_eq!(left.aborted, [call]);
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
/// binding it was written for.
#[test]
fn a_generation_takes_effect_for_its_own_binding_only() {
    let mut member = loading();
    member.leave(Leave::Off, &mut Left::new(C::Off));
    for other in [Of { binding: 8, ..OF }, Of { profile: "b", ..OF }] {
        member.confirm_generation(&other, 2);
        assert_eq!(generation(&mut member), 1, "{other:?}");
    }
    member.confirm_generation(&OF, 2);
    assert_eq!(generation(&mut member), 2);
}
