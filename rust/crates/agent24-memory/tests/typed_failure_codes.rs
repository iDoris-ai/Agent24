//! P1-02c c2a (design D2, tests T5/T6 for the memory layer): every storage
//! failure a caller can see classifies into the closed `MemoryErrorCode` set, and
//! an over-long retraction id on a full partition is its own typed failure.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::str::FromStr;

use agent24_core::util::now_iso8601;
use agent24_memory::assertion::{AssertionStore, BeliefQuery, Forget};
use agent24_memory::event::{EventStore, MemEvent, Origin, Scope, Trust};
use agent24_memory::writer::{Candidate, MemoryWriter};
use agent24_memory::{KvStore, MemoryError, MemoryErrorCode, OsPartitionIdentity};
use serde_json::json;
use sqlx::{Connection, SqlitePool};

const PERSONAL: &str = "personal-owner-key";

struct Fixture {
    kv: KvStore,
    admin: SqlitePool,
    _dir: tempfile::TempDir,
}

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory.sqlite");
    let kv = KvStore::open(&path).await.unwrap();
    let admin = SqlitePool::connect(&format!("sqlite://{}", path.display()))
        .await
        .unwrap();
    let org = kv.ensure_org_for_user("u").await.unwrap();
    kv.record_personal_partition(OsPartitionIdentity {
        owner_key: PERSONAL,
        key_version: "v2",
        org_id: &org,
        space_id: "usr:u",
        user: "u",
        module: "@agent",
    })
    .await
    .unwrap();
    Fixture {
        kv,
        admin,
        _dir: dir,
    }
}

async fn set_quota(pool: &SqlitePool, rows: i64, bytes: i64) {
    sqlx::query(
        "INSERT INTO mem_owner_quota(owner, max_rows, max_bytes) VALUES(?, ?, ?)
         ON CONFLICT(owner) DO UPDATE SET max_rows=excluded.max_rows, max_bytes=excluded.max_bytes",
    )
    .bind(PERSONAL)
    .bind(rows)
    .bind(bytes)
    .execute(pool)
    .await
    .unwrap();
}

async fn pin_to_current_rows(f: &Fixture) {
    let (rows,): (i64,) = sqlx::query_as("SELECT row_count FROM mem_owner_usage WHERE owner=?")
        .bind(PERSONAL)
        .fetch_one(&f.admin)
        .await
        .unwrap();
    set_quota(&f.admin, rows, 1_000_000_000).await;
}

fn note(id: &str) -> MemEvent {
    MemEvent::new(
        id,
        Scope::owner(PERSONAL),
        "note",
        json!({"t": "x"}),
        Origin {
            source: "test".into(),
            trust: Trust::UserSaid,
        },
    )
}

async fn assert_object(kv: &KvStore, object: &str) -> String {
    let id = format!("assertion-{object}");
    kv.write_gate()
        .propose(vec![
            Candidate::new(
                id.clone(),
                Scope::owner(PERSONAL),
                "user",
                "prefers",
                json!(object),
                Origin {
                    source: "user".into(),
                    trust: Trust::UserSaid,
                },
            )
            .with_evidence(vec!["ev".into()])
            .remember(),
        ])
        .await
        .unwrap();
    id
}

// ---- the closed code set ---------------------------------------------------

#[test]
fn the_wire_strings_are_the_designed_closed_set() {
    let got: Vec<&str> = MemoryErrorCode::ALL.iter().map(|c| c.as_str()).collect();
    assert_eq!(
        got,
        [
            "memory_quota_rows",
            "memory_quota_bytes",
            "memory_disk_low",
            "memory_disk_full",
            "memory_reserved_kind",
            "memory_retract_id_too_long",
            "memory_internal",
        ]
    );
}

#[tokio::test]
async fn quota_rows_and_bytes_classify_with_their_dimension() {
    let f = fixture().await;
    f.kv.events().append(&note("n1")).await.unwrap();
    pin_to_current_rows(&f).await;
    let err = f.kv.events().append(&note("n2")).await.unwrap_err();
    assert_eq!(err.code(), MemoryErrorCode::QuotaRows);
    assert_eq!(err.code().dimension(), Some("rows"));

    set_quota(&f.admin, 1_000_000, 1).await;
    let err = f.kv.events().append(&note("n3")).await.unwrap_err();
    assert_eq!(err.code(), MemoryErrorCode::QuotaBytes);
    assert_eq!(err.code().dimension(), Some("bytes"));
}

#[tokio::test]
async fn unclassified_errors_are_internal() {
    for e in [
        MemoryError::Io("x".into()),
        MemoryError::Conflict("x".into()),
        MemoryError::NotFound("x".into()),
        MemoryError::from(sqlx::Error::Protocol("x".into())),
    ] {
        assert_eq!(e.code(), MemoryErrorCode::Internal, "{e}");
    }
}

