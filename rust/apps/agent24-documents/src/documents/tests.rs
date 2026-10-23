#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::error::StorageCause;
use crate::state::AppState;

const SHA: &str = "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

struct Env {
    _dir: tempfile::TempDir,
    state: AppState,
}

async fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::open(dir.path()).await;
    Env { _dir: dir, state }
}

async fn exec(state: &AppState, sql: &str) {
    let storage = state.storage().await.unwrap();
    sqlx::raw_sql(sql).execute(storage.db.pool()).await.unwrap();
}

/// A document and its r1, created at `at` (the 0002/0003 guards want both
/// in one transaction).
async fn add(state: &AppState, n: u32, at: &str) -> String {
    let id = format!("doc_01K74Z3QJ8V5N2W9RTX6YB4M{n:02}");
    exec(
        state,
        &format!(
            "BEGIN;
             INSERT INTO documents (id, title, media_type, head_revision, created_at, updated_at)
                    VALUES ('{id}', '通告 {n}', 'application/pdf', 1, '{at}', '{at}');
             INSERT INTO revisions (document_id, revision, content_sha256, size, media_type, origin)
                    VALUES ('{id}', 1, '{SHA}', {n}, 'application/pdf', 'import');
             COMMIT;"
        ),
    )
    .await;
    id
}

/// A document with any SQL for its id and title, with or without its r1.
async fn add_raw(state: &AppState, id_sql: &str, title_sql: &str, at: &str, with_r1: bool) {
    let r1 = if with_r1 {
        format!(
            "INSERT INTO revisions (document_id, revision, content_sha256, size, media_type, origin)
                    VALUES ({id_sql}, 1, '{SHA}', 1, 'application/pdf', 'import');"
        )
    } else {
        String::new()
    };
    exec(
        state,
        &format!(
            "BEGIN;
             INSERT INTO documents (id, title, media_type, head_revision, created_at, updated_at)
                    VALUES ({id_sql}, {title_sql}, 'application/pdf', 1, '{at}', '{at}');
             {r1}
             COMMIT;"
        ),
    )
    .await;
}

fn doc_id(n: u32) -> String {
    format!("doc_01K74Z3QJ8V5N2W9RTX6Y{n:05}")
}

/// Every document, a page of `limit` at a time.
async fn walk(state: &AppState, limit: u32) -> Vec<String> {
    let mut all = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let path = match &cursor {
            Some(c) => format!("/documents?limit={limit}&cursor={c}"),
            None => format!("/documents?limit={limit}"),
        };
        let (status, v) = get(state, &path).await;
        assert_eq!(status, StatusCode::OK, "{v}");
        all.extend(ids(&v));
        match v["next_cursor"].as_str() {
            Some(c) => cursor = Some(c.to_owned()),
            None => return all,
        }
    }
}

