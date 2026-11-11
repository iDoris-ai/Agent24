#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

#[test]
fn a_change_older_than_one_already_applied_is_refused_and_changes_nothing() {
    let registry = KnowledgeHostRegistry::default();
    registry.apply("host", "b1", 1, Change::Activate).unwrap();
    // The deactivation (seq 3) overtook the activation it supersedes (seq 2).
    registry.apply("host", "b1", 3, Change::Deactivate).unwrap();
    assert_eq!(
        registry.apply("host", "b1", 2, Change::Activate),
        Err(ChangeError::StaleSequence)
    );
    assert!(!registry.is_active("host", "b1"));
    // The same number twice is stale as well: a retry takes a new one.
    assert_eq!(
        registry.apply("host", "b1", 3, Change::Deactivate),
        Err(ChangeError::StaleSequence)
    );
    assert_eq!(registry.record("host", "b1"), Some((false, 3)));
}

#[test]
fn concurrent_changes_end_on_the_highest_sequence_number() {
    // Check and store happen under one lock: whatever order the threads run
    // in, the record ends on seq 64 and its change, and every lower number
    // that arrived after a higher one was refused.
    let registry = std::sync::Arc::new(KnowledgeHostRegistry::default());
    let threads: Vec<_> = (1..=64u64)
        .rev()
        .map(|seq| {
            let registry = registry.clone();
            std::thread::spawn(move || {
                let change = if seq % 2 == 0 {
                    Change::Activate
                } else {
                    Change::Deactivate
                };
                registry.apply("host", "b1", seq, change).is_ok()
            })
        })
        .collect();
    let applied = threads
        .into_iter()
        .map(|t| t.join().unwrap())
        .filter(|ok| *ok)
        .count();
    assert!(applied >= 1);
    assert_eq!(registry.record("host", "b1"), Some((true, 64)));
}

#[test]
fn deactivation_keeps_the_high_water_mark() {
    let registry = KnowledgeHostRegistry::default();
    registry.apply("host", "b1", 1, Change::Activate).unwrap();
    assert!(registry.is_active("host", "b1"));
    registry.apply("host", "b1", 2, Change::Deactivate).unwrap();
    assert!(!registry.is_active("host", "b1"));
    registry.apply("host", "b1", 3, Change::Activate).unwrap();
    assert_eq!(registry.record("host", "b1"), Some((true, 3)));
}

#[test]
fn a_deactivation_may_come_first_and_still_blocks_an_older_activation() {
    let registry = KnowledgeHostRegistry::default();
    registry.apply("host", "b1", 5, Change::Deactivate).unwrap();
    assert_eq!(
        registry.apply("host", "b1", 4, Change::Activate),
        Err(ChangeError::StaleSequence)
    );
    assert!(!registry.is_active("host", "b1"));
}

#[test]
fn records_belong_to_the_host_capability_that_wrote_them() {
    let registry = KnowledgeHostRegistry::default();
    registry.apply("host-a", "b1", 7, Change::Activate).unwrap();
    assert!(!registry.is_active("host-b", "b1"));
    assert_eq!(registry.record("host-b", "b1"), None);
    // Another host starts from its own empty record for the same id.
    registry.apply("host-b", "b1", 1, Change::Activate).unwrap();
    registry
        .apply("host-b", "b1", 2, Change::Deactivate)
        .unwrap();
    assert!(registry.is_active("host-a", "b1"));
}

#[test]
fn a_host_holds_a_bounded_number_of_bindings() {
    let registry = KnowledgeHostRegistry::default();
    for i in 0..MAX_BINDINGS_PER_HOST {
        registry
            .apply("host", &format!("b{i}"), 1, Change::Deactivate)
            .unwrap();
    }
    assert_eq!(
        registry.apply("host", "one-more", 1, Change::Activate),
        Err(ChangeError::TooManyBindings)
    );
    assert_eq!(registry.record("host", "one-more"), None);
    // Existing records still change, and other hosts are unaffected.
    registry.apply("host", "b0", 2, Change::Activate).unwrap();
    registry
        .apply("other-host", "one-more", 1, Change::Activate)
        .unwrap();
}

