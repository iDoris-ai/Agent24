use agent24_protocol::WorkspaceId;
use sqlx::{Row, TypeInfo, ValueRef, sqlite::SqliteRow};

use crate::{
    AllocationFailureReason, AllocationId, AllocationPhase, RootIdentity, WorkspaceInstant,
    WorkspaceResult, WorkspaceStoreError,
};

fn bad(field: &'static str) -> WorkspaceStoreError {
    WorkspaceStoreError::CorruptRow {
        table: "workspace_allocations",
        field,
    }
}

fn text(row: &SqliteRow, field: &'static str) -> WorkspaceResult<String> {
    let raw = row.try_get_raw(field).map_err(|_| bad(field))?;
    if raw.is_null() || raw.type_info().name() != "TEXT" {
        return Err(bad(field));
    }
    row.try_get::<String, _>(field).map_err(|_| bad(field))
}
fn optional_text(row: &SqliteRow, field: &'static str) -> WorkspaceResult<Option<String>> {
    let raw = row.try_get_raw(field).map_err(|_| bad(field))?;
    if raw.is_null() {
        return Ok(None);
    }
    if raw.type_info().name() != "TEXT" {
        return Err(bad(field));
    }
    let value = row.try_get::<String, _>(field).map_err(|_| bad(field))?;
    if value.contains('\0') {
        return Err(bad(field));
    }
    Ok(Some(value))
}
fn optional_blob(row: &SqliteRow, field: &'static str) -> WorkspaceResult<Option<Vec<u8>>> {
    let raw = row.try_get_raw(field).map_err(|_| bad(field))?;
    if raw.is_null() {
        return Ok(None);
    }
    if raw.type_info().name() != "BLOB" {
        return Err(bad(field));
    }
    row.try_get::<Vec<u8>, _>(field)
        .map(Some)
        .map_err(|_| bad(field))
}
fn identity(
    row: &SqliteRow,
    kind_field: &'static str,
    optional: bool,
) -> WorkspaceResult<Option<RootIdentity>> {
    let kind = if optional {
        optional_text(row, kind_field)?
    } else {
        Some(text(row, kind_field)?)
    };
    let fields = match kind_field {
        "parent_identity_kind" => [
            "parent_unix_device",
            "parent_unix_inode",
            "parent_windows_volume",
            "parent_windows_file_id",
        ],
        _ => [
            "root_unix_device",
            "root_unix_inode",
            "root_windows_volume",
            "root_windows_file_id",
        ],
    };
    let (ud, ui, wv, wf) = (
        optional_blob(row, fields[0])?,
        optional_blob(row, fields[1])?,
        optional_blob(row, fields[2])?,
        optional_blob(row, fields[3])?,
    );
    let none = || ud.is_none() && ui.is_none() && wv.is_none() && wf.is_none();
    let exact = |value: &Option<Vec<u8>>, size| value.as_ref().is_some_and(|v| v.len() == size);
    match kind.as_deref() {
        None if optional && none() => Ok(None),
        Some("unix") if exact(&ud, 8) && exact(&ui, 8) && wv.is_none() && wf.is_none() => Ok(Some(
            RootIdentity::unix(
                ud.as_deref().unwrap_or_default(),
                ui.as_deref().unwrap_or_default(),
            )
            .map_err(|_| bad(kind_field))?,
        )),
        Some("windows") if ud.is_none() && ui.is_none() && exact(&wv, 8) && exact(&wf, 16) => {
            Ok(Some(
                RootIdentity::windows(
                    wv.as_deref().unwrap_or_default(),
                    wf.as_deref().unwrap_or_default(),
                )
                .map_err(|_| bad(kind_field))?,
            ))
        }
        _ => Err(bad(kind_field)),
    }
}

macro_rules! borrowed_accessors {
    ($($name:ident: $ty:ty = $field:ident),+ $(,)?) => { $(
        pub fn $name(&self) -> &$ty { &self.$field }
    )+ };
}
macro_rules! copied_accessors {
    ($($name:ident: $ty:ty = $field:ident),+ $(,)?) => { $(
        pub fn $name(&self) -> $ty { self.$field }
    )+ };
}

pub struct AllocationRecord {
    id: AllocationId,
    workspace_id: WorkspaceId,
    root_generation: String,
    relative_name: String,
    parent_identity: RootIdentity,
    root_identity: Option<RootIdentity>,
    phase: AllocationPhase,
    created_at: WorkspaceInstant,
    failure_reason: Option<AllocationFailureReason>,
}

impl AllocationRecord {
    borrowed_accessors!(id: AllocationId = id, workspace_id: WorkspaceId = workspace_id,
        root_generation: str = root_generation, relative_name: str = relative_name,
        created_at: WorkspaceInstant = created_at);
    copied_accessors!(parent_identity: RootIdentity = parent_identity,
        root_identity: Option<RootIdentity> = root_identity, phase: AllocationPhase = phase);
    pub fn failure_reason(&self) -> Option<&AllocationFailureReason> {
        self.failure_reason.as_ref()
    }

    pub(crate) fn decode(row: &SqliteRow) -> WorkspaceResult<Self> {
        let id =
            AllocationId::parse(&text(row, "allocation_id")?).map_err(|_| bad("allocation_id"))?;
        let workspace_id =
            WorkspaceId::parse(text(row, "workspace_id")?).map_err(|_| bad("workspace_id"))?;
        let root_generation = text(row, "root_generation")?;
        if root_generation.trim().is_empty() || root_generation.contains('\0') {
            return Err(bad("root_generation"));
        }
        let relative_name = text(row, "relative_name")?;
        let length = relative_name.chars().count();
        if length == 0
            || length > 255
            || matches!(relative_name.as_str(), "." | "..")
            || relative_name
                .chars()
                .any(|c| matches!(c, '\0' | '/' | '\\'))
        {
            return Err(bad("relative_name"));
        }
        let parent_identity =
            identity(row, "parent_identity_kind", false)?.ok_or(bad("parent_identity_kind"))?;
        let root_identity = identity(row, "root_identity_kind", true)?;
        let phase = match text(row, "phase")?.as_str() {
            "reserved" => AllocationPhase::Reserved,
            "materialized" => AllocationPhase::Materialized,
            "committed" => AllocationPhase::Committed,
            "retained" => AllocationPhase::Retained,
            _ => return Err(bad("phase")),
        };
        let created_at =
            WorkspaceInstant::parse(&text(row, "created_at")?).map_err(|_| bad("created_at"))?;
        let reason = optional_text(row, "failure_reason")?
            .as_deref()
            .map(AllocationFailureReason::parse)
            .transpose()
            .map_err(|_| bad("failure_reason"))?;
        match phase {
            AllocationPhase::Reserved if root_identity.is_some() || reason.is_some() => {
                return Err(bad("phase"));
            }
            AllocationPhase::Materialized | AllocationPhase::Committed
                if root_identity.is_none() || reason.is_some() =>
            {
                return Err(bad("phase"));
            }
            AllocationPhase::Retained if reason.is_none() => return Err(bad("failure_reason")),
            _ => {}
        }
        Ok(Self {
            id,
            workspace_id,
            root_generation,
            relative_name,
            parent_identity,
            root_identity,
            phase,
            created_at,
            failure_reason: reason,
        })
    }
}
