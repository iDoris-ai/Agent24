//! Schema for migration 0036. Behavior of the store API lives in `skill.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::str::FromStr;

use agent24_store::Store;
use agent24_store::test_hooks;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

async fn memory() -> Store {
    Store::open_memory().await.unwrap()
}

fn pool(store: &Store) -> &SqlitePool {
    test_hooks::pool(store)
}

async fn reject(store: &Store, sql: &str) {
    let err = sqlx::query(sql).execute(pool(store)).await.unwrap_err();
    assert!(
        matches!(err, sqlx::Error::Database(_)),
        "{err:?} from {sql}"
    );
}

async fn insert_revision(
    store: &Store,
    revision_id: &str,
    origin: &str,
    semver: &str,
    kind: &str,
    digest: &str,
) {
    sqlx::query(
        "INSERT INTO skill_revision ( \
            revision_id, skill_id, origin, semver, kind, content_digest, content, created_at \
         ) VALUES (?, 'notes', ?, ?, ?, ?, x'61', '2026-11-12T00:00:00Z')",
    )
    .bind(revision_id)
    .bind(origin)
    .bind(semver)
    .bind(kind)
    .bind(digest)
    .execute(pool(store))
    .await
    .unwrap();
}

#[tokio::test]
async fn upgrading_from_0035_leaves_old_rows_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.db");
    test_hooks::prepare_db_stopped_before(&path, 36)
        .await
        .unwrap();
    assert_eq!(test_hooks::applied_schema_version(&path).await.unwrap(), 35);

    let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
        .unwrap()
        .create_if_missing(false)
        .journal_mode(SqliteJournalMode::Wal)
        .foreign_keys(true);
    let raw = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO runs (id, status, input, usage, created_at) \
         VALUES ('old_run', 'completed', '{\"prompt\":\"keep-me\"}', '{}', '2026-01-01T00:00:00Z')",
    )
    .execute(&raw)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO inbound_cursor (source, recipient_identity, thread_key, cursor) \
         VALUES ('hyphae', 'npub1', 'group1', 'm1')",
    )
    .execute(&raw)
    .await
    .unwrap();
    raw.close().await;

    let store = Store::open(&path).await.unwrap();
    assert_eq!(store.latest_applied_migration().await.unwrap(), 36);
    let input: String = sqlx::query_scalar("SELECT input FROM runs WHERE id = 'old_run'")
        .fetch_one(pool(&store))
        .await
        .unwrap();
    assert_eq!(input, "{\"prompt\":\"keep-me\"}");
    let cursor: String = sqlx::query_scalar(
        "SELECT cursor FROM inbound_cursor \
         WHERE source = 'hyphae' AND recipient_identity = 'npub1' AND thread_key = 'group1'",
    )
    .fetch_one(pool(&store))
    .await
    .unwrap();
    assert_eq!(cursor, "m1");
    for table in [
        "skill_revision",
        "skill_pointer",
        "skill_eval_result",
        "skill_activation",
        "run_skill_pin",
    ] {
        let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(pool(&store))
            .await
            .unwrap();
        assert_eq!(count, 0, "{table}");
    }
    let index: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master \
         WHERE type = 'index' AND name = 'idx_skill_revision_content'",
    )
    .fetch_one(pool(&store))
    .await
    .unwrap();
    assert_eq!(index, 1);
}

