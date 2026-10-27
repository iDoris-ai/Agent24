#![allow(clippy::unwrap_used)]

use super::*;
use crate::provider::binding::BindingState;
use crate::provider::connection::Connection;
use crate::provider::credential::Credentials;
use crate::provider::evidence::{Kbs, Rule};
use crate::provider::gate::Gate;
use crate::provider::placeholder::Placeholder;
use crate::provider::profile::{Confirmation, Slot};
use crate::provider::replay::Entry;
use crate::provider::state::{ConnectionPhase, Switches};

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

type H = Host<T>;
type C = Change<T>;

fn of(profile: &'static str, binding: u32) -> Of<T> {
    Of { profile, binding }
}

/// A selected profile whose binding (generation 1) is `loading`.
fn loading() -> Profile<&'static str> {
    let mut binding = BindingState::new(
        Connection::new(Gate::new(1), true),
        Credentials::new(false),
        true,
    );
    let clear = Switches {
        off: false,
        profile_disabled: false,
    };
    assert!(binding.connect(clear).is_some());
    Profile::new(Slot::Bound(Box::new(binding)), true, None)
}

/// "a" bound to binding 7 and loading, "b" a selected placeholder; Off is
/// off and the seal present. The log's next record is 10.
fn host() -> H {
    let mut host = H::new(false, true, 10);
    host.insert(Member::new("a", loading(), Some(7), false));
    let placeholder = Slot::Placeholder(Placeholder::new(None));
    let b = Profile::new(placeholder, true, None);
    host.insert(Member::new("b", b, None, false));
    host
}

fn phase(host: &H, id: &'static str) -> Option<ConnectionPhase> {
    match host.profile(&id).unwrap().slot() {
        Slot::Bound(binding) => binding.connection().phase(),
        Slot::Placeholder(_) => panic!("{id} is a placeholder"),
    }
}

fn write(host: &mut H) -> (Attempt, u64, Vec<C>) {
    match host.take_write() {
        Some((
            attempt,
            Record {
                seq,
                entry: Entry::Changes(changes),
            },
        )) => (attempt, seq, changes),
        other => panic!("{other:?}"),
    }
}

fn durable(host: &mut H) -> Output<T> {
    let (attempt, _, _) = write(host);
    host.written(attempt, Written::Durable)
}

/// I2, S3.3: Off takes the binding out of the connection phase at once and
/// deletes the seal; the generation it invalidated to is written with it and
/// takes effect once confirmed.
#[test]
fn off_takes_effect_at_once_and_is_written_with_its_generation() {
    let mut host = host();
    let out = host.turn_off();
    assert!(host.is_off());
    assert_eq!(phase(&host, "a"), None);
    let Some(SealWrite::Delete(delete)) = out.seal else {
        panic!("{out:?}");
    };
    let (attempt, seq, changes) = write(&mut host);
    assert_eq!(seq, 10);
    assert_eq!(
        changes,
        [
            C::Off,
            C::Generation {
                of: of("a", 7),
                value: 2
            }
        ]
    );
    let generation = |host: &H| match host.profile(&"a").unwrap().slot() {
        Slot::Bound(binding) => binding.connection().gate().generation(),
        Slot::Placeholder(_) => unreachable!(),
    };
    assert_eq!(generation(&host), 1, "pending until confirmed");
    // Off already: nothing more to write.
    assert_eq!(host.turn_off(), Output::default());
    // A failed deletion is tried again; the seal stays down until the
    // record is in, then it is rebuilt.
    let again = host.sealed(SealWrite::Delete(delete), Written::Failed).seal;
    let Some(SealWrite::Delete(again)) = again else {
        panic!("{again:?}");
    };
    assert_ne!(again, delete);
    assert_eq!(
        host.sealed(SealWrite::Delete(again), Written::Durable).seal,
        None
    );
    let out = host.written(attempt, Written::Durable);
    assert_eq!(generation(&host), 2);
    assert!(matches!(out.seal, Some(SealWrite::Rebuild(_))), "{out:?}");
    assert!(out.loading.is_empty(), "Off still holds it");
    assert!(host.take_write().is_none());
}

