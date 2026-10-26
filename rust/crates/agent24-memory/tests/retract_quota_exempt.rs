//! P1-02c c1 (design D1, tests T1-T4 and the catalog half of T9): retraction by a
//! PERSONAL owner is exempt from the A-class quota gate and counted separately.
//!
//! The gate under test is the `mem_events_bi_quota` trigger (migration 0018), so
//! every case drives real SQL against a real file database with the real
//! migrations applied; nothing here stubs the trigger out.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use agent24_core::util::now_iso8601;
use agent24_memory::assertion::{AssertionStore, BeliefQuery, Forget};
use agent24_memory::event::{EventQuery, EventStore, MemEvent, Origin, Scope, Trust};
use agent24_memory::writer::{Candidate, MemoryWriter};
use agent24_memory::{KvStore, MemoryError, OsPartitionIdentity};
use serde_json::{Value, json};
use sqlx::SqlitePool;

const PERSONAL: &str = "personal-owner-key";

struct Fixture {
    kv: KvStore,
    admin: SqlitePool,
    _dir: tempfile::TempDir,
}

/// A file database (so a second pool can play the operator tweaking quotas) with
/// `PERSONAL` recorded as a personal partition.
async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory.sqlite");
    let kv = KvStore::open(&path).await.unwrap();
    let admin = SqlitePool::connect(&format!("sqlite://{}", path.display()))
        .await
        .unwrap();
    let org = kv.ensure_org_for_user("u").await.unwrap();
    record(&kv, &org, PERSONAL, "usr:u", "@agent", true).await;
    Fixture {
        kv,
        admin,
        _dir: dir,
    }
}

async fn record(kv: &KvStore, org: &str, key: &str, space: &str, module: &str, personal: bool) {
    let id = OsPartitionIdentity {
        owner_key: key,
        key_version: "v2",
        org_id: org,
        space_id: space,
        user: "u",
        module,
    };
    if personal {
        kv.record_personal_partition(id).await.unwrap();
    } else {
        kv.record_os_partition(id).await.unwrap();
    }
}

async fn set_quota(pool: &SqlitePool, owner: &str, rows: i64, bytes: i64) {
    sqlx::query(
        "INSERT INTO mem_owner_quota(owner, max_rows, max_bytes) VALUES(?, ?, ?)
         ON CONFLICT(owner) DO UPDATE SET max_rows=excluded.max_rows, max_bytes=excluded.max_bytes",
    )
    .bind(owner)
    .bind(rows)
    .bind(bytes)
    .execute(pool)
    .await
    .unwrap();
}

/// (row_count, byte_count, exempt_row_count, exempt_byte_count)
async fn usage(pool: &SqlitePool, owner: &str) -> (i64, i64, i64, i64) {
    sqlx::query_as(
        "SELECT row_count, byte_count, exempt_row_count, exempt_byte_count
         FROM mem_owner_usage WHERE owner=?",
    )
    .bind(owner)
    .fetch_optional(pool)
    .await
    .unwrap()
    .unwrap_or((0, 0, 0, 0))
}

fn candidate(object: &str, evidence: &str) -> Candidate {
    Candidate::new(
        format!("assertion-{object}"),
        Scope::owner(PERSONAL),
        "user",
        "prefers",
        json!(object),
        Origin {
            source: "user".into(),
            trust: Trust::UserSaid,
        },
    )
    .with_evidence(vec![evidence.into()])
    .remember()
}

/// Everything the A gate lets in: pin the personal owner's row quota to exactly
/// what it uses now, so the next A-class row is refused.
async fn fill_to_current(f: &Fixture, owner: &str) {
    let (rows, bytes, ..) = usage(&f.admin, owner).await;
    set_quota(&f.admin, owner, rows, bytes + 1_000_000).await;
}

async fn live(kv: &KvStore, owner: &str) -> usize {
    kv.assertions()
        .beliefs_as_of(&BeliefQuery::owner(owner))
        .await
        .unwrap()
        .len()
}

fn event(id: &str, owner: &str, kind: &str, body: Value) -> MemEvent {
    MemEvent::new(
        id,
        Scope::owner(owner),
        kind,
        body,
        Origin {
            source: "test".into(),
            trust: Trust::UserSaid,
        },
    )
}

