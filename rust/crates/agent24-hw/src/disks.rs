//! Disk enumeration helpers: filtering of read-only system volumes / APFS
//! virtual volumes, and models-dir → disk matching. Pure functions.

use std::path::Path;

use crate::DiskInfo;

/// macOS firmlinked data volume: the one writable volume under
/// `/System/Volumes/` that users actually care about.
const MACOS_DATA_VOLUME: &str = "/System/Volumes/Data";

const VIRTUAL_FS_TYPES: &[&str] = &[
    "devfs", "autofs", "tmpfs", "devtmpfs", "squashfs", "ramfs", "proc", "sysfs", "cgroup",
    "cgroup2", "overlay", "efivarfs", "fusectl", "nullfs",
];

/// Whether a mounted volume is worth listing as a user-visible disk.
///
/// Drops: zero-size volumes, read-only volumes (sealed macOS system
/// snapshot, mounted DMGs, optical media), macOS APFS helper volumes under
/// `/System/Volumes/*` (except `Data`), `/private/var/vm`, simulator images,
/// Time Machine snapshots, and virtual filesystems.
pub fn is_user_visible_volume(
    mount_point: &str,
    name: &str,
    fs_type: &str,
    total_bytes: u64,
    read_only: bool,
) -> bool {
    if total_bytes == 0 || read_only {
        return false;
    }
    if mount_point == "/dev" || mount_point.starts_with("/dev/") {
        return false;
    }
    if mount_point.starts_with("/System/Volumes/") && mount_point != MACOS_DATA_VOLUME {
        return false;
    }
    if mount_point == "/private/var/vm" || mount_point.starts_with("/private/var/vm/") {
        return false;
    }
    if mount_point.starts_with("/Library/Developer/CoreSimulator/") {
        return false;
    }
    if name.starts_with("com.apple.TimeMachine.") || mount_point.contains(".timemachine") {
        return false;
    }
    let fs = fs_type.to_ascii_lowercase();
    if VIRTUAL_FS_TYPES.contains(&fs.as_str()) {
        return false;
    }
    true
}

/// The disk whose mount point is the longest component-wise prefix of
/// `path`. `None` when no disk contains it.
pub fn disk_for_path<'a>(disks: &'a [DiskInfo], path: &Path) -> Option<&'a DiskInfo> {
    disks
        .iter()
        .filter(|d| path.starts_with(Path::new(&d.mount_point)))
        .max_by_key(|d| Path::new(&d.mount_point).components().count())
}

/// Free bytes on the disk holding `path`. Prefers the user-visible list; if
/// none matches (e.g. macOS `/Users/...` where only `/` — the read-only
/// sealed snapshot of the same container — contains it, and it was
/// filtered), falls back to the unfiltered list.
pub fn free_bytes_for_path(visible: &[DiskInfo], all: &[DiskInfo], path: &Path) -> Option<u64> {
    disk_for_path(visible, path)
        .or_else(|| disk_for_path(all, path))
        .map(|d| d.free_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(mount: &str, free: u64) -> DiskInfo {
        DiskInfo {
            name: mount.to_owned(),
            mount_point: mount.to_owned(),
            total_bytes: 1000,
            free_bytes: free,
            removable: false,
            read_only: false,
        }
    }

    #[test]
    fn filters_system_and_virtual_volumes() {
        // (mount, name, fs, total, ro, expected)
        let cases: &[(&str, &str, &str, u64, bool, bool)] = &[
            ("/", "Macintosh HD", "apfs", 500, true, false),
            (
                "/System/Volumes/Data",
                "Macintosh HD - Data",
                "apfs",
                500,
                false,
                true,
            ),
            ("/System/Volumes/VM", "VM", "apfs", 500, false, false),
            (
                "/System/Volumes/Preboot",
                "Preboot",
                "apfs",
                500,
                false,
                false,
            ),
            (
                "/System/Volumes/Update",
                "Update",
                "apfs",
                500,
                false,
                false,
            ),
            ("/private/var/vm", "VM", "apfs", 500, false, false),
            ("/dev", "devfs", "devfs", 1, false, false),
            ("/Volumes/External", "External", "exfat", 500, false, true),
            ("/Volumes/Disk Image", "img", "hfs", 500, true, false),
            (
                "/Volumes/com.apple.TimeMachine.localsnapshots",
                "com.apple.TimeMachine.2026",
                "apfs",
                5,
                false,
                false,
            ),
            (
                "/Library/Developer/CoreSimulator/Volumes/iOS",
                "iOS",
                "apfs",
                5,
                false,
                false,
            ),
            ("/run/user/1000", "tmpfs", "tmpfs", 5, false, false),
            ("/", "/dev/nvme0n1p2", "ext4", 500, false, true),
            ("/mnt/zero", "z", "ext4", 0, false, false),
        ];
        for (mount, name, fs, total, ro, expected) in cases {
            assert_eq!(
                is_user_visible_volume(mount, name, fs, *total, *ro),
                *expected,
                "{mount} ({fs})"
            );
        }
    }

    #[test]
    fn longest_prefix_wins() {
        let disks = vec![d("/", 1), d("/Volumes/Ext", 2), d("/Volumes/Ext/deep", 3)];
        let p = Path::new("/Volumes/Ext/deep/models");
        assert_eq!(disk_for_path(&disks, p).map(|x| x.free_bytes), Some(3));
        let p = Path::new("/Volumes/Ext/other");
        assert_eq!(disk_for_path(&disks, p).map(|x| x.free_bytes), Some(2));
        let p = Path::new("/Users/me/.omlx/models");
        assert_eq!(disk_for_path(&disks, p).map(|x| x.free_bytes), Some(1));
    }

    #[test]
    fn prefix_is_component_wise_not_string_wise() {
        let disks = vec![d("/", 1), d("/Volumes/Ext", 2)];
        let p = Path::new("/Volumes/ExtraDisk/models");
        assert_eq!(disk_for_path(&disks, p).map(|x| x.free_bytes), Some(1));
    }

    #[test]
    fn no_match_is_none_and_fallback_uses_all() {
        let visible = vec![d("/Volumes/Ext", 2)];
        let all = vec![d("/", 9), d("/Volumes/Ext", 2)];
        let p = Path::new("/Users/me/.omlx/models");
        assert_eq!(disk_for_path(&visible, p), None);
        assert_eq!(free_bytes_for_path(&visible, &all, p), Some(9));
        assert_eq!(free_bytes_for_path(&[], &[], p), None);
    }
}
