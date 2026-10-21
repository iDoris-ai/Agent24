//! Builtin `fs_write` path classification for L-APPR-8.
//!
//! The grant target is a canonical directory, compared by path components.
//! A string prefix is not enough: `/proj` must not cover `/proj-evil`.

use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteLocation {
    InsideConfirmedRoot,
    Outside { directory: String },
    AskWithoutDirectoryGrant,
}

pub fn classify_builtin_write(workspace_root: Option<&Path>, raw_path: &str) -> WriteLocation {
    let raw = Path::new(raw_path);
    if raw
        .components()
        .any(|part| matches!(part, Component::ParentDir))
    {
        return WriteLocation::AskWithoutDirectoryGrant;
    }
    let Some(canonical) = canonical_claim(raw) else {
        return WriteLocation::AskWithoutDirectoryGrant;
    };
    if let Some(root) = workspace_root
        && let Some(root) = canonical_claim(root)
        && !forbidden_workspace_root(&root)
        && is_within(&root, &canonical)
    {
        return WriteLocation::InsideConfirmedRoot;
    }
    match canonical.parent().and_then(path_utf8) {
        Some(directory) => WriteLocation::Outside { directory },
        None => WriteLocation::AskWithoutDirectoryGrant,
    }
}

fn forbidden_workspace_root(root: &Path) -> bool {
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from)
        && let Some(home) = canonical_claim(&home)
    {
        let ssh = home.join(".ssh");
        if root == home || root == ssh || is_within(root, &home) || is_within(root, &ssh) {
            return true;
        }
    }
    const SYSTEM: &[&str] = &[
        "/",
        "/System",
        "/usr",
        "/bin",
        "/sbin",
        "/etc",
        "/var",
        "/private",
        "/Library",
        "/Applications",
    ];
    SYSTEM.iter().any(|item| {
        let literal = Path::new(item);
        let canonical = literal
            .canonicalize()
            .unwrap_or_else(|_| literal.to_path_buf());
        root == literal || root == canonical
    })
}

fn is_within(root: &Path, path: &Path) -> bool {
    let root: Vec<_> = root.components().collect();
    let path: Vec<_> = path.components().collect();
    path.len() > root.len() && path[..root.len()] == root[..]
}

fn path_utf8(path: &Path) -> Option<String> {
    path.to_str().map(str::to_owned)
}

fn canonical_claim(raw: &Path) -> Option<PathBuf> {
    if !raw.is_absolute() {
        return None;
    }
    let mut missing = Vec::new();
    // Drop a trailing slash or `.` before lstat. Otherwise `lnk/` follows the
    // link, ENOENT looks like a missing component, and the link name is glued
    // back on as if it were a real directory.
    let mut cursor: PathBuf = raw.components().collect();
    loop {
        if let Ok(found) = cursor.canonicalize() {
            let mut out = found;
            for part in missing.iter().rev() {
                out.push(part);
            }
            return Some(out);
        }
        if cursor.symlink_metadata().is_ok() {
            return None;
        }
        missing.push(cursor.file_name()?.to_os_string());
        cursor = cursor.parent()?.to_path_buf();
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use std::fs;

    fn temp() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "a24-write-scope-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn component_boundary_does_not_treat_a_prefix_as_the_root() {
        let dir = temp();
        let root = dir.join("proj");
        let evil = dir.join("proj-evil");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&evil).unwrap();
        let inside = root.join("a.txt");
        let outside = evil.join("a.txt");
        assert_eq!(
            classify_builtin_write(Some(&root), inside.to_str().unwrap()),
            WriteLocation::InsideConfirmedRoot
        );
        assert!(matches!(
            classify_builtin_write(Some(&root), outside.to_str().unwrap()),
            WriteLocation::Outside { .. }
        ));
    }

    #[test]
    fn home_and_system_roots_are_not_confirmed() {
        let home = std::env::var("HOME").unwrap();
        let file = Path::new(&home).join("notes.txt");
        assert!(matches!(
            classify_builtin_write(Some(Path::new(&home)), file.to_str().unwrap()),
            WriteLocation::Outside { .. }
        ));
        assert!(matches!(
            classify_builtin_write(Some(Path::new("/")), "/tmp/a.txt"),
            WriteLocation::Outside { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_is_classified_by_its_canonical_target() {
        let dir = temp();
        let root = dir.join("root");
        let outside = dir.join("outside");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        let via_link = root.join("link").join("secret.txt");
        assert!(matches!(
            classify_builtin_write(Some(&root), via_link.to_str().unwrap()),
            WriteLocation::Outside { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn a_trailing_slash_does_not_hide_a_symlink() {
        let dir = temp();
        let allowed = dir.join("allowed");
        let secret = dir.join("secret");
        fs::create_dir_all(&allowed).unwrap();
        fs::create_dir_all(&secret).unwrap();
        std::os::unix::fs::symlink("../secret/new", allowed.join("lnk")).unwrap();
        for raw in [
            allowed.join("lnk").to_str().unwrap().to_owned() + "/",
            allowed.join("lnk").to_str().unwrap().to_owned() + "/.",
        ] {
            assert!(
                !matches!(
                    classify_builtin_write(None, &raw),
                    WriteLocation::Outside { directory } if directory.ends_with("/allowed")
                ),
                "{raw}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_symlink_is_not_inside_the_root() {
        let dir = temp();
        let root = dir.join("root");
        fs::create_dir(&root).unwrap();
        std::os::unix::fs::symlink("/no/such/outside", root.join("dangle")).unwrap();
        assert!(matches!(
            classify_builtin_write(Some(&root), root.join("dangle").to_str().unwrap()),
            WriteLocation::AskWithoutDirectoryGrant | WriteLocation::Outside { .. }
        ));
    }

    #[test]
    fn parent_segments_do_not_count_as_inside() {
        let dir = temp();
        let root = dir.join("root");
        fs::create_dir_all(root.join("sub")).unwrap();
        let raw = root.join("sub").join("..").join("..").join("victim");
        assert_eq!(
            classify_builtin_write(Some(&root), raw.to_str().unwrap()),
            WriteLocation::AskWithoutDirectoryGrant
        );
    }

    #[test]
    fn a_relative_path_cannot_mint_a_directory_grant() {
        assert_eq!(
            classify_builtin_write(None, "notes.txt"),
            WriteLocation::AskWithoutDirectoryGrant
        );
    }

    #[test]
    fn the_filesystem_root_cannot_mint_a_directory_grant() {
        assert_eq!(
            classify_builtin_write(None, "/"),
            WriteLocation::AskWithoutDirectoryGrant
        );
    }
}
