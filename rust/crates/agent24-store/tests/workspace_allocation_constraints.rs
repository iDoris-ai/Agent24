#![allow(clippy::unwrap_used, clippy::type_complexity)]

use agent24_store::{Store, test_hooks};

const E8: &[u8] = &[1; 8];
const E16: &[u8] = &[2; 16];
const NOW: &str = "2026-09-19T00:00:00.000Z";

#[derive(Clone, Copy)]
struct Root {
    kind: Option<&'static str>,
    ud: Option<&'static [u8]>,
    ui: Option<&'static [u8]>,
    wv: Option<&'static [u8]>,
    wf: Option<&'static [u8]>,
}

const NONE: Root = Root {
    kind: None,
    ud: None,
    ui: None,
    wv: None,
    wf: None,
};
const UNIX: Root = Root {
    kind: Some("unix"),
    ud: Some(E8),
    ui: Some(E8),
    wv: None,
    wf: None,
};
const WINDOWS: Root = Root {
    kind: Some("windows"),
    ud: None,
    ui: None,
    wv: Some(E8),
    wf: Some(E16),
};

async fn insert(store: &Store, n: usize, phase: &str, root: Root, failure: Option<&str>) -> bool {
    sqlx::query(
        "INSERT INTO workspace_allocations
        (allocation_id,workspace_id,root_generation,relative_name,parent_identity_kind,
         parent_unix_device,parent_unix_inode,root_identity_kind,root_unix_device,
         root_unix_inode,root_windows_volume,root_windows_file_id,phase,created_at,failure_reason)
        VALUES (?,?,'g1',?,'unix',X'0101010101010101',X'0101010101010101',?,?,?,?,?,?,?,?)",
    )
    .bind(format!("wa_01J5M4Q2Y7N8P9R0S1T2V3W4X{n:X}"))
    .bind(format!("ws_01J5M4Q2Y7N8P9R0S1T2V3W4X{n:X}"))
    .bind(format!("root-{n}"))
    .bind(root.kind)
    .bind(root.ud)
    .bind(root.ui)
    .bind(root.wv)
    .bind(root.wf)
    .bind(phase)
    .bind(NOW)
    .bind(failure)
    .execute(test_hooks::pool(store))
    .await
    .is_ok()
}

