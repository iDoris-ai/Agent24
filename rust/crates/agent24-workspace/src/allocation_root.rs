//! Crate-private filesystem evidence for one allocation-journal intent.
//!
//! This module deliberately knows neither the store nor workspace registration.
//! Its input is the exact persisted intent snapshot a later composition layer
//! must supply; its output is only an owned directory handle and evidence.

#![allow(dead_code)] // Dormant until the later store-composition slice owns it.

use crate::{
    WorkspaceError,
    root::{ManagedParent, RootIdentity},
};

#[cfg(unix)]
use crate::root::PinnedWorkspaceRoot;
#[cfg(unix)]
use std::fs::File;

type Result<T> = std::result::Result<T, WorkspaceError>;

/// Immutable, validated locator fields from one allocation journal intent.
///
/// The service crate owns this private mirror only to bind filesystem evidence.
/// Store-owned allocation value types remain in `agent24-store`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AllocationRootIntent {
    locator: String,
    root_generation: String,
    parent_identity: RootIdentity,
}

impl AllocationRootIntent {
    pub(crate) fn new(
        locator: String,
        root_generation: String,
        parent_identity: RootIdentity,
    ) -> Result<Self> {
        validate_locator(&locator)?;
        if root_generation.trim().is_empty()
            || root_generation.contains('\0')
            || root_generation.contains('/')
        {
            return Err(WorkspaceError::InvalidSpec {
                field: "root_generation",
            });
        }
        let generation_suffix = format!(".{root_generation}");
        if locator
            .strip_suffix(&generation_suffix)
            .is_none_or(str::is_empty)
        {
            return Err(WorkspaceError::InvalidSpec {
                field: "root_locator",
            });
        }
        Ok(Self {
            locator,
            root_generation,
            parent_identity,
        })
    }

    pub(crate) fn locator(&self) -> &str {
        &self.locator
    }

    pub(crate) fn root_generation(&self) -> &str {
        &self.root_generation
    }

    pub(crate) fn parent_identity(&self) -> RootIdentity {
        self.parent_identity
    }
}

/// A root directory pinned to an open descriptor, rather than an ambient path.
///
/// Dropping this value closes only its descriptor. It intentionally performs no
/// unlink, rename, or other compensation; retained allocation objects are later
/// reconciled by a separate lifecycle slice.
pub(crate) struct PinnedAllocationRoot {
    #[cfg(unix)]
    file: File,
    #[cfg(unix)]
    intent: AllocationRootIntent,
    #[cfg(unix)]
    identity: RootIdentity,
}

impl PinnedAllocationRoot {
    /// Create the locator only for this exact intent and immediately pin it.
    pub(crate) fn create_for_intent(
        parent: &ManagedParent,
        intent: AllocationRootIntent,
    ) -> Result<Self> {
        create_for_intent(parent, intent)
    }

    /// Reopen only the exact locator, generation, parent, and persisted root ID.
    pub(crate) fn reopen_exact(
        parent: &ManagedParent,
        intent: AllocationRootIntent,
        expected_root_identity: RootIdentity,
    ) -> Result<Self> {
        reopen_exact(parent, intent, expected_root_identity)
    }

    /// Re-prove that the parent, pinned root, and current locator name agree.
    pub(crate) fn verify_binding(
        &self,
        parent: &ManagedParent,
        intent: &AllocationRootIntent,
        expected_root_identity: RootIdentity,
    ) -> Result<()> {
        verify_binding(self, parent, intent, expected_root_identity)
    }

    #[cfg(unix)]
    pub(crate) fn identity(&self) -> RootIdentity {
        self.identity
    }
}

fn validate_locator(locator: &str) -> Result<()> {
    if locator.is_empty()
        || locator.contains('\0')
        || locator.contains('/')
        || locator == "."
        || locator == ".."
    {
        return Err(WorkspaceError::InvalidSpec {
            field: "root_locator",
        });
    }
    Ok(())
}

#[cfg(unix)]
fn unavailable(reason: &'static str) -> WorkspaceError {
    WorkspaceError::RootUnavailable { reason }
}

#[cfg(unix)]
fn exact_parent(parent: &ManagedParent, intent: &AllocationRootIntent) -> Result<()> {
    if parent.identity() != intent.parent_identity() {
        return Err(unavailable("allocation_parent_identity_mismatch"));
    }
    parent.verify_identity()
}

#[cfg(unix)]
fn from_workspace_root(
    root: PinnedWorkspaceRoot,
    intent: AllocationRootIntent,
) -> PinnedAllocationRoot {
    let (file, identity) = root.into_parts();
    PinnedAllocationRoot {
        file,
        intent,
        identity,
    }
}

#[cfg(unix)]
fn create_for_intent(
    parent: &ManagedParent,
    intent: AllocationRootIntent,
) -> Result<PinnedAllocationRoot> {
    exact_parent(parent, &intent)?;
    let root = parent.create_root(intent.locator())?;
    let pinned = from_workspace_root(root, intent.clone());
    pinned.verify_binding(parent, &intent, pinned.identity)?;
    Ok(pinned)
}

#[cfg(unix)]
fn reopen_exact(
    parent: &ManagedParent,
    intent: AllocationRootIntent,
    expected_root_identity: RootIdentity,
) -> Result<PinnedAllocationRoot> {
    exact_parent(parent, &intent)?;
    let root = parent.reopen_root(intent.locator(), expected_root_identity)?;
    let pinned = from_workspace_root(root, intent.clone());
    pinned.verify_binding(parent, &intent, expected_root_identity)?;
    Ok(pinned)
}

#[cfg(unix)]
fn verify_binding(
    pinned: &PinnedAllocationRoot,
    parent: &ManagedParent,
    intent: &AllocationRootIntent,
    expected_root_identity: RootIdentity,
) -> Result<()> {
    if pinned.intent != *intent {
        return Err(unavailable("allocation_intent_mismatch"));
    }
    exact_parent(parent, intent)?;
    let actual = crate::root::inspect(&pinned.file, parent.trusted_owner(), "root_metadata")?;
    if actual != pinned.identity || actual != expected_root_identity {
        return Err(unavailable("allocation_root_identity_mismatch"));
    }
    let locator = parent.reopen_root(intent.locator(), expected_root_identity)?;
    if locator.identity() != pinned.identity {
        return Err(unavailable("allocation_locator_identity_mismatch"));
    }
    Ok(())
}

#[cfg(not(unix))]
fn create_for_intent(_: &ManagedParent, _: AllocationRootIntent) -> Result<PinnedAllocationRoot> {
    Err(WorkspaceError::UnsupportedPlatform)
}

#[cfg(not(unix))]
fn reopen_exact(
    _: &ManagedParent,
    _: AllocationRootIntent,
    _: RootIdentity,
) -> Result<PinnedAllocationRoot> {
    Err(WorkspaceError::UnsupportedPlatform)
}

#[cfg(not(unix))]
fn verify_binding(
    _: &PinnedAllocationRoot,
    _: &ManagedParent,
    _: &AllocationRootIntent,
    _: RootIdentity,
) -> Result<()> {
    Err(WorkspaceError::UnsupportedPlatform)
}

#[cfg(test)]
mod tests;
