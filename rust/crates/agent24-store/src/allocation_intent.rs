use agent24_protocol::WorkspaceId;

use crate::{AllocationId, AllocationValueError, RootIdentity, WorkspaceInstant};

/// Structurally validated, inert input for a workspace allocation journal row.
///
/// Constructing an intent performs no database, workspace-registration, or
/// filesystem operation. Its validation establishes neither caller
/// authorization nor filesystem authority or ownership.
pub struct AllocationIntent {
    allocation_id: AllocationId,
    workspace_id: WorkspaceId,
    root_generation: String,
    relative_name: String,
    parent_identity: RootIdentity,
    created_at: WorkspaceInstant,
}

impl AllocationIntent {
    pub fn new(
        allocation_id: AllocationId,
        workspace_id: WorkspaceId,
        root_generation: String,
        relative_name: String,
        parent_identity: RootIdentity,
        created_at: WorkspaceInstant,
    ) -> Result<Self, AllocationValueError> {
        if root_generation.trim().is_empty() || root_generation.contains('\0') {
            return Err(AllocationValueError::InvalidRootGeneration);
        }
        let name_len = relative_name.chars().count();
        if !(1..=255).contains(&name_len)
            || matches!(relative_name.as_str(), "." | "..")
            || relative_name
                .chars()
                .any(|character| matches!(character, '\0' | '/' | '\\'))
        {
            return Err(AllocationValueError::InvalidRelativeName);
        }
        Ok(Self {
            allocation_id,
            workspace_id,
            root_generation,
            relative_name,
            parent_identity,
            created_at,
        })
    }

    pub fn allocation_id(&self) -> &AllocationId {
        &self.allocation_id
    }
    pub fn workspace_id(&self) -> &WorkspaceId {
        &self.workspace_id
    }
    pub fn root_generation(&self) -> &str {
        &self.root_generation
    }
    pub fn relative_name(&self) -> &str {
        &self.relative_name
    }
    pub fn parent_identity(&self) -> RootIdentity {
        self.parent_identity
    }
    pub fn created_at(&self) -> &WorkspaceInstant {
        &self.created_at
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn input(name: &str, identity: RootIdentity) -> Result<AllocationIntent, AllocationValueError> {
        AllocationIntent::new(
            AllocationId::parse("wa_01J5M4Q2Y7N8P9R0S1T2V3W4X5").unwrap(),
            WorkspaceId::parse("ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5").unwrap(),
            "generation-1".to_owned(),
            name.to_owned(),
            identity,
            WorkspaceInstant::parse("2026-09-19T00:00:00.000Z").unwrap(),
        )
    }

    #[test]
    fn accepts_unix_windows_and_unicode_character_boundary() {
        let unix = input("scratch", RootIdentity::unix(&[1; 8], &[2; 8]).unwrap()).unwrap();
        assert_eq!(
            unix.allocation_id().as_str(),
            "wa_01J5M4Q2Y7N8P9R0S1T2V3W4X5"
        );
        assert_eq!(
            unix.workspace_id().as_str(),
            "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5"
        );
        assert_eq!(unix.root_generation(), "generation-1");
        assert_eq!(unix.relative_name(), "scratch");
        assert_eq!(
            unix.parent_identity(),
            RootIdentity::unix(&[1; 8], &[2; 8]).unwrap()
        );
        assert_eq!(unix.created_at().as_str(), "2026-09-19T00:00:00.000Z");
        assert!(input("目录", RootIdentity::windows(&[3; 8], &[4; 16]).unwrap()).is_ok());
        assert!(
            input(
                &"界".repeat(255),
                RootIdentity::unix(&[1; 8], &[2; 8]).unwrap()
            )
            .is_ok()
        );
        assert!(
            input(
                &"界".repeat(256),
                RootIdentity::unix(&[1; 8], &[2; 8]).unwrap()
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_separators_empty_values_and_does_not_echo_secrets() {
        for name in ["", ".", "..", "a/b", "a\\b", "a\0b"] {
            let error = match input(name, RootIdentity::unix(&[1; 8], &[2; 8]).unwrap()) {
                Ok(_) => panic!("invalid relative name accepted"),
                Err(error) => error,
            };
            assert_eq!(error, AllocationValueError::InvalidRelativeName);
            if !name.is_empty() {
                assert!(!error.to_string().contains(name));
            }
        }
        for generation in ["", "   ", "generation\0secret"] {
            let error = match AllocationIntent::new(
                AllocationId::parse("wa_01J5M4Q2Y7N8P9R0S1T2V3W4X5").unwrap(),
                WorkspaceId::parse("ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5").unwrap(),
                generation.to_owned(),
                "root".to_owned(),
                RootIdentity::unix(&[1; 8], &[2; 8]).unwrap(),
                WorkspaceInstant::parse("2026-09-19T00:00:00.000Z").unwrap(),
            ) {
                Ok(_) => panic!("invalid root generation accepted"),
                Err(error) => error,
            };
            assert_eq!(error, AllocationValueError::InvalidRootGeneration);
            if !generation.is_empty() {
                assert!(!error.to_string().contains(generation));
            }
        }
    }
}