fn is_quota(err: &MemoryError) -> bool {
    matches!(err, MemoryError::QuotaExceeded { .. })
}

// ---- T1 -------------------------------------------------------------------

#[tokio::test]
async fn t1_full_personal_owner_can_still_retract() {
    let f = fixture().await;
    f.kv.write_gate()
        .propose(vec![candidate("espresso", "ev1")])
        .await
        .unwrap();
    assert_eq!(live(&f.kv, PERSONAL).await, 1);
    // Quota exactly equal to current use: one more A-class row would be refused.
    fill_to_current(&f, PERSONAL).await;
    let a_before = usage(&f.admin, PERSONAL).await;

    let out =
        f.kv.forget(PERSONAL, "assertion-espresso", &now_iso8601())
            .await
            .unwrap();
    assert_eq!(out, Forget::Forgotten);
    assert_eq!(
        live(&f.kv, PERSONAL).await,
        0,
        "the assertion is no longer live"
    );

    let after = usage(&f.admin, PERSONAL).await;
    assert_eq!(
        (after.0, after.1),
        (a_before.0, a_before.1),
        "A counters untouched"
    );
    assert_eq!(after.2, 1, "one exempt row");
}

// ---- T2 -------------------------------------------------------------------

#[tokio::test]
async fn t2_exempt_rows_are_bounded_by_activations_and_survive_a_lowered_limit() {
    let f = fixture().await;
    // Room for the initial commit (audit row) plus two reasserts and nothing more.
    f.kv.write_gate()
        .propose(vec![candidate("tea", "ev0")])
        .await
        .unwrap();
    let mut activations = 1_i64;
    f.kv.forget(PERSONAL, "assertion-tea", &now_iso8601())
        .await
        .unwrap();
    let (rows, ..) = usage(&f.admin, PERSONAL).await;
    // Each reassert with fresh evidence appends an audit row and a reasserted row.
    set_quota(&f.admin, PERSONAL, rows + 4, 10_000_000).await;

    let mut n = 1;
    loop {
        n += 1;
        match f
            .kv
            .write_gate()
            .propose(vec![candidate("tea", &format!("ev{n}"))])
            .await
        {
            Ok(_) => {
                activations += 1;
                let out =
                    f.kv.forget(PERSONAL, "assertion-tea", &now_iso8601())
                        .await
                        .unwrap();
                assert_eq!(out, Forget::Forgotten, "retraction works while A is full");
            }
            Err(e) => {
                assert!(is_quota(&e), "{e:?}");
                break;
            }
        }
        assert!(n < 50, "the loop must hit the A gate");
    }
    assert!(activations >= 2, "the loop exercised several cycles");
    // Reassert was refused and nothing is left to retract.
    assert_eq!(live(&f.kv, PERSONAL).await, 0);
    assert_eq!(
        f.kv.forget(PERSONAL, "assertion-tea", &now_iso8601())
            .await
            .unwrap(),
        Forget::AlreadyForgotten
    );
    let (_, _, exempt_rows, exempt_bytes) = usage(&f.admin, PERSONAL).await;
    let retracted: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM mem_events WHERE scope_owner=? AND kind='assertion.retracted'",
    )
    .bind(PERSONAL)
    .fetch_one(&f.admin)
    .await
    .unwrap();
    assert_eq!(exempt_rows, retracted);
    assert_eq!(exempt_rows, activations, "one retraction per activation");
    assert!(exempt_bytes <= 256 * exempt_rows);

    // Lower the limit below the exempt total: still legal, and a retraction of a
    // different live assertion still goes through.
    set_quota(&f.admin, PERSONAL, 100, 10_000_000).await;
    f.kv.write_gate()
        .propose(vec![candidate("juice", "evj")])
        .await
        .unwrap();
    fill_to_current(&f, PERSONAL).await;
    set_quota(&f.admin, PERSONAL, 1, 10_000_000).await;
    assert!(
        exempt_rows >= 2,
        "exempt total exceeds the new max_rows of 1"
    );
    assert_eq!(
        f.kv.forget(PERSONAL, "assertion-juice", &now_iso8601())
            .await
            .unwrap(),
        Forget::Forgotten
    );
}