#[tokio::test]
async fn checks_reject_illegal_skill_values() {
    let store = memory().await;
    reject(
        &store,
        "INSERT INTO skill_revision ( \
            revision_id, skill_id, origin, semver, kind, content_digest, content, created_at \
         ) VALUES ('r1', 'notes', 'distro', '1.0.0', 'body', 'd', x'61', '2026-11-12T00:00:00Z')",
    )
    .await;
    reject(
        &store,
        "INSERT INTO skill_revision ( \
            revision_id, skill_id, origin, semver, kind, content_digest, content, created_at \
         ) VALUES ('r2', '', 'user', '1.0.0', 'body', 'd', x'61', '2026-11-12T00:00:00Z')",
    )
    .await;
    reject(
        &store,
        "INSERT INTO skill_revision ( \
            revision_id, skill_id, origin, semver, kind, content_digest, content, created_at \
         ) VALUES ('r3', 'notes', 'user', '1.0.0', 'script', 'd', x'61', '2026-11-12T00:00:00Z')",
    )
    .await;

    insert_revision(&store, "body-1", "user", "1.0.0", "body", "db").await;
    insert_revision(&store, "evals-1", "user", "1.0.0", "evals", "de").await;
    let bad_accepted = ["'{}'", "'\"x\"'", "'null'", "'1'", "'not-json'"];
    for (index, accepted) in bad_accepted.iter().enumerate() {
        let sql = format!(
            "INSERT INTO skill_eval_result ( \
                result_id, body_revision_id, evals_revision_id, segment_digest, \
                renderer_version, cases_json, accepted_json, created_at \
             ) VALUES ('bad-{index}', 'body-1', 'evals-1', 'seg', 'r1', '[]', {accepted}, '2026-11-12T00:00:00Z')"
        );
        reject(&store, &sql).await;
    }
    sqlx::query(
        "INSERT INTO skill_eval_result ( \
            result_id, body_revision_id, evals_revision_id, segment_digest, \
            renderer_version, cases_json, created_at \
         ) VALUES ('ok-default', 'body-1', 'evals-1', 'seg', 'r1', '[]', '2026-11-12T00:00:00Z')",
    )
    .execute(pool(&store))
    .await
    .unwrap();
    let accepted: String = sqlx::query_scalar(
        "SELECT accepted_json FROM skill_eval_result WHERE result_id = 'ok-default'",
    )
    .fetch_one(pool(&store))
    .await
    .unwrap();
    assert_eq!(accepted, "[]");
    sqlx::query(
        "INSERT INTO skill_eval_result ( \
            result_id, body_revision_id, evals_revision_id, segment_digest, \
            renderer_version, cases_json, accepted_json, created_at \
         ) VALUES ('ok-list', 'body-1', 'evals-1', 'seg', 'r1', '[]', '[\"bad\"]', '2026-11-12T00:00:00Z')",
    )
    .execute(pool(&store))
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO runs (id, status, input, usage, created_at) \
         VALUES ('run-1', 'completed', '{}', '{}', '2026-11-12T00:00:00Z')",
    )
    .execute(pool(&store))
    .await
    .unwrap();
    reject(
        &store,
        "INSERT INTO run_skill_pin ( \
            run_id, skill_id, body_revision_id, content_digest, providers_json \
         ) VALUES ('run-1', 'notes', 'body-1', 'db', '[]')",
    )
    .await;
    reject(
        &store,
        "INSERT INTO run_skill_pin ( \
            run_id, skill_id, body_revision_id, content_digest, providers_json \
         ) VALUES ('run-1', 'notes', 'body-1', 'db', '\"x\"')",
    )
    .await;
    sqlx::query(
        "INSERT INTO run_skill_pin ( \
            run_id, skill_id, body_revision_id, content_digest, providers_json \
         ) VALUES ('run-1', 'notes', 'body-1', 'db', '{\"cap\":\"comp@1\"}')",
    )
    .execute(pool(&store))
    .await
    .unwrap();

    let revisions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM skill_revision")
        .fetch_one(pool(&store))
        .await
        .unwrap();
    assert_eq!(revisions, 2);
}

