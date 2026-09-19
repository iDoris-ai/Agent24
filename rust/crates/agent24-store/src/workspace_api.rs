use agent24_protocol::{Workspace, WorkspaceId};

use crate::{WorkspaceInstant, WorkspaceResult, WorkspaceState};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct WorkspaceListLimit(u16);
impl WorkspaceListLimit {
    pub fn new(value: u16) -> WorkspaceResult<Self> {
        (1..=100).contains(&value).then_some(Self(value)).ok_or(
            crate::WorkspaceStoreError::InvalidValue {
                field: "list_limit",
            },
        )
    }
    pub fn value(self) -> u16 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct WorkspaceListCursor {
    created_at: WorkspaceInstant,
    id: WorkspaceId,
}
impl WorkspaceListCursor {
    pub fn new(created_at: WorkspaceInstant, id: WorkspaceId) -> Self {
        Self { created_at, id }
    }
    pub fn created_at(&self) -> &WorkspaceInstant {
        &self.created_at
    }
    pub fn id(&self) -> &WorkspaceId {
        &self.id
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceListQuery {
    state: Option<WorkspaceState>,
    after: Option<WorkspaceListCursor>,
    limit: WorkspaceListLimit,
}
impl WorkspaceListQuery {
    pub fn new(
        state: Option<WorkspaceState>,
        after: Option<WorkspaceListCursor>,
        limit: WorkspaceListLimit,
    ) -> Self {
        Self {
            state,
            after,
            limit,
        }
    }
    pub fn state(&self) -> Option<WorkspaceState> {
        self.state
    }
    pub fn after(&self) -> Option<&WorkspaceListCursor> {
        self.after.as_ref()
    }
    pub fn limit(&self) -> WorkspaceListLimit {
        self.limit
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspacePage {
    pub items: Vec<Workspace>,
    pub next_cursor: Option<WorkspaceListCursor>,
}
impl WorkspacePage {
    pub fn new(items: Vec<Workspace>, next_cursor: Option<WorkspaceListCursor>) -> Self {
        Self { items, next_cursor }
    }
    pub fn items(&self) -> &[Workspace] {
        &self.items
    }
    pub fn next_cursor(&self) -> Option<&WorkspaceListCursor> {
        self.next_cursor.as_ref()
    }
}
