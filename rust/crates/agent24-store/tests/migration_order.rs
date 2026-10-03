#![allow(clippy::unwrap_used, clippy::expect_used)]

use sqlx::migrate::Migrator;
use std::path::Path;

#[tokio::test]
async fn migration_versions_are_unique_and_strictly_increasing() {
    let migrator = Migrator::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations"))
        .await
        .unwrap();
    for pair in migrator.iter().collect::<Vec<_>>().windows(2) {
        let earlier = &pair[0];
        let later = &pair[1];
        assert_ne!(
            earlier.version, later.version,
            "duplicate migration version {} has descriptions `{}` and `{}`",
            earlier.version, earlier.description, later.description
        );
        assert!(
            earlier.version < later.version,
            "migration versions must strictly increase: {} `{}` then {} `{}`",
            earlier.version,
            earlier.description,
            later.version,
            later.description
        );
    }
}