#[tokio::test]
async fn content_digest_is_unique_per_skill_origin_and_kind() {
    let store = memory().await;
    insert_revision(&store, "body-1", "user", "1.0.0", "body", "same").await;
    let dup = sqlx::query(
        "INSERT INTO skill_revision ( \
            revision_id, skill_id, origin, semver, kind, content_digest, content, created_at \
         ) VALUES ('body-2', 'notes', 'user', '1.1.0', 'body', 'same', x'61', '2026-11-12T00:00:01Z')",
    )
    .execute(pool(&store))
    .await
    .unwrap_err();
    assert!(matches!(dup, sqlx::Error::Database(_)), "{dup:?}");

    insert_revision(
        &store,
        "body-other-origin",
        "imported",
        "2.0.0",
        "body",
        "same",
    )
    .await;
    insert_revision(&store, "evals-1", "user", "1.0.0", "evals", "same").await;
    insert_revision(&store, "body-3", "user", "1.2.0", "body", "other").await;

    let semver = sqlx::query(
        "INSERT INTO skill_revision ( \
            revision_id, skill_id, origin, semver, kind, content_digest, content, created_at \
         ) VALUES ('body-4', 'notes', 'user', '1.0.0', 'body', 'third', x'61', '2026-11-12T00:00:02Z')",
    )
    .execute(pool(&store))
    .await
    .unwrap_err();
    assert!(matches!(semver, sqlx::Error::Database(_)), "{semver:?}");

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM skill_revision")
        .fetch_one(pool(&store))
        .await
        .unwrap();
    assert_eq!(count, 4);

    sqlx::query(
        "INSERT INTO skill_eval_result ( \
            result_id, body_revision_id, evals_revision_id, segment_digest, \
            renderer_version, cases_json, created_at \
         ) VALUES ('res-1', 'body-1', 'evals-1', 'seg', 'r1', '[]', '2026-11-12T00:00:03Z')",
    )
    .execute(pool(&store))
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO skill_activation ( \
            activation_id, skill_id, body_revision_id, evals_revision_id, \
            result_id, result_code, created_at \
         ) VALUES ('act-later', 'notes', 'body-1', 'evals-1', 'res-1', 'activated', '2026-11-12T00:00:09Z')",
    )
    .execute(pool(&store))
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO skill_activation ( \
            activation_id, skill_id, body_revision_id, evals_revision_id, \
            result_id, result_code, created_at \
         ) VALUES ('act-earlier', 'notes', 'body-1', 'evals-1', 'res-1', 'rolled_back', '2026-11-12T00:00:01Z')",
    )
    .execute(pool(&store))
    .await
    .unwrap();
    let ids: Vec<(i64, String)> = sqlx::query_as(
        "SELECT seq, activation_id FROM skill_activation WHERE skill_id = 'notes' ORDER BY seq ASC",
    )
    .fetch_all(pool(&store))
    .await
    .unwrap();
    assert_eq!(ids.len(), 2);
    assert!(ids[0].0 < ids[1].0);
    assert_eq!(ids[0].1, "act-later");
    assert_eq!(ids[1].1, "act-earlier");
}

async fn seed_body_and_evals(store: &Store) {
    insert_revision(store, "body-1", "user", "1.0.0", "body", "db").await;
    insert_revision(store, "evals-1", "user", "1.0.0", "evals", "de").await;
}

async fn insert_eval(store: &Store, result_id: &str) {
    sqlx::query(
        "INSERT INTO skill_eval_result ( \
            result_id, body_revision_id, evals_revision_id, segment_digest, \
            renderer_version, cases_json, created_at \
         ) VALUES (?, 'body-1', 'evals-1', 'seg', 'r1', '[]', '2026-11-12T00:00:00Z')",
    )
    .bind(result_id)
    .execute(pool(store))
    .await
    .unwrap();
}

