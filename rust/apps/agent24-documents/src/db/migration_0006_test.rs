//! 0006_extractions: an extraction is written once from a text layer of its
//! own revision, its values are sealed when its job succeeds, and a
//! succeeded extract job always names one of its own.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use sqlx::Connection;

use super::*;

const DOC: &str = "doc_01K74Z3QJ8V5N2W9RTX6YB4M00";
const DOC2: &str = "doc_01K74Z3QJ8V5N2W9RTX6YB4M01";
const EXT: &str = "ext_01K75A0B1C2D3E4F5G6H7J8K9M";
const EXT2: &str = "ext_01K75A0B1C2D3E4F5G6H7J8K9N";
const EXT3: &str = "ext_01K75A0B1C2D3E4F5G6H7J8K9P";
const JOB: &str = "job_01K75A0B1C2D3E4F5G6H7J8K9M";
const R1: &str = "sha256:0000000000000000000000000000000000000000000000000000000000000000";
const R2: &str = "sha256:0000000000000000000000000000000000000000000000000000000000000002";
const LAYER1: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
const LAYER2: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111112";
const SCHEMA: &str = "sha256:2222222222222222222222222222222222222222222222222222222222222222";

async fn exec(db: &Db, sql: &str) -> Result<(), sqlx::Error> {
    sqlx::query(sql).execute(db.pool()).await.map(|_| ())
}

async fn refused(db: &Db, sql: &str, why: &str) {
    let err = exec(db, sql).await.unwrap_err().to_string();
    assert!(err.contains(why), "{sql}: {err}");
}

/// Two documents; DOC has r1 and r2 of different bytes, each with a text
/// layer; DOC2 has r1 of r1's bytes.
async fn db() -> (tempfile::TempDir, Db) {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path()).await.unwrap();
    let revision = |doc: &str, n: i64, content: &str, origin: &str| {
        format!(
            "INSERT INTO revisions (document_id, revision, content_sha256, size, media_type, origin) VALUES ('{doc}', {n}, '{content}', 1, 'application/pdf', '{origin}')"
        )
    };
    let layer = |layer: &str, content: &str| {
        format!(
            "INSERT INTO text_layers (text_layer_sha256, content_sha256, engine_id, engine_version, config_sha256) VALUES ('{layer}', '{content}', 'apple-pdfkit', '26.6.2', '{SCHEMA}')"
        )
    };
    for sql in [
        format!(
            "INSERT INTO documents (id, title, media_type, head_revision) VALUES ('{DOC}', 't', 'application/pdf', 1)"
        ),
        format!(
            "INSERT INTO documents (id, title, media_type, head_revision) VALUES ('{DOC2}', 't', 'application/pdf', 1)"
        ),
        revision(DOC, 1, R1, "import"),
        revision(DOC, 2, R2, "commit"),
        revision(DOC2, 1, R1, "import"),
        layer(LAYER1, R1),
        layer(LAYER2, R2),
    ] {
        exec(&db, &sql).await.unwrap();
    }
    (dir, db)
}

fn extraction(id: &str, doc: &str, revision: i64, layer: &str) -> String {
    format!(
        "INSERT INTO extractions (id, document_id, revision, schema_sha256, extractor_version, model_id, text_layer_sha256)
         VALUES ('{id}', '{doc}', {revision}, '{SCHEMA}', '1', 'm', '{layer}')"
    )
}

fn value(ext: &str, ord: i64, json: &str) -> String {
    format!(
        "INSERT INTO extraction_values (extraction_id, ord, value) VALUES ('{ext}', {ord}, '{json}')"
    )
}

/// The extraction insert with one column's SQL expression swapped in.
fn extraction_with(column: &str, expr: &str) -> String {
    let mut cols = [
        ("id", format!("'{EXT}'")),
        ("document_id", format!("'{DOC}'")),
        ("revision", "1".to_owned()),
        ("schema_sha256", format!("'{SCHEMA}'")),
        ("extractor_version", "'1'".to_owned()),
        ("model_id", "'m'".to_owned()),
        ("text_layer_sha256", format!("'{LAYER1}'")),
    ];
    for c in &mut cols {
        if c.0 == column {
            c.1 = expr.to_owned();
        }
    }
    let names: Vec<&str> = cols.iter().map(|c| c.0).collect();
    let exprs: Vec<&str> = cols.iter().map(|c| c.1.as_str()).collect();
    format!(
        "INSERT INTO extractions ({}) VALUES ({})",
        names.join(", "),
        exprs.join(", ")
    )
}