/// A REAL SQLITE_FULL (a connection whose `max_page_count` is exhausted), not a
/// hand-built error: pins that the driver reports primary code 13 and that the
/// classifier reads it.
#[tokio::test]
async fn a_real_sqlite_full_classifies_as_disk_full() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}?mode=rwc", dir.path().join("full.db").display());
    let mut conn = sqlx::sqlite::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::from_str(&url).unwrap(),
    )
    .await
    .unwrap();
    sqlx::query("CREATE TABLE t(x BLOB)")
        .execute(&mut conn)
        .await
        .unwrap();
    sqlx::query("PRAGMA max_page_count = 8")
        .execute(&mut conn)
        .await
        .unwrap();
    let mut failure = None;
    for _ in 0..64 {
        if let Err(e) = sqlx::query("INSERT INTO t VALUES(zeroblob(4096))")
            .execute(&mut conn)
            .await
        {
            failure = Some(e);
            break;
        }
    }
    let err = MemoryError::from(failure.expect("the tiny database must fill up"));
    assert_eq!(err.code(), MemoryErrorCode::DiskFull, "{err:?}");
    assert_eq!(err.code().dimension(), None);
}

// ---- retraction of an over-long id on a full partition ----------------------

#[tokio::test]
async fn overlong_retract_id_on_a_full_partition_is_typed_and_changes_nothing() {
    let f = fixture().await;
    // The exemption covers `{"assertion_id":"<id>"}` up to 256 bytes: the JSON
    // wrapper is 19 bytes, so a 240-byte id is over (259) and a 200-byte one is under.
    let long_obj = "x".repeat(240 - "assertion-".len());
    let id = assert_object(&f.kv, &long_obj).await;
    assert_eq!(id.len(), 240);
    pin_to_current_rows(&f).await;

    let err =
        f.kv.forget(PERSONAL, &id, &now_iso8601())
            .await
            .unwrap_err();
    assert!(matches!(err, MemoryError::RetractIdTooLong), "{err:?}");
    assert_eq!(err.code(), MemoryErrorCode::RetractIdTooLong);
    // No owner key, no id, in what the error says about itself.
    let shown = format!("{err} {err:?}");
    assert!(!shown.contains(PERSONAL), "{shown}");
    assert!(!shown.contains(&id), "{shown}");

    // The attempt rolled back: the assertion is still live.
    let live =
        f.kv.assertions()
            .beliefs_as_of(&BeliefQuery::owner(PERSONAL))
            .await
            .unwrap();
    assert_eq!(live.len(), 1);
}

#[tokio::test]
async fn a_retract_id_under_the_cap_still_succeeds_on_a_full_partition() {
    let f = fixture().await;
    let obj = "y".repeat(200 - "assertion-".len());
    let id = assert_object(&f.kv, &obj).await;
    assert_eq!(id.len(), 200);
    pin_to_current_rows(&f).await;
    assert_eq!(
        f.kv.forget(PERSONAL, &id, &now_iso8601()).await.unwrap(),
        Forget::Forgotten
    );
}

/// The boundary itself: 237 bytes serialises to exactly 256 and is exempt; 238 is
/// the first one that is not.
#[tokio::test]
async fn the_retract_id_boundary_is_237_bytes() {
    for (len, exempt) in [(237usize, true), (238, false)] {
        let f = fixture().await;
        let id = assert_object(&f.kv, &"z".repeat(len - "assertion-".len())).await;
        assert_eq!(id.len(), len);
        pin_to_current_rows(&f).await;
        let got = f.kv.forget(PERSONAL, &id, &now_iso8601()).await;
        if exempt {
            assert_eq!(got.unwrap(), Forget::Forgotten, "len {len}");
        } else {
            assert!(
                matches!(got, Err(MemoryError::RetractIdTooLong)),
                "len {len}"
            );
        }
    }
}

/// c2b (W3): a failed legacy import keeps the code of its cause, so a receipt
/// can still say "quota" while the type says "history not imported".
#[test]
fn a_session_import_failure_keeps_its_causes_code() {
    let quota = MemoryError::SessionImportIncomplete(Box::new(MemoryError::QuotaExceeded {
        owner: "k".into(),
        dimension: "bytes",
    }));
    assert_eq!(quota.code(), MemoryErrorCode::QuotaBytes);
    let other = MemoryError::SessionImportIncomplete(Box::new(MemoryError::Io("x".into())));
    assert_eq!(other.code(), MemoryErrorCode::Internal);
}
