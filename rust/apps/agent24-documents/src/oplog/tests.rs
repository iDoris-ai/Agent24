#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::extract::job::tests::{DOC, env};

type Line = (
    String,
    Option<String>,
    Option<i64>,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
);

async fn lines(pool: &sqlx::SqlitePool) -> Vec<Line> {
    sqlx::query_as(
        "SELECT op, document_id, revision, origin, run_id, tool_call_id, details
         FROM oplog ORDER BY seq",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

fn run() -> Run {
    Run {
        run_id: "run_1".into(),
        tool_call_id: "call_1".into(),
    }
}

#[tokio::test]
async fn an_extract_a_run_submits_is_logged_with_the_run() {
    let env = env(None).await;
    let pool = env.storage.db.pool();
    let mut tx = pool.begin().await.unwrap();
    extract_submitted(&mut tx, &run(), DOC, 1, "job_x")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let rows = lines(pool).await;
    assert_eq!(rows.len(), 1);
    let (op, doc, rev, origin, run_id, call, details) = rows[0].clone();
    assert_eq!(
        (op.as_str(), doc.as_deref(), rev, origin.as_str()),
        ("extract", Some(DOC), Some(1), "run")
    );
    assert_eq!(
        (run_id.as_deref(), call.as_deref()),
        (Some("run_1"), Some("call_1"))
    );
    let details: Value = serde_json::from_str(&details.unwrap()).unwrap();
    assert_eq!(details, json!({ "job_id": "job_x" }));
}

#[tokio::test]
async fn the_page_is_logged_without_run_ids_and_a_run_needs_both() {
    let env = env(None).await;
    let pool = env.storage.db.pool();
    let mut conn = pool.acquire().await.unwrap();
    append(&mut conn, "x", DOC, 1, &Origin::Page, &json!({}))
        .await
        .unwrap();
    let rows = lines(pool).await;
    assert_eq!(
        (rows[0].3.as_str(), &rows[0].4, &rows[0].5),
        ("page", &None, &None)
    );
    // The table, not this function, holds the rule: a run without a call
    // id is refused, and nothing is written.
    for bad in [
        Run {
            run_id: String::new(),
            tool_call_id: "c".into(),
        },
        Run {
            run_id: "r".into(),
            tool_call_id: String::new(),
        },
    ] {
        let mut conn = pool.acquire().await.unwrap();
        assert!(
            extract_submitted(&mut conn, &bad, DOC, 1, "job_x")
                .await
                .is_err()
        );
    }
    assert_eq!(lines(pool).await.len(), 1);
}

#[test]
fn a_job_origin_is_the_json_the_table_checks() {
    let page: Value = serde_json::from_str(&Origin::Page.job_json()).unwrap();
    assert_eq!(page, json!({ "kind": "page" }));
    let run: Value = serde_json::from_str(&Origin::Run(run()).job_json()).unwrap();
    assert_eq!(
        run,
        json!({ "kind": "run", "run_id": "run_1", "tool_call_id": "call_1" })
    );
}