/// Insert a retraction-shaped event through the normal append path while the
/// owner is exactly full, and report whether the A gate refused it.
async fn full_owner_retraction_refused(
    owner: &str,
    body: Value,
    setup: impl AsyncFnOnce(&Fixture),
) -> bool {
    let f = fixture().await;
    setup(&f).await;
    // One A-class row so "exactly full" is meaningful for this owner.
    f.kv.events()
        .append(&event(
            &format!("seed-{owner}"),
            owner,
            "note",
            json!({"n": 1}),
        ))
        .await
        .unwrap();
    fill_to_current(&f, owner).await;
    match f
        .kv
        .events()
        .append(&event("probe", owner, "assertion.retracted", body))
        .await
    {
        Ok(_) => false,
        Err(e) => {
            assert!(is_quota(&e), "{e:?}");
            true
        }
    }
}

#[tokio::test]
async fn t2_valid_shape_is_exempt_for_personal() {
    // Positive control for the four negatives below: the same harness lets a
    // well-formed personal retraction through, so a refusal means the shape or
    // owner check fired and not that the harness always refuses.
    let refused =
        full_owner_retraction_refused(PERSONAL, json!({"assertion_id": "a"}), async |_| {}).await;
    assert!(!refused);
}

#[tokio::test]
async fn t2_multi_key_payload_takes_the_a_gate() {
    assert!(
        full_owner_retraction_refused(
            PERSONAL,
            json!({"assertion_id": "a", "extra": 1}),
            async |_| {}
        )
        .await
    );
}

#[tokio::test]
async fn t2_non_string_assertion_id_takes_the_a_gate() {
    assert!(
        full_owner_retraction_refused(PERSONAL, json!({"assertion_id": 7}), async |_| {}).await
    );
}

#[tokio::test]
async fn t2_over_256_bytes_takes_the_a_gate() {
    let big = "x".repeat(300);
    assert!(
        full_owner_retraction_refused(PERSONAL, json!({"assertion_id": big}), async |_| {}).await
    );
    // Boundary: exactly 256 bytes is still exempt. `{"assertion_id":""}` is 19 bytes.
    let edge = "x".repeat(256 - 19);
    assert_eq!(json!({"assertion_id": edge}).to_string().len(), 256);
    assert!(
        !full_owner_retraction_refused(PERSONAL, json!({"assertion_id": edge}), async |_| {}).await
    );
    let over = "x".repeat(256 - 19 + 1);
    assert!(
        full_owner_retraction_refused(PERSONAL, json!({"assertion_id": over}), async |_| {}).await
    );
}

#[tokio::test]
async fn t2_prefix_lookalike_owner_outside_the_catalog_takes_the_a_gate() {
    // Looks personal by prefix, but is not in the personal catalog.
    assert!(
        full_owner_retraction_refused(
            "personal-owner-key-lookalike",
            json!({"assertion_id": "a"}),
            async |_| {}
        )
        .await
    );
}

#[tokio::test]
async fn t2_owner_recorded_as_a_module_partition_takes_the_a_gate() {
    let owner = "module-owner-key";
    assert!(
        full_owner_retraction_refused(owner, json!({"assertion_id": "a"}), async |f| {
            let org = f.kv.ensure_org_for_user("u").await.unwrap();
            record(&f.kv, &org, owner, "os:sin90", "sin90", false).await;
        })
        .await
    );
}

#[tokio::test]
async fn t2_malformed_or_non_object_payloads_take_the_a_gate_without_erroring() {
    // Raw SQL: `MemEvent` cannot carry invalid JSON. The CASE in the trigger must
    // keep json_each away from malformed text, so these end in the quota error,
    // not in a JSON error.
    for payload in [
        "not json",
        "[\"a\"]",
        "\"assertion_id\"",
        "{\"assertion_id\":\"a\",\"assertion_id\":\"b\"}",
    ] {
        let f = fixture().await;
        f.kv.events()
            .append(&event("seed", PERSONAL, "note", json!({"n": 1})))
            .await
            .unwrap();
        fill_to_current(&f, PERSONAL).await;
        let err = sqlx::query(
            "INSERT INTO mem_events (id, scope_owner, scope, kind, payload, origin_source, origin_trust, at)
             VALUES ('raw', ?, '{}', 'assertion.retracted', ?, 't', 'user_said', 'now')",
        )
        .bind(PERSONAL)
        .bind(payload)
        .execute(&f.admin)
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("mem_quota:rows"),
            "payload {payload:?}: {err}"
        );
    }
}

