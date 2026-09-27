use std::collections::HashMap;

fn validate_migration_versions<'a>(
    migrations: impl IntoIterator<Item = (i64, &'a str)>,
) -> Result<(), String> {
    let mut descriptions_by_version = HashMap::new();
    let mut previous = None;

    for (version, description) in migrations {
        if let Some(previous_description) = descriptions_by_version.insert(version, description) {
            return Err(format!(
                "duplicate migration version {version}: {previous_description} and {description}"
            ));
        }

        if let Some((previous_version, previous_description)) = previous
            && version <= previous_version
        {
            return Err(format!(
                "migration versions must be strictly increasing: {previous_version} ({previous_description}) followed by {version} ({description})"
            ));
        }

        previous = Some((version, description));
    }

    Ok(())
}

#[test]
fn embedded_migration_versions_are_unique_and_strictly_increasing() {
    let migrator = sqlx::migrate!("./migrations");
    let versions = migrator
        .iter()
        .map(|migration| (migration.version, migration.description.as_ref()));

    if let Err(error) = validate_migration_versions(versions) {
        panic!("{error}");
    }
}

#[test]
fn duplicate_migration_versions_report_both_descriptions() {
    let error = match validate_migration_versions([(7, "module_approvals"), (7, "workspaces")]) {
        Ok(()) => panic!("duplicate versions must be rejected"),
        Err(error) => error,
    };

    assert!(error.contains("module_approvals"), "{error}");
    assert!(error.contains("workspaces"), "{error}");
}
