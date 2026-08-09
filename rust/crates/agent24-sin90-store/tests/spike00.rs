//! SPIKE-00 gate tests: CAS idempotency, pure-replay attention, and the
//! `incremental == full rebuild` invariant (SIN90-domain.md §7).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use agent24_sin90::{
    ProposalSource, ProposalStatus, ScheduleBlockStatus, Sin90Op, Sin90Proposal, TaskStatus,
};
use agent24_sin90_store::Sin90Store;

fn proposal(id: &str, ops: Vec<Sin90Op>) -> Sin90Proposal {
    Sin90Proposal {
        id: id.to_string(),
        status: ProposalStatus::Pending,
        source: ProposalSource::LocalBrain,
        ops,
        rationale: None,
    }
}

use agent24_sin90_store::test_hooks;

#[tokio::test]
async fn apply_proposal_is_cas_idempotent() {
    let store = Sin90Store::open_memory().await.unwrap();
    let p = proposal(
        "p-idem",
        vec![Sin90Op::CreateDirection {
            title: "iDoris site".into(),
            target_window: "2026-08".into(),
        }],
    );
    store.submit_proposal(&p).await.unwrap();

    let first = store.apply_proposal("p-idem").await.unwrap();
    // Re-applying the same proposal returns the SAME receipt and creates nothing new.
    let second = store.apply_proposal("p-idem").await.unwrap();
    assert_eq!(first, second, "re-apply must be idempotent");
    assert_eq!(
        test_hooks::direction_count(&store).await.unwrap(),
        1,
        "exactly one direction despite two applies"
    );
}

#[tokio::test]
async fn illegal_op_rolls_back_and_leaves_proposal_pending() {
    let store = Sin90Store::open_memory().await.unwrap();
    // Create a planned task via a proposal, then propose an illegal jump.
    let wk = store.create_week("2026-W33").await.unwrap();
    let seed = proposal(
        "p-seed",
        vec![Sin90Op::CreateTasks {
            week_id: wk.id.clone(),
            tasks: vec![agent24_sin90::NewTask {
                title: "t".into(),
                direction_id: None,
            }],
        }],
    );
    store.submit_proposal(&seed).await.unwrap();
    store.apply_proposal("p-seed").await.unwrap();

    let task_id = test_hooks::first_task_id(&store).await.unwrap();
    // planned -> done is illegal (must pass through in_progress).
    let bad = proposal(
        "p-bad",
        vec![Sin90Op::TransitionTask {
            task_id,
            to: TaskStatus::Done,
        }],
    );
    store.submit_proposal(&bad).await.unwrap();
    let err = store.apply_proposal("p-bad").await;
    assert!(err.is_err(), "illegal transition must fail");

    // Rolled back: proposal is back to pending, retryable.
    let status = test_hooks::proposal_status(&store, "p-bad").await.unwrap();
    assert_eq!(status.as_deref(), Some("pending"));
}

#[tokio::test]
async fn attention_replay_is_pure_snapshot() {
    let store = Sin90Store::open_memory().await.unwrap();
    let d = store.create_direction("Coding", "2026-08").await.unwrap();
    let b = store.create_block(Some(&d.id), None, 120).await.unwrap();
    store
        .transition_block(&b.id, ScheduleBlockStatus::Started)
        .await
        .unwrap();
    let done = store
        .transition_block(&b.id, ScheduleBlockStatus::Completed)
        .await
        .unwrap();
    let occurred = done.updated_at.clone();
    let (start, end) = window_around(&occurred);

    let before = store.attention(&start, &end).await.unwrap();
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].actual_min, 120);
    assert_eq!(before[0].direction_title.as_deref(), Some("Coding"));

    // Rename the direction AFTER the fact — replay must be unchanged.
    test_hooks::rename_direction(&store, &d.id, "RENAMED")
        .await
        .unwrap();
    let after = store.attention(&start, &end).await.unwrap();
    assert_eq!(after, before, "editing the title must not rewrite history");
    assert_eq!(after[0].direction_title.as_deref(), Some("Coding"));
}