#[tokio::test]
async fn an_extraction_names_what_it_was_made_from() {
    let (_dir, db) = db().await;
    let nul = |s: &str| format!("'{s}' || char(0) || 'x'");
    for (column, expr, why) in [
        // No such layer or revision: no bytes to match.
        (
            "text_layer_sha256",
            format!("'sha256:{}'", "3".repeat(64)),
            "its own revision",
        ),
        ("revision", "3".to_owned(), "its own revision"),
        ("id", "'ext_x'".to_owned(), "CHECK"),
        // length() stops at a NUL: each would pass a check on the text.
        ("id", nul(EXT), "CHECK"),
        ("schema_sha256", nul(SCHEMA), "CHECK"),
        (
            "extractor_version",
            format!("'1' || char(0) || '{}'", "9".repeat(1000)),
            "CHECK",
        ),
        ("model_id", nul("m"), "CHECK"),
        ("extractor_version", "''".to_owned(), "CHECK"),
        (
            "extractor_version",
            format!("'{}'", "9".repeat(65)),
            "CHECK",
        ),
        ("model_id", "''".to_owned(), "CHECK"),
        // 257 bytes: the kernel names a model in at most 256.
        ("model_id", format!("'{}'", "模".repeat(86)), "CHECK"),
        // r2's layer for r1: anchors would claim bytes they were not read from.
        (
            "text_layer_sha256",
            format!("'{LAYER2}'"),
            "its own revision",
        ),
    ] {
        refused(&db, &extraction_with(column, &expr), why).await;
    }
    refused(&db, &extraction(EXT, DOC, 2, LAYER1), "its own revision").await;
    exec(
        &db,
        &extraction_with("model_id", &format!("'{}'", "m".repeat(256))),
    )
    .await
    .unwrap();
    exec(&db, &extraction(EXT2, DOC, 2, LAYER2)).await.unwrap();
    // Another document of the same bytes reads the same layer.
    exec(&db, &extraction(EXT3, DOC2, 1, LAYER1)).await.unwrap();
}

#[tokio::test]
async fn values_are_json_objects_in_field_order() {
    let (_dir, db) = db().await;
    exec(&db, &extraction(EXT, DOC, 1, LAYER1)).await.unwrap();
    for sql in [
        value(EXT, 0, "[1]"),
        value(EXT, 0, "not json"),
        value(EXT, -1, "{}"),
        value(EXT, 100, "{}"),
        format!(
            "INSERT INTO extraction_values (extraction_id, ord, value) VALUES ('{EXT}', 0, '{{}}' || char(0) || 'x')"
        ),
        value(EXT2, 0, "{}"),
    ] {
        assert!(exec(&db, &sql).await.is_err(), "{sql}");
    }
    exec(&db, &value(EXT, 0, "{}")).await.unwrap();
    exec(&db, &value(EXT, 99, "{}")).await.unwrap();
}

