use sqlx::{Sqlite, Transaction};

use crate::{Store, WorkspaceResult, WorkspaceStoreError};

impl Store {
    /// Acquire the SQLite write lock before reading a workspace or lease row.
    #[allow(dead_code)]
    pub(crate) async fn begin_workspace_immediate(
        &self,
    ) -> WorkspaceResult<Transaction<'_, Sqlite>> {
        self.pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|_| WorkspaceStoreError::Database)
    }
}