#[tokio::test]
async fn incremental_equals_full_rebuild() {
    let store = Sin90Store::open_memory().await.unwrap();
    let d1 = store.create_direction("Coding", "2026-08").await.unwrap();
    let d2 = store.create_direction("Business", "2026-08").await.unwrap();

    // Complete several blocks interleaved, folding incrementally after each.
    for (dir, mins) in [(&d1, 90u32), (&d2, 30), (&d1, 120), (&d1, 60), (&d2, 45)] {
        let b = store.create_block(Some(&dir.id), None, mins).await.unwrap();
        store
            .transition_block(&b.id, ScheduleBlockStatus::Started)
            .await
            .unwrap();
        store
            .transition_block(&b.id, ScheduleBlockStatus::Completed)
            .await
            .unwrap();
        store.attention_apply_new_events().await.unwrap();
    }

    let incremental = store.attention_daily().await.unwrap();
    store.attention_rebuild().await.unwrap();
    let rebuilt = store.attention_daily().await.unwrap();
    assert_eq!(
        incremental, rebuilt,
        "incremental fold must equal a full rebuild"
    );
    // Sanity: d1 = 90+120+60 = 270, d2 = 30+45 = 75.
    let total: i64 = rebuilt.iter().map(|r| r.actual_min).sum();
    assert_eq!(total, 345);
}

#[tokio::test]
async fn carry_over_twice_is_rejected() {
    let store = Sin90Store::open_memory().await.unwrap();
    let wk = store.create_week("2026-W33").await.unwrap();
    let next = store.create_week("2026-W34").await.unwrap();
    let seed = proposal(
        "p-seed",
        vec![Sin90Op::CreateTasks {
            week_id: wk.id.clone(),
            tasks: vec![agent24_sin90::NewTask {
                title: "carry me".into(),
                direction_id: None,
            }],
        }],
    );
    store.submit_proposal(&seed).await.unwrap();
    store.apply_proposal("p-seed").await.unwrap();
    let task_id = test_hooks::first_task_id(&store).await.unwrap();

    let carry = proposal(
        "p-carry",
        vec![Sin90Op::CarryOverTask {
            task_id: task_id.clone(),
            to_week: next.id.clone(),
        }],
    );
    store.submit_proposal(&carry).await.unwrap();
    store.apply_proposal("p-carry").await.unwrap();

    // Second carry-over of the now carried_over task is an illegal transition.
    let again = proposal(
        "p-carry2",
        vec![Sin90Op::CarryOverTask {
            task_id,
            to_week: next.id.clone(),
        }],
    );
    store.submit_proposal(&again).await.unwrap();
    assert!(store.apply_proposal("p-carry2").await.is_err());
}

#[tokio::test]
async fn transition_in_closed_week_is_rejected() {
    // Relational invariant enforced store-side: a task in a closed week is
    // immutable, even though the pure validator (per-entity) can't see it.
    let store = Sin90Store::open_memory().await.unwrap();
    let wk = store.create_week("2026-W33").await.unwrap();
    let seed = proposal(
        "p-seed",
        vec![Sin90Op::CreateTasks {
            week_id: wk.id.clone(),
            tasks: vec![agent24_sin90::NewTask {
                title: "t".into(),
                direction_id: None,
            }],
        }],
    );
    store.submit_proposal(&seed).await.unwrap();
    store.apply_proposal("p-seed").await.unwrap();
    let task_id = test_hooks::first_task_id(&store).await.unwrap();

    test_hooks::close_week(&store, &wk.id).await.unwrap();

    let mv = proposal(
        "p-move",
        vec![Sin90Op::TransitionTask {
            task_id,
            to: TaskStatus::InProgress,
        }],
    );
    store.submit_proposal(&mv).await.unwrap();
    assert!(
        store.apply_proposal("p-move").await.is_err(),
        "mutating a task in a closed week must be rejected"
    );
    // Rolled back → still retryable.
    assert_eq!(
        test_hooks::proposal_status(&store, "p-move")
            .await
            .unwrap()
            .as_deref(),
        Some("pending")
    );
}

#[tokio::test]
async fn carry_over_into_same_week_is_rejected() {
    // Carrying a task into its OWN week would strand it (carried_over) and spawn
    // an endlessly re-carryable duplicate in that same week.
    let store = Sin90Store::open_memory().await.unwrap();
    let wk = store.create_week("2026-W33").await.unwrap();
    let seed = proposal(
        "p-seed",
        vec![Sin90Op::CreateTasks {
            week_id: wk.id.clone(),
            tasks: vec![agent24_sin90::NewTask {
                title: "t".into(),
                direction_id: None,
            }],
        }],
    );
    store.submit_proposal(&seed).await.unwrap();
    store.apply_proposal("p-seed").await.unwrap();
    let task_id = test_hooks::first_task_id(&store).await.unwrap();

    let same = proposal(
        "p-same",
        vec![Sin90Op::CarryOverTask {
            task_id,
            to_week: wk.id.clone(), // same week!
        }],
    );
    store.submit_proposal(&same).await.unwrap();
    assert!(store.apply_proposal("p-same").await.is_err());
    // Rolled back — the source task is untouched, no duplicate created.
    assert_eq!(test_hooks::direction_count(&store).await.unwrap(), 0);
}