fn bearer(value: &str) -> Credential {
    Credential::Bearer(value.to_owned())
}

#[test]
fn credential_follows_pushes_revokes_and_deactivation() {
    let registry = KnowledgeHostRegistry::default();
    let now = unix_now();
    let later = now + 600;
    // Only an active binding takes a credential; the refusal leaves no record.
    assert_eq!(
        registry.push_credential("host", "b1", 1, bearer("c1"), later),
        Err(ChangeError::BindingInactive)
    );
    assert_eq!(registry.record("host", "b1"), None);

    registry.apply("host", "b1", 1, Change::Activate).unwrap();
    registry
        .push_credential("host", "b1", 2, bearer("c2"), later)
        .unwrap();
    let current = registry.credential("host", "b1", now).unwrap();
    assert_eq!((current.credential, current.seq), (bearer("c2"), 2));

    // A revoke drops C at once and leaves the binding active.
    registry
        .apply("host", "b1", 3, Change::RevokeCredential)
        .unwrap();
    assert_eq!(registry.credential("host", "b1", now), None);
    assert!(registry.is_active("host", "b1"));

    registry
        .push_credential("host", "b1", 4, Credential::ApiKey("k4".into()), later)
        .unwrap();
    assert_eq!(
        registry.credential("host", "b1", now).unwrap().credential,
        Credential::ApiKey("k4".into())
    );
    // Deactivation drops C too, and reactivating does not bring it back.
    registry.apply("host", "b1", 5, Change::Deactivate).unwrap();
    assert_eq!(registry.credential("host", "b1", now), None);
    registry.apply("host", "b1", 6, Change::Activate).unwrap();
    assert_eq!(registry.credential("host", "b1", now), None);
}

#[test]
fn a_push_overtaken_by_a_revoke_cannot_bring_the_old_credential_back() {
    let registry = KnowledgeHostRegistry::default();
    let later = unix_now() + 600;
    registry.apply("host", "b1", 1, Change::Activate).unwrap();
    // The revoke (seq 3) arrives before the push it supersedes (seq 2).
    registry
        .apply("host", "b1", 3, Change::RevokeCredential)
        .unwrap();
    assert_eq!(
        registry.push_credential("host", "b1", 2, bearer("old"), later),
        Err(ChangeError::StaleSequence)
    );
    assert_eq!(registry.credential("host", "b1", unix_now()), None);
    // A revoke may also come before the binding was ever activated.
    registry
        .apply("host", "b2", 2, Change::RevokeCredential)
        .unwrap();
    assert_eq!(
        registry.apply("host", "b2", 1, Change::Activate),
        Err(ChangeError::StaleSequence)
    );
}

#[test]
fn a_credential_is_not_handed_out_once_it_has_expired() {
    let registry = KnowledgeHostRegistry::default();
    let expires_at = unix_now() + 10;
    registry.apply("host", "b1", 1, Change::Activate).unwrap();
    registry
        .push_credential("host", "b1", 2, bearer("c"), expires_at)
        .unwrap();
    assert!(registry.credential("host", "b1", expires_at - 1).is_some());
    assert_eq!(registry.credential("host", "b1", expires_at), None);
    // Dropped, not merely hidden: an earlier clock does not see it again.
    assert_eq!(registry.credential("host", "b1", expires_at - 1), None);
}