async fn get(state: &AppState, path: &str) -> (StatusCode, Value) {
    let res = crate::router(state.clone())
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

fn ids(v: &Value) -> Vec<String> {
    v["documents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["document_id"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn an_empty_store_lists_no_documents() {
    let env = env().await;
    let (status, v) = get(&env.state, "/documents").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v, json!({ "documents": [], "next_cursor": null }));
}

#[tokio::test]
async fn documents_list_newest_first_in_pages() {
    let env = env().await;
    let mut made = Vec::new();
    for n in 1..=5 {
        made.push(add(&env.state, n, &format!("2026-10-0{n}T00:00:00.000Z")).await);
    }
    // Two created in the same millisecond: the id breaks the tie.
    made.push(add(&env.state, 6, "2026-10-05T00:00:00.000Z").await);
    // 6 and 5 share a timestamp: the larger id comes first.
    let newest_first: Vec<String> = [5, 4, 3, 2, 1, 0]
        .iter()
        .map(|&i| made[i].clone())
        .collect();
    let (status, all) = get(&env.state, "/documents").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ids(&all), newest_first);
    assert_eq!(all["next_cursor"], Value::Null);
    assert_eq!(
        all["documents"][0],
        json!({
            "document_id": newest_first[0],
            "title": "通告 6",
            "media_type": "application/pdf",
            "head_revision": 1,
            "created_at": "2026-10-05T00:00:00.000Z",
            "updated_at": "2026-10-05T00:00:00.000Z",
        })
    );
    // Pages of 4 then 2: the same order, nothing repeated or skipped.
    let (_, first) = get(&env.state, "/documents?limit=4").await;
    let cursor = first["next_cursor"].as_str().unwrap().to_owned();
    let (_, second) = get(&env.state, &format!("/documents?limit=4&cursor={cursor}")).await;
    assert_eq!(second["next_cursor"], Value::Null);
    let mut paged = ids(&first);
    paged.extend(ids(&second));
    assert_eq!(paged, newest_first);
    // A page that ends exactly at the last document says there is no more.
    let (_, exact) = get(&env.state, "/documents?limit=6").await;
    assert_eq!(exact["next_cursor"], Value::Null);
}

