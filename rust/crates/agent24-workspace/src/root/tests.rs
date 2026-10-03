use super::*;
use crate::WorkspaceError;
use std::fmt::Debug;
fn must<T, E: Debug>(result: std::result::Result<T, E>) -> T {
    result.unwrap_or_else(|error| panic!("{error:?}"))
}
#[cfg(unix)]
mod unix {
    use super::*;
    use WorkspaceError::*;
    use std::fs::{self, File};
    use std::io::Write;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::path::Path;
    use tempfile::TempDir;
    fn state() -> (TempDir, File) {
        let dir = must(tempfile::tempdir());
        let state_file = must(File::open(dir.path()));
        (dir, state_file)
    }
    fn parent() -> (TempDir, ManagedParent) {
        let (dir, state_file) = state();
        (dir, must(ManagedParent::open_or_create(&state_file)))
    }
    fn mode(path: &Path) -> u32 {
        must(fs::metadata(path)).permissions().mode() & 0o7777
    }
    fn mkdir(path: &Path, mode: u32) {
        must(fs::create_dir(path));
        must(fs::set_permissions(path, fs::Permissions::from_mode(mode)));
    }
    fn id(path: &Path) -> RootIdentity {
        let metadata = must(fs::metadata(path));
        RootIdentity {
            dev_le: metadata.dev().to_le_bytes(),
            ino_le: metadata.ino().to_le_bytes(),
        }
    }
    fn fd_id(file: &File) -> RootIdentity {
        let metadata = must(file.metadata());
        RootIdentity {
            dev_le: metadata.dev().to_le_bytes(),
            ino_le: metadata.ino().to_le_bytes(),
        }
    }
    fn unavailable<T>(result: Result<T>) -> bool {
        matches!(result, Err(RootUnavailable { .. }))
    }
    fn ur<T>(result: Result<T>) -> bool {
        matches!(result, Err(RootUnavailable { reason }) if reason == "root_identity_mismatch")
    }
    fn cr<T>(result: Result<T>, expected: &str) -> bool {
        matches!(result, Err(RootConflict { reason }) if reason == expected)
    }
    #[test]
    fn create_and_reopen_pin_exact_mode_and_identity() {
        let (dir, parent) = parent();
        let root_path = dir.path().join("workspace-roots/r");
        let root = must(parent.create_root("r"));
        assert_eq!(mode(&root_path), 0o700);
        assert_eq!(root.identity(), fd_id(&root.file));
        let reopened = must(parent.reopen_root("r", root.identity()));
        assert_eq!(reopened.identity(), fd_id(&reopened.file));
    }
    #[test]
    fn invalid_names_are_rejected_without_children() {
        let (dir, parent) = parent();
        let roots = dir.path().join("workspace-roots");
        for name in ["", ".", "..", "a/b", "/abs", "nul\0x"] {
            let before = must(fs::read_dir(&roots)).count();
            assert!(matches!(
                parent.create_root(name),
                Err(InvalidSpec { field: "root_name" })
            ));
            assert_eq!(must(fs::read_dir(&roots)).count(), before);
        }
    }
    #[test]
    fn managed_parent_rejects_symlink_file_and_wrong_mode() {
        let (dir, state_file) = state();
        let roots = dir.path().join("workspace-roots");
        let target = dir.path().join("target");
        mkdir(&target, 0o700);
        must(std::os::unix::fs::symlink(&target, &roots));
        assert!(unavailable(ManagedParent::open_or_create(&state_file)));
        assert_eq!(mode(&target), 0o700);
        let (dir, state_file) = state();
        let roots = dir.path().join("workspace-roots");
        must(fs::write(&roots, b"file"));
        let before = mode(&roots);
        assert!(unavailable(ManagedParent::open_or_create(&state_file)));
        assert_eq!(mode(&roots), before);
        let (dir, state_file) = state();
        let roots = dir.path().join("workspace-roots");
        mkdir(&roots, 0o755);
        assert!(unavailable(ManagedParent::open_or_create(&state_file)));
        assert_eq!(mode(&roots), 0o755);
    }
    #[test]
    fn existing_root_is_not_adopted_or_chmodded() {
        let (dir, parent) = parent();
        let root = dir.path().join("workspace-roots/existing");
        mkdir(&root, 0o755);
        let marker = root.join("marker");
        must(fs::write(&marker, b"keep"));
        assert!(cr(parent.create_root("existing"), "root_exists"));
        assert_eq!(mode(&root), 0o755);
        assert_eq!(must(fs::read(&marker)), b"keep");
    }
    #[test]
    fn root_symlink_is_not_followed() {
        let (dir, parent) = parent();
        let target = dir.path().join("target");
        mkdir(&target, 0o700);
        let link = dir.path().join("workspace-roots/link");
        must(std::os::unix::fs::symlink(&target, &link));
        assert!(unavailable(parent.reopen_root("link", id(&target))));
        assert!(must(fs::symlink_metadata(&link)).file_type().is_symlink());
        assert_eq!(mode(&target), 0o700);
    }
    #[test]
    fn delete_recreate_rejects_old_identity() {
        let (dir, parent) = parent();
        let old = must(parent.create_root("same"));
        let old_id = old.identity();
        must(fs::remove_dir(dir.path().join("workspace-roots/same")));
        let new = must(parent.create_root("same"));
        assert_ne!(old_id, new.identity());
        assert!(ur(parent.reopen_root("same", old_id)));
    }
    #[test]
    fn pinned_root_writes_through_parent_rename_swap() {
        let (dir, parent) = parent();
        let pinned = must(parent.create_root("pinned"));
        let roots = dir.path().join("workspace-roots");
        let old_roots = dir.path().join("workspace-roots-old");
        must(fs::rename(&roots, &old_roots));
        mkdir(&roots, 0o700);
        mkdir(&roots.join("pinned"), 0o700);
        must(parent.create_root("after-swap"));
        assert!(old_roots.join("after-swap").is_dir());
        assert!(!roots.join("after-swap").exists());
        let fd = must(rustix::fs::openat(
            &pinned.file,
            "marker",
            rustix::fs::OFlags::WRONLY
                | rustix::fs::OFlags::CREATE
                | rustix::fs::OFlags::TRUNC
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        ));
        must(File::from(fd).write_all(b"original"));
        assert_eq!(must(fs::read(old_roots.join("pinned/marker"))), b"original");
        assert!(!roots.join("pinned/marker").exists());
    }
}
#[cfg(not(unix))]
#[test]
fn open_or_create_is_unsupported() {
    let dir = must(tempfile::tempdir());
    let state = must(std::fs::File::create(dir.path().join("state")));
    let result = ManagedParent::open_or_create(&state);
    assert!(matches!(result, Err(WorkspaceError::UnsupportedPlatform)));
}