#[test]
fn a_reactivation_that_overtakes_a_removal_does_not_keep_the_credential() {
    let registry = KnowledgeHostRegistry::default();
    let later = unix_now() + 600;
    for (binding, removal) in [("b1", Change::Deactivate), ("b2", Change::RevokeCredential)] {
        registry
            .apply("host", binding, 1, Change::Activate)
            .unwrap();
        registry
            .push_credential("host", binding, 2, bearer("c2"), later)
            .unwrap();
        // The host removed C (3) and then activated again (4); 4 arrives first.
        registry
            .apply("host", binding, 4, Change::Activate)
            .unwrap();
        assert_eq!(
            registry.apply("host", binding, 3, removal),
            Err(ChangeError::StaleSequence)
        );
        assert_eq!(registry.credential("host", binding, unix_now()), None);
        assert_eq!(registry.record("host", binding), Some((true, 4)));
    }
}

#[test]
fn a_credential_belongs_to_the_host_capability_that_pushed_it() {
    let registry = KnowledgeHostRegistry::default();
    let later = unix_now() + 600;
    registry.apply("host-a", "b1", 1, Change::Activate).unwrap();
    registry
        .push_credential("host-a", "b1", 2, bearer("a"), later)
        .unwrap();
    assert_eq!(registry.credential("host-b", "b1", unix_now()), None);
    // Another host cannot push onto host-a's binding either.
    assert_eq!(
        registry.push_credential("host-b", "b1", 3, bearer("b"), later),
        Err(ChangeError::BindingInactive)
    );
}

#[test]
fn debug_output_never_carries_the_credential() {
    let current = CurrentCredential {
        credential: bearer("SECRET-BEARER"),
        seq: 1,
        expires_at: 0,
    };
    let printed = format!("{current:?} {:?}", Credential::ApiKey("SECRET-KEY".into()));
    assert!(!printed.contains("SECRET"), "{printed}");
}

fn active(registry: &KnowledgeHostRegistry, binding: &str) {
    registry
        .apply("host", binding, 1, Change::Activate)
        .unwrap();
}

#[test]
fn push_credential_rejects_an_empty_value_and_stores_nothing() {
    let registry = KnowledgeHostRegistry::default();
    // Before the binding is active, so a check that ran second would say
    // `BindingInactive` instead.
    assert_eq!(
        registry.push_credential("host", "b1", 1, bearer(""), unix_now() + 60),
        Err(ChangeError::Invalid("empty"))
    );
    assert_eq!(registry.record("host", "b1"), None);
}

#[test]
fn push_credential_rejects_a_value_that_contains_a_control_character() {
    let registry = KnowledgeHostRegistry::default();
    active(&registry, "b1");
    let err = registry
        .push_credential("host", "b1", 2, bearer("SECRET\n"), unix_now() + 60)
        .unwrap_err();
    assert_eq!(err, ChangeError::Invalid("not_graphic"));
    assert!(!format!("{err:?}").contains("SECRET"));
    assert_eq!(registry.record("host", "b1"), Some((true, 1)));
    assert_eq!(registry.credential("host", "b1", unix_now()), None);
}

#[test]
fn push_credential_rejects_a_value_past_the_byte_cap() {
    let registry = KnowledgeHostRegistry::default();
    active(&registry, "b1");
    let too_long = "x".repeat(MAX_CREDENTIAL_BYTES + 1);
    assert_eq!(
        registry.push_credential("host", "b1", 2, bearer(&too_long), unix_now() + 60),
        Err(ChangeError::Invalid("too_long"))
    );
    assert_eq!(registry.record("host", "b1"), Some((true, 1)));
}

#[test]
fn push_credential_rejects_a_credential_that_has_already_expired() {
    let registry = KnowledgeHostRegistry::default();
    active(&registry, "b1");
    assert_eq!(
        registry.push_credential("host", "b1", 2, Credential::ApiKey("k".into()), unix_now()),
        Err(ChangeError::Invalid("expired"))
    );
    assert_eq!(registry.record("host", "b1"), Some((true, 1)));
    assert_eq!(registry.credential("host", "b1", unix_now()), None);
}