#[tokio::test]
async fn bad_limits_and_cursors_are_400() {
    let env = env().await;
    for query in [
        "limit=0",
        "limit=201",
        "limit=x",
        "limit=1.5",
        "cursor=zz",
        "cursor=abc",
        // Hex of a well-formed pair, but not a timestamp and an id.
        "cursor=6e6f7420612063757273",
    ] {
        let (status, v) = get(&env.state, &format!("/documents?{query}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}: {v}");
        assert_eq!(v["error"]["code"], "invalid_request", "{query}");
    }
    let (status, _) = get(&env.state, "/documents?limit=200").await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn a_document_is_read_with_its_head_revision() {
    let env = env().await;
    let id = add(&env.state, 7, "2026-10-08T09:00:00.000Z").await;
    let (status, v) = get(&env.state, &format!("/documents/{id}")).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(
        v,
        json!({
            "document_id": id,
            "title": "通告 7",
            "media_type": "application/pdf",
            "head_revision": 1,
            "created_at": "2026-10-08T09:00:00.000Z",
            "updated_at": "2026-10-08T09:00:00.000Z",
            "head": { "revision": 1, "content_sha256": SHA, "size": 7, "media_type": "application/pdf" },
        })
    );
    let (status, v) = get(&env.state, "/documents/doc_01K74Z3QJ8V5N2W9RTX6YB4M99").await;
    assert_eq!(
        (status, v["error"]["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("not_found"))
    );
    let (status, v) = get(&env.state, "/documents/doc_x").await;
    assert_eq!(
        (status, v["error"]["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_request"))
    );
}

#[tokio::test]
async fn a_row_the_contract_does_not_allow_is_never_sent() {
    for change in [
        "UPDATE documents SET media_type = 'Application/PDF'",
        "UPDATE documents SET updated_at = 'not-a-date'",
    ] {
        let env = env().await;
        let id = add(&env.state, 1, "2026-10-01T00:00:00.000Z").await;
        exec(&env.state, change).await;
        for path in ["/documents".to_owned(), format!("/documents/{id}")] {
            let (status, v) = get(&env.state, &path).await;
            assert_eq!(
                status,
                StatusCode::INTERNAL_SERVER_ERROR,
                "{change} {path}: {v}"
            );
            assert_eq!(v["error"]["code"], "internal");
        }
    }
}

#[tokio::test]
async fn unavailable_storage_is_503() {
    let state = AppState::unavailable(StorageCause::Locked);
    for path in ["/documents", "/documents/doc_01K74Z3QJ8V5N2W9RTX6YB4M01"] {
        let (status, v) = get(&state, path).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{path}");
        assert_eq!(v["error"]["details"]["cause"], "locked");
    }
}

#[tokio::test]
async fn one_at_a_time_a_tie_across_pages_is_neither_skipped_nor_repeated() {
    let env = env().await;
    let mut made = Vec::new();
    for n in 1..=5 {
        made.push(add(&env.state, n, &format!("2026-10-0{n}T00:00:00.000Z")).await);
    }
    made.push(add(&env.state, 6, "2026-10-05T00:00:00.000Z").await);
    let expected: Vec<String> = [5, 4, 3, 2, 1, 0]
        .iter()
        .map(|&i| made[i].clone())
        .collect();
    assert_eq!(walk(&env.state, 1).await, expected);
}

#[tokio::test]
async fn paging_is_live_newer_documents_wait_older_ones_join() {
    let env = env().await;
    for n in 1..=4 {
        add(&env.state, n, &format!("2026-10-0{n}T00:00:00.000Z")).await;
    }
    let (_, first) = get(&env.state, "/documents?limit=2").await;
    let cursor = first["next_cursor"].as_str().unwrap().to_owned();
    // Imported after the first page: one newer than every row, one older
    // than the cursor (a clock that went back).
    let newer = add(&env.state, 8, "2026-10-09T00:00:00.000Z").await;
    let older = add(&env.state, 9, "2026-09-30T00:00:00.000Z").await;
    let (_, rest) = get(&env.state, &format!("/documents?limit=10&cursor={cursor}")).await;
    let rest = ids(&rest);
    assert!(!rest.contains(&newer), "{rest:?}");
    assert_eq!(
        rest.iter().filter(|id| **id == older).count(),
        1,
        "{rest:?}"
    );
    assert_eq!(rest.len(), 3);
}

#[tokio::test]
async fn the_default_page_is_50() {
    let env = env().await;
    for n in 0..55 {
        add_raw(
            &env.state,
            &format!("'{}'", doc_id(n)),
            "'t'",
            "2026-10-01T00:00:00.000Z",
            true,
        )
        .await;
    }
    let (_, v) = get(&env.state, "/documents").await;
    assert_eq!(ids(&v).len(), 50);
    assert!(v["next_cursor"].is_string());
}

#[tokio::test]
async fn a_repeated_query_parameter_is_400() {
    let env = env().await;
    for n in 1..=2 {
        add(&env.state, n, &format!("2026-10-0{n}T00:00:00.000Z")).await;
    }
    // A cursor the OS issued: fine alone, refused when repeated.
    let (_, first) = get(&env.state, "/documents?limit=1").await;
    let cursor = first["next_cursor"].as_str().unwrap().to_owned();
    let (status, _) = get(&env.state, &format!("/documents?cursor={cursor}")).await;
    assert_eq!(status, StatusCode::OK);
    for query in [
        "limit=1&limit=2".to_owned(),
        format!("cursor={cursor}&cursor={cursor}"),
    ] {
        let (status, v) = get(&env.state, &format!("/documents?{query}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}: {v}");
        assert_eq!(v["error"]["code"], "invalid_request");
    }
    // A cursor of any other length is refused before it is decoded.
    let (status, _) = get(
        &env.state,
        &format!("/documents?cursor={}", "30".repeat(5000)),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_page_stays_under_512_kib_however_long_its_titles() {
    let env = env().await;
    // 500 NULs: a title the import allows, six JSON bytes per character.
    for n in 0..200 {
        let title = "CAST(zeroblob(500) AS TEXT)";
        add_raw(
            &env.state,
            &format!("'{}'", doc_id(n)),
            title,
            "2026-10-01T00:00:00.000Z",
            true,
        )
        .await;
    }
    let res = crate::router(env.state.clone())
        .oneshot(
            Request::get("/documents?limit=200")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = axum::body::to_bytes(res.into_body(), 4 << 20)
        .await
        .unwrap();
    assert!(bytes.len() <= 512 * 1024, "{} bytes", bytes.len());
    let page: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(ids(&page).len() < 200);
    assert!(page["next_cursor"].is_string());
    let all = walk(&env.state, 200).await;
    let distinct: std::collections::HashSet<_> = all.iter().collect();
    assert_eq!((all.len(), distinct.len()), (200, 200));
}

#[tokio::test]
async fn the_detail_shows_the_current_head_and_a_missing_head_is_a_bug() {
    let env = env().await;
    let id = add(&env.state, 1, "2026-10-01T00:00:00.000Z").await;
    let sha2 = format!("sha256:{}", "a".repeat(64));
    exec(
        &env.state,
        &format!(
            "BEGIN;
             INSERT INTO revisions (document_id, revision, content_sha256, size, media_type, origin)
                    VALUES ('{id}', 2, '{sha2}', 22, 'application/pdf', 'commit');
             UPDATE documents SET head_revision = 2 WHERE id = '{id}';
             COMMIT;"
        ),
    )
    .await;
    let (status, v) = get(&env.state, &format!("/documents/{id}")).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(
        (v["head_revision"].as_i64(), v["head"]["revision"].as_i64()),
        (Some(2), Some(2))
    );
    assert_eq!(
        (
            v["head"]["content_sha256"].as_str(),
            v["head"]["size"].as_i64()
        ),
        (Some(sha2.as_str()), Some(22))
    );
    // A document committed without its r1 exists, so it is not a 404.
    let lone = doc_id(7);
    add_raw(
        &env.state,
        &format!("'{lone}'"),
        "'t'",
        "2026-10-02T00:00:00.000Z",
        false,
    )
    .await;
    let (status, v) = get(&env.state, &format!("/documents/{lone}")).await;
    assert_eq!(
        (status, v["error"]["code"].as_str()),
        (StatusCode::INTERNAL_SERVER_ERROR, Some("internal"))
    );
}

#[tokio::test]
async fn each_row_guard_refuses_on_its_own() {
    let at = "2026-10-01T00:00:00.000Z";
    let good = doc_id(1);
    // (id SQL, title SQL, created_at, the path that must refuse it)
    let cases = [
        // 0001's CHECK reads the id only up to a NUL.
        (
            format!("'{good}' || char(0) || 'x'"),
            "'t'".to_owned(),
            at,
            "/documents".to_owned(),
        ),
        (
            format!("'{good}'"),
            format!("'{}'", "x".repeat(501)),
            at,
            "/documents".to_owned(),
        ),
        (
            format!("'{good}'"),
            "'t'".to_owned(),
            "2026-02-30T00:00:00.000Z",
            format!("/documents/{good}"),
        ),
    ];
    for (id_sql, title_sql, created, path) in cases {
        let env = env().await;
        add_raw(&env.state, &id_sql, &title_sql, created, true).await;
        // Only created_at is wrong in the last case: updated_at stays valid.
        exec(
            &env.state,
            &format!("UPDATE documents SET updated_at = '{at}'"),
        )
        .await;
        let (status, v) = get(&env.state, &path).await;
        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "{id_sql} {title_sql} {created}: {v}"
        );
    }
    // The head's hash, past a NUL that 0001's CHECK cannot see, and its type.
    for (sha_sql, media) in [
        (format!("'{SHA}' || char(0) || 'x'"), "application/pdf"),
        (format!("'{SHA}'"), "PDF"),
    ] {
        let env = env().await;
        exec(
            &env.state,
            &format!(
                "BEGIN;
                 INSERT INTO documents (id, title, media_type, head_revision) VALUES ('{good}', 't', 'application/pdf', 1);
                 INSERT INTO revisions (document_id, revision, content_sha256, size, media_type, origin)
                        VALUES ('{good}', 1, {sha_sql}, 1, '{media}', 'import');
                 COMMIT;"
            ),
        )
        .await;
        let (status, v) = get(&env.state, &format!("/documents/{good}")).await;
        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "{sha_sql} {media}: {v}"
        );
    }
}
