#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::hooks::{DIR_SYNCS, FAIL_DIR_SYNC};
use super::*;

fn write(dir: &Path, offset: u64, bytes: &[u8]) -> Result<(), WriteError> {
    write_at(dir, offset, bytes)
}

#[test]
fn the_first_write_creates_the_upload_directory_and_file() {
    let root = tempfile::tempdir().unwrap();
    let dir = upload_dir(root.path(), "upl_01K74Z3QJ8V5N2W9RTX6YB4MCD");
    assert_eq!(
        dir,
        root.path().join("uploads/upl_01K74Z3QJ8V5N2W9RTX6YB4MCD")
    );
    assert!(write(&dir, 0, b"0123").is_ok());
    assert!(write(&dir, 4, b"45").is_ok());
    assert_eq!(std::fs::read(dir.join("data")).unwrap(), b"012345");
}

#[test]
fn a_write_cuts_the_file_back_to_its_offset_first() {
    let root = tempfile::tempdir().unwrap();
    let dir = upload_dir(root.path(), "u1");
    std::fs::create_dir_all(&dir).unwrap();
    // Bytes past the offset, as a crash between write and commit leaves them.
    std::fs::write(dir.join("data"), b"0123XXXXXXXX").unwrap();
    assert!(write(&dir, 4, b"45").is_ok());
    assert_eq!(std::fs::read(dir.join("data")).unwrap(), b"012345");
}

#[test]
fn a_file_shorter_than_the_offset_is_reported_not_padded() {
    let root = tempfile::tempdir().unwrap();
    let dir = upload_dir(root.path(), "u2");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("data"), b"01").unwrap();
    assert!(matches!(
        write(&dir, 4, b"45"),
        Err(WriteError::Short { len: 2 })
    ));
    assert_eq!(std::fs::read(dir.join("data")).unwrap(), b"01");
}

#[test]
fn every_write_syncs_the_three_directories_even_on_a_retry() {
    let root = tempfile::tempdir().unwrap();
    let id = "u3-dir-sync";
    let dir = upload_dir(root.path(), id);
    FAIL_DIR_SYNC.lock().unwrap().push(id.to_owned());
    assert!(matches!(write(&dir, 0, b"01"), Err(WriteError::Io(_))));
    let before = DIR_SYNCS.lock().unwrap().len();
    // The retry finds the directory and file in place, and still syncs.
    assert!(write(&dir, 0, b"01").is_ok());
    let synced: Vec<String> = DIR_SYNCS.lock().unwrap()[before..].to_vec();
    for d in [
        dir.clone(),
        root.path().join("uploads"),
        root.path().to_owned(),
    ] {
        let d = d.display().to_string();
        assert!(synced.contains(&d), "{d} not synced: {synced:?}");
    }
}

#[test]
fn an_unwritable_directory_is_an_io_error() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("uploads"), b"a file, not a directory").unwrap();
    let dir = upload_dir(root.path(), "u4");
    assert!(matches!(write(&dir, 0, b"01"), Err(WriteError::Io(_))));
}