#[test]
fn activate_rejects_a_base_url_the_transport_would_refuse() {
    let registry = KnowledgeHostRegistry::default();
    assert_eq!(
        registry.activate(
            "host",
            "b1",
            1,
            Endpoint {
                base_url: "http://kb.example/api/v1".into(),
                knowledge_base_ids: vec!["kb-a".into()],
            },
        ),
        Err(ChangeError::Invalid("base_url"))
    );
    assert_eq!(registry.record("host", "b1"), None);
}

#[test]
fn activate_rejects_a_knowledge_base_id_that_is_blank() {
    let registry = KnowledgeHostRegistry::default();
    assert_eq!(
        registry.activate(
            "host",
            "b1",
            1,
            Endpoint {
                base_url: "https://kb.example/api/v1".into(),
                knowledge_base_ids: vec![" ".into()],
            },
        ),
        Err(ChangeError::Invalid("knowledge_base_id"))
    );
    assert_eq!(registry.record("host", "b1"), None);
}

// ── run binding (PR-C) ────────────────────────────────────────────────────────

fn pin(host: &str, binding: &str, generation: u64) -> RunBinding {
    RunBinding {
        host: host.to_owned(),
        binding: binding.to_owned(),
        generation,
    }
}

#[test]
fn the_generation_moves_with_each_activation_only() {
    let registry = KnowledgeHostRegistry::default();
    let later = unix_now() + 600;
    assert_eq!(registry.generation("host", "b1"), None);
    registry.apply("host", "b1", 1, Change::Activate).unwrap();
    assert_eq!(registry.generation("host", "b1"), Some(1));
    // Rotating or revoking C is not a new generation.
    registry
        .push_credential("host", "b1", 2, bearer("c"), later)
        .unwrap();
    registry
        .apply("host", "b1", 3, Change::RevokeCredential)
        .unwrap();
    assert_eq!(registry.generation("host", "b1"), Some(1));
    registry.apply("host", "b1", 4, Change::Deactivate).unwrap();
    assert_eq!(registry.generation("host", "b1"), None);
    registry.apply("host", "b1", 5, Change::Activate).unwrap();
    assert_eq!(registry.generation("host", "b1"), Some(5));
    // Another host's records are its own.
    assert_eq!(registry.generation("other", "b1"), None);
}

#[test]
fn a_run_resolves_only_to_the_binding_host_and_generation_it_was_pinned_to() {
    let registry = KnowledgeHostRegistry::default();
    registry.apply("host", "b1", 1, Change::Activate).unwrap();
    registry.pin_run("run_1", pin("host", "b1", 1));

    assert_eq!(
        registry.resolve_run("run_1", Some("b1")),
        Ok(pin("host", "b1", 1))
    );
    // No selector: unbound, whatever is pinned.
    assert_eq!(
        registry.resolve_run("run_1", None),
        Err(KnowledgeRefusal::Unbound)
    );
    // A selector without a pin (say, after a restart) is never bound anew.
    assert_eq!(
        registry.resolve_run("run_2", Some("b1")),
        Err(KnowledgeRefusal::Stale)
    );
    // A selector that is not the pinned binding.
    assert_eq!(
        registry.resolve_run("run_1", Some("b2")),
        Err(KnowledgeRefusal::Stale)
    );

    // Deactivated, then activated again: a new generation, so the run is stale.
    registry.apply("host", "b1", 2, Change::Deactivate).unwrap();
    assert_eq!(
        registry.resolve_run("run_1", Some("b1")),
        Err(KnowledgeRefusal::Stale)
    );
    registry.apply("host", "b1", 3, Change::Activate).unwrap();
    assert_eq!(
        registry.resolve_run("run_1", Some("b1")),
        Err(KnowledgeRefusal::Stale)
    );

    // The same binding and generation under another host does not count.
    registry.apply("other", "b1", 1, Change::Activate).unwrap();
    registry.pin_run("run_3", pin("host", "b1", 1));
    assert_eq!(
        registry.resolve_run("run_3", Some("b1")),
        Err(KnowledgeRefusal::Stale)
    );

    assert_eq!(KnowledgeRefusal::Unbound.code(), "knowledge_unbound");
    assert_eq!(KnowledgeRefusal::Stale.code(), "knowledge_binding_stale");
}

