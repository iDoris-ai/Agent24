use sqlx::{Row, sqlite::SqliteRow};

use crate::{WorkspaceInstant, WorkspaceResult, WorkspaceStoreError};

pub(super) fn bad(field: &'static str) -> WorkspaceStoreError {
    WorkspaceStoreError::CorruptRow {
        table: "workspaces",
        field,
    }
}
pub(super) fn text(row: &SqliteRow, field: &'static str) -> WorkspaceResult<String> {
    let value = row.try_get::<String, _>(field).map_err(|_| bad(field))?;
    if value.is_empty() || value.contains('\0') {
        return Err(bad(field));
    }
    Ok(value)
}
pub(super) fn nonblank(row: &SqliteRow, field: &'static str) -> WorkspaceResult<String> {
    let value = text(row, field)?;
    if value.trim().is_empty() {
        return Err(bad(field));
    }
    Ok(value)
}
pub(super) fn opt_text(row: &SqliteRow, field: &'static str) -> WorkspaceResult<Option<String>> {
    let value = row
        .try_get::<Option<String>, _>(field)
        .map_err(|_| bad(field))?;
    if value.as_deref().is_some_and(|v| v.contains('\0')) {
        return Err(bad(field));
    }
    Ok(value)
}
pub(super) fn instant(row: &SqliteRow, field: &'static str) -> WorkspaceResult<WorkspaceInstant> {
    WorkspaceInstant::parse(&text(row, field)?).map_err(|_| bad(field))
}
pub(super) fn opt_instant(
    row: &SqliteRow,
    field: &'static str,
) -> WorkspaceResult<Option<WorkspaceInstant>> {
    opt_text(row, field)?
        .map(|v| WorkspaceInstant::parse(&v).map_err(|_| bad(field)))
        .transpose()
}
pub(super) fn blob(row: &SqliteRow, field: &'static str) -> WorkspaceResult<Option<Vec<u8>>> {
    row.try_get::<Option<Vec<u8>>, _>(field)
        .map_err(|_| bad(field))
}
pub(super) fn count(row: &SqliteRow, field: &'static str) -> WorkspaceResult<u64> {
    u64::try_from(row.try_get::<i64, _>(field).map_err(|_| bad(field))?).map_err(|_| bad(field))
}
pub(super) fn at_least(
    a: &WorkspaceInstant,
    b: &WorkspaceInstant,
    field: &'static str,
) -> WorkspaceResult<()> {
    (a.epoch_millis() >= b.epoch_millis())
        .then_some(())
        .ok_or(bad(field))
}
