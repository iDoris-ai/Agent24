use super::*;
use crate::WorkspaceError;
use std::fmt::Debug;

fn must<T, E: Debug>(value: std::result::Result<T, E>) -> T {
    value.unwrap_or_else(|error| panic!("{error:?}"))
}

#[cfg(unix)]
mod unix {
    use super::*;
    use crate::root::ManagedParent;
    use std::fs::{self, File};
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
    use tempfile::TempDir;

    fn managed_parent() -> (TempDir, ManagedParent) {
        let directory = must(tempfile::tempdir());
        let state = must(File::open(directory.path()));
        let parent = must(ManagedParent::open_or_create(&state));
        (directory, parent)
    }

    fn new_intent(parent: &ManagedParent, locator: &str, generation: &str) -> AllocationRootIntent {
        must(AllocationRootIntent::new(
            locator.to_owned(),
            generation.to_owned(),
            parent.identity(),
        ))
    }

    fn changed(identity: RootIdentity) -> RootIdentity {
        RootIdentity {
            dev_le: (u64::from_le_bytes(identity.dev_le) ^ 1).to_le_bytes(),
            ino_le: identity.ino_le,
        }
    }

    fn unavailable<T>(result: Result<T>, reason: &str) {
        assert!(
            matches!(result, Err(WorkspaceError::RootUnavailable { reason: actual }) if actual == reason)
        );
    }

    #[test]
    fn create_reopen_and_verify_use_descriptor_identity() {
        let (_directory, parent) = managed_parent();
        let intent = new_intent(&parent, "ws01.generation-1", "generation-1");
        let created = must(PinnedAllocationRoot::create_for_intent(
            &parent,
            intent.clone(),
        ));
        let root_identity = created.identity();

        must(created.verify_binding(&parent, &intent, root_identity));
        let reopened = must(PinnedAllocationRoot::reopen_exact(
            &parent,
            intent.clone(),
            root_identity,
        ));
        assert_eq!(reopened.identity(), root_identity);
        must(reopened.verify_binding(&parent, &intent, root_identity));
    }

    #[test]
    fn locator_replacement_and_symlink_are_not_authenticated_by_name() {
        let (directory, parent) = managed_parent();
        let locator = "ws_02.generation-1";
        let intent = new_intent(&parent, locator, "generation-1");
        let pinned = must(PinnedAllocationRoot::create_for_intent(
            &parent,
            intent.clone(),
        ));
        let root_identity = pinned.identity();
        let root = directory.path().join("workspace-roots").join(locator);
        let old = directory.path().join("old-root");
        must(fs::rename(&root, &old));
        must(fs::create_dir(&root));
        must(fs::set_permissions(
            &root,
            fs::Permissions::from_mode(0o700),
        ));

        unavailable(
            pinned.verify_binding(&parent, &intent, root_identity),
            "root_identity_mismatch",
        );
        unavailable(
            PinnedAllocationRoot::reopen_exact(&parent, intent.clone(), root_identity),
            "root_identity_mismatch",
        );
        must(fs::remove_dir(&root));
        must(symlink(&old, &root));
        unavailable(
            pinned.verify_binding(&parent, &intent, root_identity),
            "root_open",
        );
        assert!(must(fs::symlink_metadata(&root)).file_type().is_symlink());
        assert_eq!(
            must(fs::metadata(&old)).ino().to_le_bytes(),
            root_identity.ino_le
        );
    }

    #[test]
    fn intent_generation_parent_and_root_evidence_must_all_match() {
        let (_directory, parent) = managed_parent();
        let intent = new_intent(&parent, "ws03.generation-1", "generation-1");
        let pinned = must(PinnedAllocationRoot::create_for_intent(
            &parent,
            intent.clone(),
        ));
        let root_identity = pinned.identity();
        assert!(matches!(
            AllocationRootIntent::new(
                "ws03.generation-1".into(),
                "generation-2".into(),
                parent.identity(),
            ),
            Err(WorkspaceError::InvalidSpec {
                field: "root_locator"
            })
        ));
        let wrong_locator = new_intent(&parent, "ws03-other.generation-1", "generation-1");
        unavailable(
            pinned.verify_binding(&parent, &wrong_locator, root_identity),
            "allocation_intent_mismatch",
        );
        unavailable(
            pinned.verify_binding(&parent, &intent, changed(root_identity)),
            "allocation_root_identity_mismatch",
        );

        let (_other_directory, other_parent) = managed_parent();
        unavailable(
            pinned.verify_binding(&other_parent, &intent, root_identity),
            "allocation_parent_identity_mismatch",
        );
    }

    #[test]
    fn parent_path_swap_cannot_redirect_the_pinned_parent_handle() {
        let (directory, parent) = managed_parent();
        let intent = new_intent(&parent, "ws_04.generation-1", "generation-1");
        let pinned = must(PinnedAllocationRoot::create_for_intent(
            &parent,
            intent.clone(),
        ));
        let root_identity = pinned.identity();
        let roots = directory.path().join("workspace-roots");
        let old_roots = directory.path().join("workspace-roots-old");
        must(fs::rename(&roots, &old_roots));
        must(fs::create_dir(&roots));
        must(fs::set_permissions(
            &roots,
            fs::Permissions::from_mode(0o700),
        ));
        must(fs::create_dir(roots.join(intent.locator())));
        must(fs::set_permissions(
            roots.join(intent.locator()),
            fs::Permissions::from_mode(0o700),
        ));

        // The pinned parent remains the original directory; the replacement
        // pathname is never consulted by create, reopen, or verification.
        must(parent.verify_identity());
        must(pinned.verify_binding(&parent, &intent, root_identity));
        assert!(old_roots.join(intent.locator()).is_dir());
        assert!(roots.join(intent.locator()).is_dir());
    }

    #[test]
    fn invalid_locator_and_generation_never_create_a_directory() {
        let (directory, parent) = managed_parent();
        for (locator, generation, field) in [
            ("", "generation", "root_locator"),
            ("a/b", "generation", "root_locator"),
            (".", "generation", "root_locator"),
            ("root", " \t", "root_generation"),
            ("root", "generation\0", "root_generation"),
            ("root.generation", "generation/escape", "root_generation"),
            ("root.generation-1", "generation-2", "root_locator"),
        ] {
            assert!(matches!(
                AllocationRootIntent::new(locator.into(), generation.into(), parent.identity()),
                Err(WorkspaceError::InvalidSpec { field: actual }) if actual == field
            ));
        }
        assert_eq!(
            must(fs::read_dir(directory.path().join("workspace-roots"))).count(),
            0
        );
    }
}

#[cfg(not(unix))]
#[test]
fn windows_boundary_is_explicitly_unsupported_without_handle_evidence() {
    use crate::root::ManagedParent;

    let parent = ManagedParent { _private: () };
    let intent = must(AllocationRootIntent::new(
        "ws_01.generation-1".into(),
        "generation-1".into(),
        RootIdentity,
    ));
    assert!(matches!(
        PinnedAllocationRoot::create_for_intent(&parent, intent),
        Err(WorkspaceError::UnsupportedPlatform)
    ));
}