#[test]
fn the_oldest_run_pin_goes_first_once_the_cap_is_reached() {
    let registry = KnowledgeHostRegistry::default();
    registry.apply("host", "b1", 1, Change::Activate).unwrap();
    for i in 0..=MAX_PINNED_RUNS {
        registry.pin_run(&format!("run_{i}"), pin("host", "b1", 1));
    }
    assert_eq!(
        registry.resolve_run("run_0", Some("b1")),
        Err(KnowledgeRefusal::Stale)
    );
    assert!(registry.resolve_run("run_1", Some("b1")).is_ok());
    let last = format!("run_{MAX_PINNED_RUNS}");
    assert!(registry.resolve_run(&last, Some("b1")).is_ok());
    // Pinning a run again does not take a second place in line.
    registry.pin_run("run_1", pin("host", "b1", 1));
    assert_eq!(registry.runs.lock().unwrap().order.len(), MAX_PINNED_RUNS);
    registry.pin_run("run_new", pin("host", "b1", 1));
    assert_eq!(
        registry.resolve_run("run_1", Some("b1")),
        Err(KnowledgeRefusal::Stale)
    );
    assert!(registry.resolve_run("run_2", Some("b1")).is_ok());
}

#[test]
fn a_session_pin_is_exact() {
    let registry = KnowledgeHostRegistry::default();
    assert!(registry.pin_session("s1", &pin("host", "b1", 1), false));
    assert!(registry.pin_session("s1", &pin("host", "b1", 1), true));
    assert!(!registry.pin_session("s1", &pin("host", "b1", 2), false));
    assert!(!registry.pin_session("s1", &pin("host", "b2", 1), false));
    assert!(!registry.pin_session("s1", &pin("other", "b1", 1), false));
    assert!(registry.pin_session("s2", &pin("host", "b1", 2), false));
}

#[test]
fn concurrent_first_runs_of_a_session_agree_on_one_activation() {
    let registry = std::sync::Arc::new(KnowledgeHostRegistry::default());
    let start = std::sync::Arc::new(std::sync::Barrier::new(16));
    let calls: Vec<_> = (0..16u64)
        .map(|n| {
            let (registry, start) = (registry.clone(), start.clone());
            std::thread::spawn(move || {
                let binding = pin("host", "b1", 1 + n % 2);
                start.wait();
                (
                    binding.generation,
                    registry.pin_session("s1", &binding, false),
                )
            })
        })
        .collect();
    let results: Vec<(u64, bool)> = calls.into_iter().map(|c| c.join().unwrap()).collect();
    let won: Vec<u64> = results
        .iter()
        .filter(|(_, ok)| *ok)
        .map(|(g, _)| *g)
        .collect();
    let first = won[0];
    assert!(
        results.iter().all(|(g, ok)| *ok == (*g == first)),
        "{results:?}"
    );
}

#[test]
fn no_new_pin_for_a_session_with_knowledge_results_and_none_past_the_cap() {
    let registry = KnowledgeHostRegistry::default();
    assert!(!registry.pin_session("s1", &pin("host", "b1", 1), true));
    assert!(!registry.pin_session("s1", &pin("host", "b1", 1), true));

    // Never dropped: past the cap a new session is refused, a pinned one
    // keeps its pin.
    assert!(registry.pin_session("s2", &pin("host", "b1", 1), false));
    for n in 1..MAX_PINNED_SESSIONS {
        assert!(registry.pin_session(&format!("other-{n}"), &pin("host", "b1", 1), false));
    }
    assert!(!registry.pin_session("late", &pin("host", "b1", 1), false));
    assert!(registry.pin_session("s2", &pin("host", "b1", 1), true));
    assert!(!registry.pin_session("s2", &pin("host", "b1", 2), false));
}