#[tokio::test]
async fn t2_payloads_that_make_the_exemption_expression_null_take_the_a_gate() {
    // Three-valued logic: a missing key makes json_type(..) NULL, and an
    // unguarded NOT(NULL) is NULL, which would skip the BEFORE gate while the
    // AFTER trigger still counted the row. Every shape below must be refused by
    // the quota while the owner is exactly full, again and again with fresh ids.
    let bodies: [(&str, Value); 8] = [
        ("missing key", json!({"x": "a"})),
        ("wrong key name", json!({"assertion_ids": "a"})),
        ("null value", json!({"assertion_id": null})),
        ("empty object", json!({})),
        ("json null literal", Value::Null),
        ("number", json!(7)),
        ("string", json!("assertion_id")),
        ("array", json!(["a"])),
    ];
    for (label, body) in bodies {
        let f = fixture().await;
        f.kv.events()
            .append(&event("seed", PERSONAL, "note", json!({"n": 1})))
            .await
            .unwrap();
        fill_to_current(&f, PERSONAL).await;
        let before = usage(&f.admin, PERSONAL).await;
        for i in 0..3 {
            let err =
                f.kv.events()
                    .append(&event(
                        &format!("probe-{i}"),
                        PERSONAL,
                        "assertion.retracted",
                        body.clone(),
                    ))
                    .await
                    .expect_err(label);
            assert!(is_quota(&err), "{label}: {err:?}");
        }
        assert_eq!(usage(&f.admin, PERSONAL).await, before, "{label}");
    }
}

// ---- T3 -------------------------------------------------------------------

#[tokio::test]
async fn t3_full_owner_still_refuses_content_and_rolls_the_audit_back() {
    let f = fixture().await;
    f.kv.write_gate()
        .propose(vec![candidate("espresso", "ev1")])
        .await
        .unwrap();
    f.kv.forget(PERSONAL, "assertion-espresso", &now_iso8601())
        .await
        .unwrap();
    fill_to_current(&f, PERSONAL).await;
    let events_before =
        f.kv.events()
            .scan(&EventQuery::owner(PERSONAL))
            .await
            .unwrap();
    let usage_before = usage(&f.admin, PERSONAL).await;

    // A message.
    let err =
        f.kv.events()
            .append(&event("msg", PERSONAL, "message", json!({"text": "hi"})))
            .await
            .unwrap_err();
    assert!(is_quota(&err), "{err:?}");
    // A brand-new assertion: the assertion row and its mem.write_decision audit
    // are one transaction and both vanish.
    let err =
        f.kv.write_gate()
            .propose(vec![candidate("cocoa", "evc")])
            .await
            .unwrap_err();
    assert!(is_quota(&err), "{err:?}");
    let ghost: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM mem_assertions WHERE id='assertion-cocoa'")
            .fetch_one(&f.admin)
            .await
            .unwrap();
    assert_eq!(ghost, 0, "no assertion row without its audit");
    // A reassert of the retracted assertion.
    let err =
        f.kv.write_gate()
            .propose(vec![candidate("espresso", "ev2")])
            .await
            .unwrap_err();
    assert!(is_quota(&err), "{err:?}");
    assert_eq!(live(&f.kv, PERSONAL).await, 0, "still retracted");

    assert_eq!(
        f.kv.events()
            .scan(&EventQuery::owner(PERSONAL))
            .await
            .unwrap(),
        events_before,
        "no content row and no mem.write_decision was added"
    );
    assert_eq!(usage(&f.admin, PERSONAL).await, usage_before);
}

// ---- T4 -------------------------------------------------------------------