/// I1: On is written first and takes effect once confirmed; the binding
/// then connects again.
#[test]
fn on_takes_effect_once_its_record_is_confirmed() {
    let mut host = host();
    host.turn_off();
    durable(&mut host);
    host.turn_on().unwrap();
    host.turn_on().unwrap();
    let (on, seq, changes) = write(&mut host);
    assert_eq!((seq, changes), (11, vec![C::On]));
    assert!(host.is_off(), "not before it is confirmed");
    let out = host.written(on, Written::Durable);
    assert!(!host.is_off());
    assert_eq!(out.loading.len(), 1);
    assert_eq!(out.loading[0].0, "a");
    assert_eq!(phase(&host, "a"), Some(ConnectionPhase::Loading));
    assert!(host.take_write().is_none(), "On was written once");
    // In effect now: Off is an ordinary tightening again.
    host.turn_off();
    assert!(host.is_off());
    assert_eq!(phase(&host, "a"), None);
}

/// I1's blocking scope: an unconfirmed tightening refuses a loosening of the
/// same profile, and a host-level one every loosening; Off answers first.
#[test]
fn an_unconfirmed_tightening_refuses_the_loosenings_it_covers() {
    let mut host = host();
    host.disable("a");
    host.disable("b");
    assert_eq!(host.enable("a"), Err(Refused::LocalStorage));
    assert_eq!(host.enable("b"), Err(Refused::LocalStorage));
    durable(&mut host);
    host.enable("a").unwrap();
    assert_eq!(
        host.enable("b"),
        Err(Refused::LocalStorage),
        "b still waits"
    );

    let mut host = H::new(true, true, 1);
    host.insert(Member::new("a", loading(), Some(7), true));
    host.disable("x");
    host.turn_off();
    assert!(host.take_write().is_none(), "unknown profile; Off already");
    host.turn_on().unwrap();
    assert_eq!(write(&mut host).2, [C::On], "nothing blocks it");
    let mut host = host_off();
    host.disable("a");
    assert_eq!(host.turn_on(), Err(Refused::KnowledgeOff));
    assert_eq!(host.enable("a"), Err(Refused::KnowledgeOff));
}

fn host_off() -> H {
    let mut host = host();
    host.turn_off();
    durable(&mut host);
    host
}

/// I1: the opposite action while a loosening is pending gives it up, and its
/// offset is written after it; a later confirmation of it changes nothing.
#[test]
fn the_opposite_action_gives_a_pending_loosening_up() {
    let mut host = host();
    host.disable("a");
    durable(&mut host);
    host.enable("a").unwrap();
    let (enable, _, _) = write(&mut host);
    host.disable("a");
    assert_eq!(host.written(enable, Written::Durable), Output::default());
    assert_eq!(phase(&host, "a"), None, "still disabled");
    let (_, _, changes) = write(&mut host);
    assert_eq!(changes, [C::Disable("a")]);

    let mut host = host_off();
    host.turn_on().unwrap();
    host.turn_off();
    let (on, _, _) = write(&mut host);
    host.written(on, Written::Durable);
    assert!(host.is_off());
    assert_eq!(write(&mut host).2, [C::Off]);
}

/// S5.6: disabling a bound profile advances the evidence epoch with a
/// placeholder rule over every KB, written with it; enabling it again
/// connects the same binding once confirmed (S3.3).
#[test]
fn disable_places_a_rule_and_enable_connects_the_same_binding() {
    let mut host = host();
    host.disable("a");
    let (disable, _, changes) = write(&mut host);
    let rule = Rule {
        epoch: 1,
        kbs: Kbs::All,
    };
    assert_eq!(
        changes,
        [
            C::Disable("a"),
            C::Placed {
                of: of("a", 7),
                rule
            },
            C::Generation {
                of: of("a", 7),
                value: 2
            },
        ]
    );
    // Confirming the generation alone does not reconnect: disabled.
    assert!(host.written(disable, Written::Durable).loading.is_empty());
    host.enable("a").unwrap();
    let out = durable(&mut host);
    assert_eq!(out.loading.len(), 1);
    assert_eq!(phase(&host, "a"), Some(ConnectionPhase::Loading));
}