#[allow(clippy::too_many_arguments)]
async fn custom(
    store: &Store,
    allocation: &str,
    workspace: &str,
    name: &str,
    parent: Root,
    root: Root,
    phase: &str,
    created: &str,
    failure: Option<&str>,
) -> bool {
    sqlx::query(
        "INSERT INTO workspace_allocations
        (allocation_id,workspace_id,root_generation,relative_name,parent_identity_kind,
         parent_unix_device,parent_unix_inode,parent_windows_volume,parent_windows_file_id,
         root_identity_kind,root_unix_device,root_unix_inode,root_windows_volume,
         root_windows_file_id,phase,created_at,failure_reason)
        VALUES (?,?,'g1',?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
    )
    .bind(allocation)
    .bind(workspace)
    .bind(name)
    .bind(parent.kind)
    .bind(parent.ud)
    .bind(parent.ui)
    .bind(parent.wv)
    .bind(parent.wf)
    .bind(root.kind)
    .bind(root.ud)
    .bind(root.ui)
    .bind(root.wv)
    .bind(root.wf)
    .bind(phase)
    .bind(created)
    .bind(failure)
    .execute(test_hooks::pool(store))
    .await
    .is_ok()
}

fn ids(n: usize) -> (String, String) {
    let suffix = "0123456789ABCDEFGHJKMNPQRSTVWXYZ"
        .chars()
        .nth(n % 32)
        .unwrap();
    (
        format!("wa_01J5M4Q2Y7N8P9R0S1T2V3W4X{suffix}"),
        format!("ws_01J5M4Q2Y7N8P9R0S1T2V3W4X{suffix}"),
    )
}

#[tokio::test]
async fn allocation_root_identity_and_phase_checks_are_two_valued() {
    let store = Store::open_memory().await.unwrap();
    let null_kind_unix = Root {
        kind: None,
        ud: Some(E8),
        ui: Some(E8),
        ..NONE
    };
    let null_kind_windows = Root {
        kind: None,
        wv: Some(E8),
        wf: Some(E16),
        ..NONE
    };
    let partial = Root {
        kind: Some("unix"),
        ud: Some(E8),
        ..NONE
    };
    let mixed = Root {
        kind: Some("windows"),
        ud: Some(E8),
        ui: Some(E8),
        wv: Some(E8),
        wf: Some(E16),
    };
    let cases = [
        ("reserved", NONE, None, true),
        ("materialized", UNIX, None, true),
        ("committed", WINDOWS, None, true),
        ("materialized", null_kind_unix, None, false),
        ("materialized", null_kind_windows, None, false),
        ("materialized", partial, None, false),
        ("committed", mixed, None, false),
        ("committed", NONE, None, false),
        ("retained", NONE, Some("io_error"), true),
        ("retained", UNIX, Some("io_error"), true),
        ("retained", NONE, None, false),
        ("retained", NONE, Some(""), false),
        ("retained", NONE, Some("/private/path"), false),
    ];
    for (n, (phase, root, failure, expected)) in cases.into_iter().enumerate() {
        assert_eq!(
            insert(&store, n, phase, root, failure).await,
            expected,
            "case {n}"
        );
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM workspaces")
            .fetch_one(test_hooks::pool(&store))
            .await
            .unwrap(),
        0,
        "reserved allocations must be accepted before workspace registration"
    );
    assert!(
        sqlx::query("PRAGMA foreign_key_list('workspace_allocations')")
            .fetch_all(test_hooks::pool(&store))
            .await
            .unwrap()
            .is_empty(),
        "allocation journal must have no foreign keys"
    );
}

#[tokio::test]
async fn allocation_remaining_database_constraints() {
    let store = Store::open_memory().await.unwrap();
    let nul_unix = Root {
        kind: None,
        ud: Some(E8),
        ui: Some(E8),
        ..NONE
    };
    let nul_windows = Root {
        kind: None,
        wv: Some(E8),
        wf: Some(E16),
        ..NONE
    };
    for (n, root) in [(0, nul_unix), (1, nul_windows)] {
        let (a, w) = ids(n);
        assert!(
            !custom(
                &store,
                &a,
                &w,
                "retained-root",
                UNIX,
                root,
                "retained",
                NOW,
                Some("io_error")
            )
            .await
        );
    }

    let bad7: &[u8] = &[3; 7];
    let bad15: &[u8] = &[4; 15];
    let parents = [
        (
            Root {
                kind: Some("unix"),
                ud: Some(E8),
                ..NONE
            },
            false,
        ),
        (
            Root {
                kind: Some("unix"),
                ud: Some(E8),
                ui: Some(E8),
                wv: Some(E8),
                ..NONE
            },
            false,
        ),
        (
            Root {
                kind: Some("unix"),
                ud: Some(bad7),
                ui: Some(E8),
                ..NONE
            },
            false,
        ),
        (
            Root {
                kind: Some("windows"),
                wv: Some(E8),
                wf: Some(bad15),
                ..NONE
            },
            false,
        ),
        (WINDOWS, true),
    ];
    for (n, (parent, expected)) in parents.into_iter().enumerate() {
        let (a, w) = ids(n + 2);
        assert_eq!(
            custom(
                &store, &a, &w, "parent", parent, NONE, "reserved", NOW, None
            )
            .await,
            expected,
            "parent {n}"
        );
    }

    let (a, w) = ids(50);
    assert!(custom(&store, &a, &w, "unique", UNIX, NONE, "reserved", NOW, None).await);
    let (_a2, w2) = ids(51);
    assert!(!custom(&store, &a, &w2, "other", UNIX, NONE, "reserved", NOW, None).await);
    let (a3, _) = ids(52);
    assert!(!custom(&store, &a3, &w, "other", UNIX, NONE, "reserved", NOW, None).await);
    let (a4, w4) = ids(53);
    assert!(
        !custom(
            &store, &a4, &w4, "unique", UNIX, NONE, "reserved", NOW, None
        )
        .await
    );
}

#[tokio::test]
async fn allocation_ids_names_times_and_failure_reasons_are_bounded() {
    let store = Store::open_memory().await.unwrap();
    let valid = [
        (
            "ax_01J5M4Q2Y7N8P9R0S1T2V3W4X0",
            "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X1",
        ),
        (
            "wa_01J5M4Q2Y7N8P9R0S1T2V3W4X",
            "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X2",
        ),
        (
            "wa_01J5M4Q2Y7N8P9R0S1T2V3W4XI",
            "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X3",
        ),
        (
            "wa_01J5M4Q2Y7N8P9R0S1T2V3W4X4",
            "wx_01J5M4Q2Y7N8P9R0S1T2V3W4X5",
        ),
        (
            "wa_01J5M4Q2Y7N8P9R0S1T2V3W4X6",
            "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X",
        ),
        (
            "wa_01J5M4Q2Y7N8P9R0S1T2V3W4X7",
            "ws_01J5M4Q2Y7N8P9R0S1T2V3W4XI",
        ),
    ];
    for (n, (a, w)) in valid.into_iter().enumerate() {
        assert!(
            !custom(
                &store,
                a,
                w,
                "invalid-id",
                UNIX,
                NONE,
                "reserved",
                NOW,
                None
            )
            .await,
            "identifier {n}"
        );
    }

    let names = [
        (".".to_owned(), false),
        ("..".to_owned(), false),
        ("a/b".to_owned(), false),
        ("a\\b".to_owned(), false),
        ("a\0b".to_owned(), false),
        ("".to_owned(), false),
        ("x".to_owned(), true),
        ("x".repeat(255), true),
        ("x".repeat(256), false),
    ];
    for (n, (name, expected)) in names.into_iter().enumerate() {
        let (a, w) = ids(n);
        assert_eq!(
            custom(&store, &a, &w, &name, UNIX, NONE, "reserved", NOW, None).await,
            expected,
            "relative name {n}"
        );
    }

    for (n, date) in [
        "2026-09-19T00:00:00Z",
        "2026-09-19T00:00:00.000+00:00",
        "bad",
    ]
    .into_iter()
    .enumerate()
    {
        let (a, w) = ids(n + 12);
        assert!(
            !custom(
                &store,
                &a,
                &w,
                &format!("date-{n}"),
                UNIX,
                NONE,
                "reserved",
                date,
                None
            )
            .await
        );
    }

    let reasons = [
        ("x".to_owned(), true),
        ("x".repeat(128), true),
        ("x".repeat(129), false),
        ("UPPER".to_owned(), false),
        ("bad\0reason".to_owned(), false),
        ("bad/reason".to_owned(), false),
    ];
    for (n, (reason, expected)) in reasons.into_iter().enumerate() {
        let (a, w) = ids(n + 20);
        assert_eq!(
            custom(
                &store,
                &a,
                &w,
                &format!("reason-{n}"),
                UNIX,
                NONE,
                "retained",
                NOW,
                Some(&reason)
            )
            .await,
            expected,
            "failure reason {n}"
        );
    }
}