#[tokio::test]
async fn equal_body_and_evals_revision_ids_are_rejected() {
    let store = memory().await;
    seed_body_and_evals(&store).await;
    reject(
        &store,
        "INSERT INTO skill_pointer ( \
            skill_id, body_revision_id, evals_revision_id, generation \
         ) VALUES ('notes', 'body-1', 'body-1', 1)",
    )
    .await;
    reject(
        &store,
        "INSERT INTO skill_eval_result ( \
            result_id, body_revision_id, evals_revision_id, segment_digest, \
            renderer_version, cases_json, created_at \
         ) VALUES ('same', 'body-1', 'body-1', 'seg', 'r1', '[]', '2026-11-12T00:00:00Z')",
    )
    .await;
    insert_eval(&store, "res-ok").await;
    reject(
        &store,
        "INSERT INTO skill_activation ( \
            activation_id, skill_id, body_revision_id, evals_revision_id, \
            result_id, result_code, created_at \
         ) VALUES ('act-same', 'notes', 'body-1', 'body-1', 'res-ok', 'activated', '2026-11-12T00:00:00Z')",
    )
    .await;
}

#[tokio::test]
async fn pointer_generation_below_one_is_rejected() {
    let store = memory().await;
    seed_body_and_evals(&store).await;
    reject(
        &store,
        "INSERT INTO skill_pointer ( \
            skill_id, body_revision_id, evals_revision_id, generation \
         ) VALUES ('notes', 'body-1', 'evals-1', 0)",
    )
    .await;
}

#[tokio::test]
async fn activation_result_code_outside_the_pair_is_rejected() {
    let store = memory().await;
    seed_body_and_evals(&store).await;
    insert_eval(&store, "res-ok").await;
    reject(
        &store,
        "INSERT INTO skill_activation ( \
            activation_id, skill_id, body_revision_id, evals_revision_id, \
            result_id, result_code, created_at \
         ) VALUES ('act-other', 'notes', 'body-1', 'evals-1', 'res-ok', 'other', '2026-11-12T00:00:00Z')",
    )
    .await;
}

#[tokio::test]
async fn eval_cases_json_must_be_an_array() {
    let store = memory().await;
    seed_body_and_evals(&store).await;
    for (index, cases) in ["'{}'", "'\"x\"'"].iter().enumerate() {
        let sql = format!(
            "INSERT INTO skill_eval_result ( \
                result_id, body_revision_id, evals_revision_id, segment_digest, \
                renderer_version, cases_json, created_at \
             ) VALUES ('bad-cases-{index}', 'body-1', 'evals-1', 'seg', 'r1', {cases}, '2026-11-12T00:00:00Z')"
        );
        reject(&store, &sql).await;
    }
}

#[tokio::test]
async fn activation_seq_is_not_reused_after_deleting_the_newest_row() {
    let store = memory().await;
    seed_body_and_evals(&store).await;
    insert_eval(&store, "res-ok").await;
    for activation_id in ["act-1", "act-2"] {
        sqlx::query(
            "INSERT INTO skill_activation ( \
                activation_id, skill_id, body_revision_id, evals_revision_id, \
                result_id, result_code, created_at \
             ) VALUES (?, 'notes', 'body-1', 'evals-1', 'res-ok', 'activated', '2026-11-12T00:00:00Z')",
        )
        .bind(activation_id)
        .execute(pool(&store))
        .await
        .unwrap();
    }
    let newest: i64 = sqlx::query_scalar("SELECT MAX(seq) FROM skill_activation")
        .fetch_one(pool(&store))
        .await
        .unwrap();
    sqlx::query("DELETE FROM skill_activation WHERE seq = ?")
        .bind(newest)
        .execute(pool(&store))
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO skill_activation ( \
            activation_id, skill_id, body_revision_id, evals_revision_id, \
            result_id, result_code, created_at \
         ) VALUES ('act-3', 'notes', 'body-1', 'evals-1', 'res-ok', 'rolled_back', '2026-11-12T00:00:00Z')",
    )
    .execute(pool(&store))
    .await
    .unwrap();
    let next: i64 =
        sqlx::query_scalar("SELECT seq FROM skill_activation WHERE activation_id = 'act-3'")
            .fetch_one(pool(&store))
            .await
            .unwrap();
    assert!(next > newest, "seq {next} reused {newest}");
}