/// I2: a failed write is still unconfirmed and shows "not saved" until its
/// retry lands; a late report of the failed attempt changes nothing.
#[test]
fn a_failed_write_is_taken_again_under_its_number() {
    let mut host = host();
    host.disable("a");
    let (first, ..) = write(&mut host);
    assert!(host.written(first, Written::Failed).seal.is_none());
    assert!(host.unsaved());
    let (again, ..) = write(&mut host);
    assert_eq!(host.written(first, Written::Durable), Output::default());
    assert!(host.unsaved(), "a late report of the failed attempt");
    host.written(again, Written::Durable);
    assert!(!host.unsaved());
}

/// A switch already where the user puts it writes nothing, and a loosening
/// already pending is not written twice.
#[test]
fn a_switch_already_in_place_writes_nothing() {
    let mut host = host();
    host.turn_on().unwrap();
    host.enable("a").unwrap();
    assert!(host.take_write().is_none());
    host.disable("a");
    host.disable("a");
    host.disable("b");
    host.disable("b");
    durable(&mut host);
    durable(&mut host);
    assert!(host.take_write().is_none(), "one write each");
    host.enable("a").unwrap();
    host.enable("a").unwrap();
    host.enable("b").unwrap();
    host.enable("b").unwrap();
    assert_eq!(durable(&mut host).loading.len(), 1, "a connects again");
    durable(&mut host);
    assert!(host.take_write().is_none(), "one write each");
}

/// S4.5: the calls in flight on every binding Off takes out are aborted,
/// each with its binding.
#[test]
fn off_aborts_the_calls_in_flight() {
    let mut profile = loading();
    let Slot::Bound(binding) = profile.slot_mut() else {
        unreachable!()
    };
    let (call, _) = binding.connection_mut().open_call().unwrap();
    let mut host = H::new(false, true, 1);
    host.insert(Member::new("a", profile, Some(7), false));
    assert_eq!(host.turn_off().aborted, [(of("a", 7), call)]);
}

/// The tightening `first` is written on its own, no loosening slipped in;
/// it lands.
fn tightening_alone(host: &mut H, first: C) {
    let (attempt, _, changes) = write(host);
    assert_eq!(changes[0], first);
    assert!(!changes.iter().any(|c| matches!(c, C::On | C::Enable(_))));
    host.written(attempt, Written::Durable);
}

/// I1: a refused loosening is dropped: nothing is written or remembered, and
/// asking again once the tightening is in goes through.
#[test]
fn a_refused_loosening_is_dropped() {
    let mut host = host();
    host.disable("a");
    assert_eq!(host.enable("a"), Err(Refused::LocalStorage));
    tightening_alone(&mut host, C::Disable("a"));
    assert!(host.take_write().is_none(), "nothing queued");
    assert_eq!(phase(&host, "a"), None, "still disabled");
    host.enable("a").unwrap();
    assert_eq!(write(&mut host).2, [C::Enable("a")]);

    let mut off = host_off();
    off.disable("a");
    assert_eq!(off.turn_on(), Err(Refused::KnowledgeOff));
    tightening_alone(&mut off, C::Disable("a"));
    assert!(off.take_write().is_none(), "nothing queued");
    assert!(off.is_off());
    off.turn_on().unwrap();
    assert_eq!(write(&mut off).2, [C::On]);
}

/// S1.2: with both switches on, lifting either one leaves the binding out
/// of the connection phase; it connects once neither holds it.
#[test]
fn a_binding_connects_only_once_neither_switch_holds_it() {
    let mut off = host_off();
    off.disable("a");
    durable(&mut off);
    off.enable("a").unwrap();
    assert!(durable(&mut off).loading.is_empty(), "Off still holds it");
    assert_eq!(phase(&off, "a"), None);
    off.turn_on().unwrap();
    assert_eq!(durable(&mut off).loading.len(), 1);

    let mut host = host();
    host.disable("a");
    durable(&mut host);
    host.turn_off();
    durable(&mut host);
    host.turn_on().unwrap();
    assert!(durable(&mut host).loading.is_empty(), "disabled still");
    assert_eq!(phase(&host, "a"), None);
    host.enable("a").unwrap();
    assert_eq!(durable(&mut host).loading.len(), 1);
}