#[tokio::test]
async fn t4_exempt_rows_do_not_move_the_a_class_counters() {
    let f = fixture().await;
    for (i, o) in ["a", "b", "c"].iter().enumerate() {
        f.kv.write_gate()
            .propose(vec![candidate(o, &format!("e{i}"))])
            .await
            .unwrap();
    }
    fill_to_current(&f, PERSONAL).await;
    let (rows0, bytes0, ..) = usage(&f.admin, PERSONAL).await;
    let mut expect_exempt_bytes = 0;
    for (i, o) in ["a", "b", "c"].iter().enumerate() {
        f.kv.forget(PERSONAL, &format!("assertion-{o}"), &now_iso8601())
            .await
            .unwrap();
        let (rows, bytes, er, eb) = usage(&f.admin, PERSONAL).await;
        assert_eq!((rows, bytes), (rows0, bytes0), "A counters never move");
        assert_eq!(er, i as i64 + 1);
        expect_exempt_bytes = eb;
    }
    assert!(expect_exempt_bytes > 0);
    let real: i64 = sqlx::query_scalar(
        "SELECT SUM(LENGTH(CAST(payload AS BLOB))) FROM mem_events
         WHERE scope_owner=? AND kind='assertion.retracted'",
    )
    .bind(PERSONAL)
    .fetch_one(&f.admin)
    .await
    .unwrap();
    assert_eq!(expect_exempt_bytes, real);
    // The A gate is unaffected by the exempt rows: it still refuses at the same
    // boundary, and the refusal is not caused by exempt rows being counted.
    let err =
        f.kv.events()
            .append(&event("n1", PERSONAL, "note", json!({"n": 1})))
            .await
            .unwrap_err();
    assert!(is_quota(&err), "{err:?}");
    // One row of headroom is enough, whatever the exempt total is.
    set_quota(&f.admin, PERSONAL, rows0 + 1, 10_000_000).await;
    f.kv.events()
        .append(&event("n2", PERSONAL, "note", json!({"n": 2})))
        .await
        .unwrap();
}

// ---- persisted billing class (UPDATE / DELETE) ----------------------------

/// The counters must equal what the rows and the persisted-class table say,
/// and nothing may be negative.
async fn assert_usage_consistent(pool: &SqlitePool, owner: &str) {
    let (rows, bytes, er, eb) = usage(pool, owner).await;
    let (want_er, want_eb): (i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), COALESCE(SUM(LENGTH(CAST(e.payload AS BLOB))), 0) FROM mem_events e
         JOIN mem_event_quota_exempt x ON x.event_id = e.id WHERE e.scope_owner = ?",
    )
    .bind(owner)
    .fetch_one(pool)
    .await
    .unwrap();
    let (all_rows, all_bytes): (i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), COALESCE(SUM(LENGTH(CAST(payload AS BLOB))), 0) FROM mem_events
         WHERE scope_owner = ?",
    )
    .bind(owner)
    .fetch_one(pool)
    .await
    .unwrap();
    assert!(
        rows >= 0 && bytes >= 0 && er >= 0 && eb >= 0,
        "{owner}: negative counter"
    );
    assert_eq!((er, eb), (want_er, want_eb), "{owner}: exempt counters");
    assert_eq!(
        (rows, bytes),
        (all_rows - want_er, all_bytes - want_eb),
        "{owner}: A counters"
    );
}

async fn exempt_table_rows(pool: &SqlitePool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM mem_event_quota_exempt")
        .fetch_one(pool)
        .await
        .unwrap()
}

/// One exempt retraction ("exempt-1") and one A-class note ("a-1") for PERSONAL.
async fn with_exempt_and_a_rows() -> Fixture {
    let f = fixture().await;
    f.kv.events()
        .append(&event("a-1", PERSONAL, "note", json!({"n": 1})))
        .await
        .unwrap();
    fill_to_current(&f, PERSONAL).await;
    f.kv.events()
        .append(&event(
            "exempt-1",
            PERSONAL,
            "assertion.retracted",
            json!({"assertion_id": "a"}),
        ))
        .await
        .unwrap();
    assert_eq!(exempt_table_rows(&f.admin).await, 1);
    assert_usage_consistent(&f.admin, PERSONAL).await;
    f
}

#[tokio::test]
async fn delete_of_an_exempt_row_decrements_the_exempt_counters_only() {
    let f = with_exempt_and_a_rows().await;
    let before = usage(&f.admin, PERSONAL).await;
    sqlx::query("DELETE FROM mem_events WHERE id='exempt-1'")
        .execute(&f.admin)
        .await
        .unwrap();
    let after = usage(&f.admin, PERSONAL).await;
    assert_eq!((after.0, after.1), (before.0, before.1));
    assert_eq!((after.2, after.3), (0, 0));
    assert_eq!(
        exempt_table_rows(&f.admin).await,
        0,
        "the record goes with the row"
    );
    assert_usage_consistent(&f.admin, PERSONAL).await;
}