#[tokio::test]
async fn reorder_with_foreign_task_id_rejected() {
    // An id not in the week must fail the whole apply — the reordered event may
    // not assert an order the table never adopted.
    let store = Sin90Store::open_memory().await.unwrap();
    let wk = store.create_week("2026-W33").await.unwrap();
    let seed = proposal(
        "p-seed",
        vec![Sin90Op::CreateTasks {
            week_id: wk.id.clone(),
            tasks: vec![agent24_sin90::NewTask {
                title: "t".into(),
                direction_id: None,
            }],
        }],
    );
    store.submit_proposal(&seed).await.unwrap();
    store.apply_proposal("p-seed").await.unwrap();
    let real = test_hooks::first_task_id(&store).await.unwrap();

    let bad = proposal(
        "p-reorder",
        vec![Sin90Op::ReorderTasks {
            week_id: wk.id.clone(),
            order: vec![real, "ghost".into()],
        }],
    );
    store.submit_proposal(&bad).await.unwrap();
    assert!(store.apply_proposal("p-reorder").await.is_err());
}

#[tokio::test]
async fn resubmit_same_id_different_ops_conflicts() {
    let store = Sin90Store::open_memory().await.unwrap();
    let a = proposal(
        "p",
        vec![Sin90Op::CreateDirection {
            title: "A".into(),
            target_window: "2026-08".into(),
        }],
    );
    store.submit_proposal(&a).await.unwrap();
    // Same id, different ops → must not be silently ignored.
    let b = proposal(
        "p",
        vec![Sin90Op::CreateDirection {
            title: "B".into(),
            target_window: "2026-08".into(),
        }],
    );
    assert!(store.submit_proposal(&b).await.is_err());
    // Re-submitting the identical batch stays idempotent (ok).
    assert!(store.submit_proposal(&a).await.is_ok());
}

#[tokio::test]
async fn attention_replay_matches_materialized_view() {
    // The two paths documented as "MUST agree" are actually cross-checked here,
    // not folded-vs-itself.
    use std::collections::HashMap;
    let store = Sin90Store::open_memory().await.unwrap();
    let d1 = store.create_direction("Coding", "2026-08").await.unwrap();
    let none_dir = store.create_block(None, None, 25).await.unwrap(); // no-direction block
    store
        .transition_block(&none_dir.id, ScheduleBlockStatus::Started)
        .await
        .unwrap();
    let last = {
        for (dir, mins) in [(Some(&d1.id), 90u32), (Some(&d1.id), 30)] {
            let b = store
                .create_block(dir.map(String::as_str), None, mins)
                .await
                .unwrap();
            store
                .transition_block(&b.id, ScheduleBlockStatus::Started)
                .await
                .unwrap();
            store
                .transition_block(&b.id, ScheduleBlockStatus::Completed)
                .await
                .unwrap();
        }
        store
            .transition_block(&none_dir.id, ScheduleBlockStatus::Completed)
            .await
            .unwrap()
    };
    store.attention_apply_new_events().await.unwrap();

    let (start, end) = window_around(&last.updated_at);
    let replay: HashMap<String, i64> = store
        .attention(&start, &end)
        .await
        .unwrap()
        .into_iter()
        .map(|r| (r.direction_id, r.actual_min))
        .collect();
    let view: HashMap<String, i64> = store
        .attention_daily()
        .await
        .unwrap()
        .into_iter()
        .map(|r| (r.direction_id, r.actual_min))
        .collect();
    assert_eq!(replay, view, "replay and materialized view must agree");
    assert_eq!(replay.get(&d1.id).copied(), Some(120));
    assert_eq!(
        replay.get("").copied(),
        Some(25),
        "no-direction folds under \"\""
    );
}

// ---- helpers ----

/// A ±1h window around an ISO-8601 timestamp is overkill precision-wise; we just
/// need the day to fall inside, so use a wide day-bracket.
fn window_around(ts: &str) -> (String, String) {
    let day = &ts[0..10];
    (format!("{day}T00:00:00Z"), format!("{day}T23:59:59Z"))
}