fn activate_with_credential(
    registry: &KnowledgeHostRegistry,
    binding: &str,
    url: &str,
    activate_seq: u64,
    credential_seq: u64,
    secret: &str,
    expires: UnixSeconds,
) {
    registry
        .activate(
            "host",
            binding,
            activate_seq,
            Endpoint {
                base_url: url.to_owned(),
                knowledge_base_ids: vec!["kb".into()],
            },
        )
        .unwrap();
    registry
        .push_credential(
            "host",
            binding,
            credential_seq,
            Credential::Bearer(secret.to_owned()),
            expires,
        )
        .unwrap();
}

/// Same base URL, two live credentials. The greatest seq is the one probed.
/// Repeating with fresh maps catches a choice that follows `HashMap` order.
#[test]
fn the_same_base_url_keeps_the_highest_credential_seq() {
    let now = unix_now();
    let later = now + 600;
    let url = "http://127.0.0.1:9/api/v1";
    for n in 0..16 {
        let registry = KnowledgeHostRegistry::default();
        let low_name = format!("low{n}");
        let high_name = format!("high{n}");
        let (low_binding, high_binding) = if n % 2 == 0 {
            (&low_name, &high_name)
        } else {
            (&high_name, &low_name)
        };
        activate_with_credential(&registry, low_binding, url, 1, 2, "low-secret", later);
        activate_with_credential(&registry, high_binding, url, 3, 9, "high-secret", later);
        let targets = registry.probe_targets(now);
        assert_eq!(targets.len(), 1, "round {n}");
        assert_eq!(targets[0].credential_epoch, 9, "round {n}");
        assert_eq!(
            targets[0].credential.as_ref(),
            Some(&crate::Credential::Bearer("high-secret".into())),
            "round {n}"
        );
    }

    // Equal seq: greatest (host, binding), not whichever record was visited first.
    for n in 0..16 {
        let registry = KnowledgeHostRegistry::default();
        let left = format!("a{n}");
        let right = format!("b{n}");
        activate_with_credential(&registry, &left, url, 1, 4, "left-secret", later);
        activate_with_credential(&registry, &right, url, 2, 4, "right-secret", later);
        let targets = registry.probe_targets(now);
        assert_eq!(targets.len(), 1, "tie {n}");
        assert_eq!(targets[0].credential_epoch, 4, "tie {n}");
        assert_eq!(
            targets[0].credential.as_ref(),
            Some(&crate::Credential::Bearer("right-secret".into())),
            "tie {n}"
        );
    }

    // A live credential still beats the same URL with none.
    let registry = KnowledgeHostRegistry::default();
    registry
        .activate(
            "host",
            "bare",
            1,
            Endpoint {
                base_url: url.to_owned(),
                knowledge_base_ids: vec!["kb".into()],
            },
        )
        .unwrap();
    activate_with_credential(&registry, "with-c", url, 1, 2, "only-secret", later);
    let targets = registry.probe_targets(now);
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].credential_epoch, 2);
}

#[test]
fn a_poisoned_entries_lock_still_lists_probe_targets() {
    let registry = KnowledgeHostRegistry::default();
    registry
        .activate(
            "host",
            "b1",
            1,
            Endpoint {
                base_url: "http://127.0.0.1:9/api/v1".to_owned(),
                knowledge_base_ids: vec!["kb".into()],
            },
        )
        .unwrap();
    let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = registry.entries.lock().unwrap();
        panic!("poison entries");
    }));
    assert!(poisoned.is_err());
    let targets = registry.probe_targets(0);
    assert_eq!(
        targets.len(),
        1,
        "a poisoned entries lock must not look unconfigured"
    );
    assert_eq!(targets[0].base_url, "http://127.0.0.1:9/api/v1");
}