#[tokio::test]
async fn delete_of_an_a_class_row_decrements_the_a_counters_only() {
    let f = with_exempt_and_a_rows().await;
    let before = usage(&f.admin, PERSONAL).await;
    sqlx::query("DELETE FROM mem_events WHERE id='a-1'")
        .execute(&f.admin)
        .await
        .unwrap();
    let after = usage(&f.admin, PERSONAL).await;
    assert_eq!((after.0, after.1), (0, 0));
    assert_eq!((after.2, after.3), (before.2, before.3));
    assert_usage_consistent(&f.admin, PERSONAL).await;
}

#[tokio::test]
async fn owner_move_transfers_each_row_within_its_own_class() {
    for moved in ["a-1", "exempt-1"] {
        let f = with_exempt_and_a_rows().await;
        // The target is neither personal nor module-recorded: the class travels
        // with the row, not with the destination's catalog state.
        let before = usage(&f.admin, PERSONAL).await;
        sqlx::query("UPDATE mem_events SET scope_owner='other' WHERE id=?")
            .bind(moved)
            .execute(&f.admin)
            .await
            .unwrap();
        let (p, o) = (
            usage(&f.admin, PERSONAL).await,
            usage(&f.admin, "other").await,
        );
        if moved == "a-1" {
            assert_eq!((p.0, p.2), (before.0 - 1, before.2), "{moved}");
            assert_eq!((o.0, o.2), (1, 0), "{moved}");
        } else {
            assert_eq!((p.0, p.2), (before.0, before.2 - 1), "{moved}");
            assert_eq!((o.0, o.2), (0, 1), "{moved}");
        }
        assert_usage_consistent(&f.admin, PERSONAL).await;
        assert_usage_consistent(&f.admin, "other").await;
    }
}

#[tokio::test]
async fn catalog_change_after_the_write_does_not_change_how_a_row_is_released() {
    // Exempt row written while PERSONAL is personal; the partition is then
    // reclassified. Deleting must still release the exempt counter.
    let f = with_exempt_and_a_rows().await;
    sqlx::query("UPDATE mem_os_partitions SET space_kind='module' WHERE owner_key=?")
        .bind(PERSONAL)
        .execute(&f.admin)
        .await
        .unwrap();
    sqlx::query("DELETE FROM mem_events WHERE id='exempt-1'")
        .execute(&f.admin)
        .await
        .unwrap();
    assert_eq!(usage(&f.admin, PERSONAL).await.2, 0);
    assert_usage_consistent(&f.admin, PERSONAL).await;

    // Converse: a retraction-shaped row written while the owner is a MODULE is
    // A-class; making the owner personal afterwards must not turn its release
    // into an exempt decrement.
    f.kv.events()
        .append(&event(
            "late",
            PERSONAL,
            "assertion.retracted",
            json!({"assertion_id": "z"}),
        ))
        .await
        .expect_err("a module-classified owner is at its quota");
    set_quota(&f.admin, PERSONAL, 100, 10_000_000).await;
    f.kv.events()
        .append(&event(
            "late",
            PERSONAL,
            "assertion.retracted",
            json!({"assertion_id": "z"}),
        ))
        .await
        .unwrap();
    assert_eq!(exempt_table_rows(&f.admin).await, 0, "billed A-class");
    sqlx::query("UPDATE mem_os_partitions SET space_kind='personal' WHERE owner_key=?")
        .bind(PERSONAL)
        .execute(&f.admin)
        .await
        .unwrap();
    sqlx::query("DELETE FROM mem_events WHERE id='late'")
        .execute(&f.admin)
        .await
        .unwrap();
    assert_eq!(usage(&f.admin, PERSONAL).await.2, 0);
    assert_usage_consistent(&f.admin, PERSONAL).await;
}

#[tokio::test]
async fn broken_bookkeeping_aborts_instead_of_being_clamped_away() {
    let f = with_exempt_and_a_rows().await;
    sqlx::query("UPDATE mem_owner_usage SET row_count = 0 WHERE owner=?")
        .bind(PERSONAL)
        .execute(&f.admin)
        .await
        .unwrap();
    let err = sqlx::query("DELETE FROM mem_events WHERE id='a-1'")
        .execute(&f.admin)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("mem_quota:underflow"), "{err}");
    let err = sqlx::query("UPDATE mem_events SET scope_owner='other' WHERE id='a-1'")
        .execute(&f.admin)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("mem_quota:underflow"), "{err}");
}

