#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::state::AppState;

const DOC: &str = "doc_01K74Z3QJ8V5N2W9RTX6YB4M00";
const JOB: &str = "job_01K75A0B1C2D3E4F5G6H7J8K01";

/// 0007: an acceptance is written once and kept; what it names is bounded.
#[tokio::test]
async fn acceptances_are_append_only_and_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::open(dir.path()).await;
    let storage = state.storage().await.unwrap();
    let pool = storage.db.pool();
    for sql in [
        format!(
            "INSERT INTO documents (id, title, media_type, head_revision) VALUES ('{DOC}', 't', 'application/pdf', 1)"
        ),
        format!(
            "INSERT INTO revisions (document_id, revision, content_sha256, size, media_type, origin)
             VALUES ('{DOC}', 1, 'sha256:{}', 1, 'application/pdf', 'import')",
            "0".repeat(64)
        ),
        format!(
            "INSERT INTO jobs (id, kind, document_id, revision, status, attempt, origin)
             VALUES ('{JOB}', 'extract', '{DOC}', 1, 'queued', 1, '{{\"kind\":\"page\"}}')"
        ),
    ] {
        sqlx::query(&sql).execute(pool).await.unwrap();
    }
    let run = Run {
        run_id: "run-1".into(),
        tool_call_id: "call-1".into(),
    };
    let accepted = Accepted {
        operation: "extract".into(),
        authorization_ref: Some("ga1:x".into()),
    };
    let mut conn = pool.acquire().await.unwrap();
    record(&mut conn, JOB, 1, &run, &accepted).await.unwrap();
    for sql in [
        "UPDATE job_acceptances SET operation = 'find'",
        "DELETE FROM job_acceptances",
    ] {
        let e = sqlx::query(sql).execute(&mut *conn).await.unwrap_err();
        assert!(e.to_string().contains("append-only"), "{e}");
    }
    for (operation, grant) in [
        ("x".repeat(65), Some("ga1:x".to_owned())),
        (String::new(), None),
        ("extract".to_owned(), Some("g".repeat(257))),
        ("extract".to_owned(), Some(String::new())),
    ] {
        let accepted = Accepted {
            operation,
            authorization_ref: grant,
        };
        assert!(record(&mut conn, JOB, 1, &run, &accepted).await.is_err());
    }
}