/// S5.6: unselecting a bound profile advances the evidence epoch with a
/// placeholder rule over every KB; selecting it again activates the same
/// binding once confirmed (S3.3), Off permitting.
#[test]
fn unselect_places_a_rule_and_select_activates_the_same_binding() {
    let mut host = host();
    host.unselect("a");
    assert!(!host.profile(&"a").unwrap().selected());
    let (unselect, _, changes) = write(&mut host);
    let rule = Rule {
        epoch: 1,
        kbs: Kbs::All,
    };
    let generation = C::Generation {
        of: of("a", 7),
        value: 2,
    };
    let placed = C::Placed {
        of: of("a", 7),
        rule,
    };
    assert_eq!(changes, [C::Unselect("a"), placed, generation]);
    assert!(host.written(unselect, Written::Durable).loading.is_empty());
    host.select("a").unwrap();
    assert!(
        !host.profile(&"a").unwrap().selected(),
        "not before confirmed"
    );
    assert_eq!(durable(&mut host).loading.len(), 1);
    assert!(host.profile(&"a").unwrap().selected());

    let mut off = host_off();
    off.unselect("a");
    durable(&mut off);
    off.select("a").unwrap();
    assert!(durable(&mut off).loading.is_empty(), "Off still holds it");
    assert!(off.profile(&"a").unwrap().selected());
}

/// I1 for selection: refused while its profile's tightening is unconfirmed;
/// the opposite action gives a pending one up, and a selection already in
/// place writes nothing.
#[test]
fn selection_follows_the_same_rules_as_the_other_switches() {
    let mut host = host();
    host.unselect("b");
    host.unselect("b");
    assert_eq!(host.select("b"), Err(Refused::LocalStorage));
    tightening_alone(&mut host, C::Unselect("b"));
    assert!(host.take_write().is_none(), "one write");
    host.select("a").unwrap();
    assert!(host.take_write().is_none(), "selected already");

    host.select("b").unwrap();
    host.select("b").unwrap();
    let (select, _, changes) = write(&mut host);
    assert_eq!(changes, [C::Select("b")]);
    host.unselect("b");
    assert_eq!(host.written(select, Written::Durable), Output::default());
    assert!(!host.profile(&"b").unwrap().selected());
    assert_eq!(write(&mut host).2, [C::Unselect("b")]);
}

/// I1 blocks by profile: another profile's unconfirmed tightening does not
/// refuse a selection.
#[test]
fn a_selection_is_blocked_by_its_own_profile_only() {
    let mut host = host();
    host.unselect("a");
    durable(&mut host);
    host.unselect("b");
    assert_eq!(host.select("a"), Ok(()));
    tightening_alone(&mut host, C::Unselect("b"));
    assert_eq!(write(&mut host).2, [C::Select("a")]);
}

/// S1.13: with a confirmation awaited, selecting only sets the flag; the
/// binding stays deactivated until the confirmation.
#[test]
fn a_selection_awaiting_confirmation_does_not_activate() {
    let binding = BindingState::new(
        Connection::new(Gate::new(1), true),
        Credentials::new(false),
        false,
    );
    let awaited = Some(Confirmation::PrincipalChanged);
    let profile = Profile::new(Slot::Bound(Box::new(binding)), false, awaited);
    let mut host = H::new(false, true, 1);
    host.insert(Member::new("a", profile, Some(7), false));
    host.select("a").unwrap();
    assert!(durable(&mut host).loading.is_empty());
    let profile = host.profile(&"a").unwrap();
    assert!(profile.selected());
    assert_eq!(profile.confirmation(), awaited);
    assert_eq!(phase(&host, "a"), None);
}

/// A placeholder's selection is remembered once confirmed; nothing connects.
#[test]
fn a_placeholder_selection_is_remembered() {
    let mut host = host();
    host.unselect("b");
    tightening_alone(&mut host, C::Unselect("b"));
    host.select("b").unwrap();
    assert!(!host.profile(&"b").unwrap().selected());
    assert!(durable(&mut host).loading.is_empty());
    let b = host.profile(&"b").unwrap();
    assert!(b.selected());
    assert!(matches!(b.slot(), Slot::Placeholder(_)));
}