#[tokio::test]
async fn event_id_cannot_be_rewritten_under_the_persisted_class() {
    let f = with_exempt_and_a_rows().await;
    let err = sqlx::query("UPDATE mem_events SET id='renamed' WHERE id='exempt-1'")
        .execute(&f.admin)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("immutable"), "{err}");
}

/// Database upgraded from 0017 holding one pre-0018 personal retraction row.
async fn upgraded_with_old_row() -> (tempfile::TempDir, KvStore, SqlitePool, (i64, i64)) {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.db");
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
                .unwrap()
                .create_if_missing(true)
                .foreign_keys(true),
        )
        .await
        .unwrap();
    let mut migrator = sqlx::migrate!("./migrations");
    migrator.migrations = migrator
        .migrations
        .iter()
        .filter(|m| m.version < 18)
        .cloned()
        .collect::<Vec<_>>()
        .into();
    migrator.run(&pool).await.unwrap();
    sqlx::query("INSERT INTO mem_orgs (org_id, display_name, created_at) VALUES ('o','O','t')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO mem_org_members (org_id,user_id,joined_at) VALUES ('o','u','t')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO mem_os_partitions (owner_key,key_version,org_id,space_id,logical_user,module_name,first_seen_at,last_seen_at,space_kind) VALUES ('P','v2','o','usr:u','u','@agent','t',NULL,'personal')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO mem_events (id, scope_owner, scope, kind, payload, origin_source, origin_trust, at)
         VALUES ('old', 'P', '{}', 'assertion.retracted', '{\"assertion_id\":\"a\"}', 't', 'user_said', 'now')")
        .execute(&pool)
        .await
        .unwrap();
    let old: (i64, i64) =
        sqlx::query_as("SELECT row_count, byte_count FROM mem_owner_usage WHERE owner='P'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(old.0, 1);
    pool.close().await;
    let kv = KvStore::open(&path).await.unwrap();
    let admin = SqlitePool::connect(&format!("sqlite://{}", path.display()))
        .await
        .unwrap();
    (dir, kv, admin, old)
}

#[tokio::test]
async fn upgrade_does_not_backfill_old_retractions_and_new_ones_are_exempt() {
    let (_dir, _kv, admin, old) = upgraded_with_old_row().await;
    assert_eq!(
        usage(&admin, "P").await,
        (old.0, old.1, 0, 0),
        "no backfill"
    );
    assert_eq!(exempt_table_rows(&admin).await, 0);
    set_quota(&admin, "P", 1, 10_000_000).await;
    sqlx::query("INSERT INTO mem_events (id, scope_owner, scope, kind, payload, origin_source, origin_trust, at)
         VALUES ('new', 'P', '{}', 'assertion.retracted', '{\"assertion_id\":\"a\"}', 't', 'user_said', 'now')")
        .execute(&admin)
        .await
        .unwrap();
    let after = usage(&admin, "P").await;
    assert_eq!((after.0, after.1, after.2), (old.0, old.1, 1));
}

#[tokio::test]
async fn deleting_a_pre_upgrade_retraction_row_releases_the_a_counters() {
    let (_dir, _kv, admin, old) = upgraded_with_old_row().await;
    assert_eq!(old.0, 1);
    sqlx::query("DELETE FROM mem_events WHERE id='old'")
        .execute(&admin)
        .await
        .unwrap();
    assert_eq!(usage(&admin, "P").await, (0, 0, 0, 0));
    assert_usage_consistent(&admin, "P").await;
}

#[tokio::test]
async fn moving_a_pre_upgrade_retraction_row_keeps_it_a_class() {
    let (_dir, _kv, admin, _old) = upgraded_with_old_row().await;
    sqlx::query("UPDATE mem_events SET scope_owner='Q' WHERE id='old'")
        .execute(&admin)
        .await
        .unwrap();
    assert_eq!(usage(&admin, "P").await, (0, 0, 0, 0));
    let q = usage(&admin, "Q").await;
    assert_eq!((q.0, q.2), (1, 0));
    assert_usage_consistent(&admin, "Q").await;
}

// ---- REPLACE guard (mem_events_bi_no_replace) --------------------------------

async fn snapshot(pool: &SqlitePool) -> Vec<(i64, String, String, String)> {
    sqlx::query_as("SELECT seq, id, kind, payload FROM mem_events ORDER BY seq")
        .fetch_all(pool)
        .await
        .unwrap()
}

const REPLACE_FORMS: [&str; 3] = [
    "INSERT OR REPLACE INTO",
    "REPLACE INTO",
    "INSERT OR IGNORE INTO",
];

#[tokio::test]
async fn replace_by_id_cannot_swap_an_exempt_row_for_an_over_quota_row() {
    for form in REPLACE_FORMS {
        let f = with_exempt_and_a_rows().await;
        let rows = snapshot(&f.admin).await;
        let counters = usage(&f.admin, PERSONAL).await;
        let big = "x".repeat(5000);
        // Same id as the exempt retraction, but an ordinary, large note.
        sqlx::query(&format!(
            "{form} mem_events (id, scope_owner, scope, kind, payload, origin_source, origin_trust, at)
             VALUES ('exempt-1', ?, '{{}}', 'note', ?, 't', 'user_said', 'now')"
        ))
        .bind(PERSONAL)
        .bind(format!("{{\"pad\":\"{big}\"}}"))
        .execute(&f.admin)
        .await
        .unwrap();
        assert_eq!(
            snapshot(&f.admin).await,
            rows,
            "{form}: stored events unchanged"
        );
        assert_eq!(usage(&f.admin, PERSONAL).await, counters, "{form}");
        assert_eq!(exempt_table_rows(&f.admin).await, 1, "{form}");
        assert_usage_consistent(&f.admin, PERSONAL).await;
    }
}

#[tokio::test]
async fn replace_by_seq_with_a_new_id_cannot_leave_a_ghost_or_a_stale_record() {
    for victim in ["a-1", "exempt-1"] {
        let f = with_exempt_and_a_rows().await;
        let seq: i64 = sqlx::query_scalar("SELECT seq FROM mem_events WHERE id = ?")
            .bind(victim)
            .fetch_one(&f.admin)
            .await
            .unwrap();
        let rows = snapshot(&f.admin).await;
        let counters = usage(&f.admin, PERSONAL).await;
        sqlx::query(
            "INSERT OR REPLACE INTO mem_events (seq, id, scope_owner, scope, kind, payload, origin_source, origin_trust, at)
             VALUES (?, 'fresh-id', 'other', '{}', 'note', '{}', 't', 'user_said', 'now')",
        )
        .bind(seq)
        .execute(&f.admin)
        .await
        .unwrap();
        assert_eq!(snapshot(&f.admin).await, rows, "{victim}");
        assert_eq!(usage(&f.admin, PERSONAL).await, counters, "{victim}");
        assert_eq!(usage(&f.admin, "other").await, (0, 0, 0, 0), "{victim}");
        assert_eq!(exempt_table_rows(&f.admin).await, 1, "{victim}");
        assert_usage_consistent(&f.admin, PERSONAL).await;
    }
}

#[tokio::test]
async fn idempotent_replay_still_works_at_the_quota_boundary() {
    let f = with_exempt_and_a_rows().await;
    // Both the A-class row and the exempt row replay as no-ops while full.
    let a = event("a-1", PERSONAL, "note", json!({"n": 1}));
    let first = f.kv.events().append(&a).await.unwrap();
    assert_eq!(f.kv.events().append(&a).await.unwrap(), first);
    let ex = event(
        "exempt-1",
        PERSONAL,
        "assertion.retracted",
        json!({"assertion_id": "a"}),
    );
    f.kv.events().append(&ex).await.unwrap();
    // A same-id replay with different content is still a Rust-level conflict,
    // not a silent success and not a quota error.
    let err =
        f.kv.events()
            .append(&event("a-1", PERSONAL, "note", json!({"n": 2})))
            .await
            .unwrap_err();
    assert!(matches!(err, MemoryError::Conflict(_)), "{err:?}");
    assert_eq!(exempt_table_rows(&f.admin).await, 1);
    assert_usage_consistent(&f.admin, PERSONAL).await;
}