#[tokio::test]
async fn extractions_and_their_values_never_change() {
    let (dir, db) = db().await;
    exec(&db, &extraction(EXT, DOC, 1, LAYER1)).await.unwrap();
    exec(&db, &extraction(EXT2, DOC, 2, LAYER2)).await.unwrap();
    exec(&db, &value(EXT, 0, "{\"key\":\"a\"}")).await.unwrap();
    exec(&db, &value(EXT, 1, "{\"key\":\"b\"}")).await.unwrap();
    for (sql, why) in [
        ("UPDATE extractions SET model_id = 'x'", "immutable"),
        ("UPDATE extractions SET created_at = 'x'", "immutable"),
        ("DELETE FROM extractions", "never deleted"),
        ("UPDATE extraction_values SET value = '{}'", "immutable"),
        ("UPDATE extraction_values SET ord = ord + 10", "immutable"),
        ("DELETE FROM extraction_values", "never deleted"),
        // The layer an extraction names stays (0001 pins every layer).
        ("DELETE FROM text_layers", "pinned"),
    ] {
        refused(&db, sql, why).await;
    }
    let rowid = |table: &str, key: &str| format!("(SELECT rowid FROM {table} WHERE {key})");
    let replaces = [
        extraction(EXT, DOC, 1, LAYER1).replace("INSERT", "INSERT OR REPLACE"),
        value(EXT, 0, "{}").replace("INSERT", "INSERT OR REPLACE"),
        // A new key on an existing row's rowid.
        format!(
            "INSERT OR REPLACE INTO extractions (rowid, id, document_id, revision, schema_sha256, extractor_version, model_id, text_layer_sha256)
             VALUES ({}, '{EXT3}', '{DOC}', 1, '{SCHEMA}', '1', 'm', '{LAYER1}')",
            rowid("extractions", &format!("id = '{EXT}'"))
        ),
        format!(
            "INSERT OR REPLACE INTO extraction_values (rowid, extraction_id, ord, value) VALUES ({}, '{EXT}', 5, '{{}}')",
            rowid("extraction_values", "ord = 0")
        ),
    ];
    for sql in &replaces {
        refused(&db, sql, "never replaced").await;
    }
    // Without recursive_triggers REPLACE skips delete triggers; the insert
    // guards still hold, on the key and on the rowid.
    let mut raw = sqlx::SqliteConnection::connect_with(
        &SqliteConnectOptions::new().filename(dir.path().join(DB_FILE)),
    )
    .await
    .unwrap();
    for sql in &replaces {
        let err = sqlx::query(sql).execute(&mut raw).await.unwrap_err();
        assert!(err.to_string().contains("never replaced"), "{sql}: {err}");
    }
    for sql in [
        format!(
            "INSERT INTO extractions (rowid, id, document_id, revision, schema_sha256, extractor_version, model_id, text_layer_sha256)
             VALUES (-5, '{EXT3}', '{DOC}', 1, '{SCHEMA}', '1', 'm', '{LAYER1}')"
        ),
        format!("INSERT INTO extraction_values (rowid, extraction_id, ord, value) VALUES (0, '{EXT}', 7, '{{}}')"),
    ] {
        refused(&db, &sql, "rowids are positive").await;
    }
    let kept: Vec<(String, String)> = sqlx::query_as(
        "SELECT extraction_id || ':' || ord, value FROM extraction_values ORDER BY rowid",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(
        kept,
        [
            (format!("{EXT}:0"), "{\"key\":\"a\"}".to_owned()),
            (format!("{EXT}:1"), "{\"key\":\"b\"}".to_owned())
        ]
    );
}

#[tokio::test]
async fn a_succeeded_extract_job_names_its_own_extraction_and_seals_it() {
    let (_dir, db) = db().await;
    exec(&db, &extraction(EXT, DOC, 1, LAYER1)).await.unwrap();
    exec(&db, &extraction(EXT2, DOC, 2, LAYER2)).await.unwrap();
    // DOC2's r1, of the same bytes and layer as DOC's r1.
    exec(&db, &extraction(EXT3, DOC2, 1, LAYER1)).await.unwrap();
    exec(&db, &value(EXT, 0, "{}")).await.unwrap();
    let job = |status: &str, result: &str| {
        format!(
            "INSERT INTO jobs (id, kind, document_id, revision, status, result_ref, origin)
             VALUES ('{JOB}', 'extract', '{DOC}', 1, '{status}', {result}, '{{\"kind\":\"page\"}}')"
        )
    };
    for result in [
        "'ext_01K75A0B1C2D3E4F5G6H7J8K9Q'".to_owned(),
        format!("'{EXT2}'"),
        format!("'{EXT3}'"),
        "NULL".to_owned(),
    ] {
        refused(&db, &job("succeeded", &result), "names its extraction").await;
    }
    exec(&db, &job("running", "NULL")).await.unwrap();
    for set in [
        "status = 'succeeded'".to_owned(),
        format!("status = 'succeeded', result_ref = '{EXT2}'"),
        format!("status = 'succeeded', result_ref = '{EXT3}'"),
    ] {
        refused(
            &db,
            &format!("UPDATE jobs SET {set}"),
            "names its extraction",
        )
        .await;
    }
    exec(
        &db,
        &format!("UPDATE jobs SET status = 'succeeded', result_ref = '{EXT}'"),
    )
    .await
    .unwrap();
    // Once succeeded, it cannot be pointed or moved elsewhere by any column.
    for set in [
        format!("result_ref = '{EXT2}'"),
        "revision = 2".to_owned(),
        format!("document_id = '{DOC2}'"),
        "document_id = NULL, revision = NULL".to_owned(),
    ] {
        refused(&db, &format!("UPDATE jobs SET {set}"), "is final").await;
    }
    // It is final: no way back to running, to another extraction, or
    // elsewhere with its result, which would lift the seal below.
    for set in [
        "status = 'running'".to_owned(),
        "status = 'failed', error = '{\"code\":\"internal\"}'".to_owned(),
        format!("result_ref = '{EXT3}', document_id = '{DOC2}'"),
    ] {
        refused(&db, &format!("UPDATE jobs SET {set}"), "is final").await;
    }
    exec(&db, "UPDATE jobs SET updated_at = 'later'")
        .await
        .unwrap();
    // Its values are sealed: none can be added after publication.
    for ord in [1, 99] {
        refused(&db, &value(EXT, ord, "{}"), "sealed").await;
    }
    // A job born succeeded, with its own extraction, is fine; other kinds are untouched.
    exec(&db, &format!(
        "INSERT INTO jobs (id, kind, document_id, revision, status, result_ref, origin)
         VALUES ('job_01K75A0B1C2D3E4F5G6H7J8K9N', 'extract', '{DOC2}', 1, 'succeeded', '{EXT3}', '{{\"kind\":\"page\"}}')"
    )).await.unwrap();
    exec(&db, "INSERT INTO jobs (id, kind, status, origin) VALUES ('job_01K75A0B1C2D3E4F5G6H7J8K9P', 'import', 'succeeded', '{\"kind\":\"page\"}')")
        .await
        .unwrap();
}
