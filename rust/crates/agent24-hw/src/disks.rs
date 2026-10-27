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

/// Free bytes on the disk holding `path`. Chooses the longest component-wise
/// mount point match between `visible` and `all`. If mount point component
/// lengths tie, prefers the user-visible list; falls back to `all` when `all`
/// has a strictly longer mount point match (e.g. a filtered read-only child
/// mount) or when `visible` has no match (e.g. macOS `/Users/...` where only
/// `/` — the read-only sealed snapshot of the same container — contains it).
pub fn free_bytes_for_path(visible: &[DiskInfo], all: &[DiskInfo], path: &Path) -> Option<u64> {
    let v_disk = disk_for_path(visible, path);
    let a_disk = disk_for_path(all, path);
    match (v_disk, a_disk) {
        (Some(v), Some(a)) => {
            let v_len = Path::new(&v.mount_point).components().count();
            let a_len = Path::new(&a.mount_point).components().count();
            if a_len > v_len {
                Some(a.free_bytes)
            } else {
                Some(v.free_bytes)
            }
        }
        (Some(v), None) => Some(v.free_bytes),
        (None, Some(a)) => Some(a.free_bytes),
        (None, None) => None,
    }
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

    #[test]
    fn gap_disk_longest_mount_must_win_even_if_filtered() {
        let root = d("/", 50 * 1024 * 1024 * 1024);
        let mut nested = d("/mnt/readonly", 0);
        nested.read_only = true;
        let visible = vec![root.clone()];
        let all = vec![root, nested];
        let p = Path::new("/mnt/readonly/models");
        assert_eq!(free_bytes_for_path(&visible, &all, p), Some(0));
    }

    #[test]
    fn deeper_filtered_writable_volume_wins() {
        let root = d("/", 50);
        let mut sub = d("/var/filtered_writable", 12);
        sub.read_only = false;
        let visible = vec![root.clone()];
        let all = vec![root, sub];
        let p = Path::new("/var/filtered_writable/models");
        assert_eq!(free_bytes_for_path(&visible, &all, p), Some(12));
    }

    #[test]
    fn component_boundary_foo_vs_foobar() {
        let root = d("/", 50);
        let foo = d("/mnt/foo", 20);
        let visible = vec![root.clone(), foo.clone()];
        let all = vec![root, foo];
        let p = Path::new("/mnt/foobar/models");
        // /mnt/foobar is not under /mnt/foo, so / should be the longest component match
        assert_eq!(free_bytes_for_path(&visible, &all, p), Some(50));
    }

    #[test]
    fn no_match_and_same_mount_depth() {
        let visible = vec![d("/mnt/data", 30)];
        let all = vec![d("/mnt/data", 30)];
        let p_same = Path::new("/mnt/data/models");
        assert_eq!(free_bytes_for_path(&visible, &all, p_same), Some(30));

        let p_relative = Path::new("relative/path/models");
        assert_eq!(free_bytes_for_path(&visible, &all, p_relative), None);
    }

    #[test]
    fn macos_firmlink_and_apfs_root_fallback() {
        // Simulates macOS where / is filtered (read-only sealed snapshot with free space)
        // and /System/Volumes/Data is visible. User path /Users/jason/... is not under
        // /System/Volumes/Data directly, but matches / in all.
        let mut root_sealed = d("/", 200 * 1024 * 1024 * 1024);
        root_sealed.read_only = true;
        let data_vol = d(MACOS_DATA_VOLUME, 200 * 1024 * 1024 * 1024);
        let visible = vec![data_vol.clone()];
        let all = vec![root_sealed, data_vol];
        let p = Path::new("/Users/jason/.cache/models");
        assert_eq!(
            free_bytes_for_path(&visible, &all, p),
            Some(200 * 1024 * 1024 * 1024)
        );
    }

    /// Same component length in `visible` and `all` but different free bytes
    /// (e.g. a duplicated / bind mount entry): the user-visible one must win.
    /// The existing same-depth test uses equal values and cannot tell them apart.
    #[test]
    fn same_length_prefers_visible_with_distinct_values() {
        let visible = vec![d("/", 1), d("/srv/data", 10)];
        let all = vec![d("/", 1), d("/srv/data", 99)];
        let p = Path::new("/srv/data/models");
        assert_eq!(free_bytes_for_path(&visible, &all, p), Some(10));
    }

    /// Linux absolute-path boundary counterexamples (run on a real Linux host).
    #[cfg(target_os = "linux")]
    mod linux_paths {
        use super::*;

        #[test]
        fn linux_home_vs_homework_boundary_counterexample() {
            // `/homework` shares the string prefix `/home` but not the component.
            let visible = vec![d("/", 1), d("/home", 2)];
            let all = visible.clone();
            assert_eq!(
                free_bytes_for_path(&visible, &all, Path::new("/homework/models")),
                Some(1)
            );
            assert_eq!(
                free_bytes_for_path(&visible, &all, Path::new("/home/u/.omlx/models")),
                Some(2)
            );
            // Exact mount point itself belongs to that mount.
            assert_eq!(
                free_bytes_for_path(&visible, &all, Path::new("/home")),
                Some(2)
            );
        }

        #[test]
        fn linux_trailing_slash_and_repeated_separator_are_same_component_length() {
            let visible = vec![d("/", 1), d("/mnt/data/", 5)];
            let all = vec![d("/", 1), d("/mnt//data", 9)];
            // Both normalize to [/, mnt, data]: same length, visible wins.
            assert_eq!(
                free_bytes_for_path(&visible, &all, Path::new("/mnt/data/models")),
                Some(5)
            );
        }

        #[test]
        fn linux_deeper_filtered_tmpfs_and_overlay_mounts_win() {
            // Deeper mounts that `is_user_visible_volume` filters out still
            // own the path; `/` must not be reported for them.
            assert!(!is_user_visible_volume(
                "/var/lib/docker/overlay2/x/merged",
                "overlay",
                "overlay",
                100,
                false
            ));
            assert!(!is_user_visible_volume(
                "/run/user/1000",
                "tmpfs",
                "tmpfs",
                100,
                false
            ));
            let visible = vec![d("/", 50)];
            let all = vec![
                d("/", 50),
                d("/run/user/1000", 3),
                d("/var/lib/docker/overlay2/x/merged", 4),
            ];
            assert_eq!(
                free_bytes_for_path(&visible, &all, Path::new("/run/user/1000/m")),
                Some(3)
            );
            assert_eq!(
                free_bytes_for_path(
                    &visible,
                    &all,
                    Path::new("/var/lib/docker/overlay2/x/merged/m")
                ),
                Some(4)
            );
            // Sibling of the filtered mount falls back to `/`.
            assert_eq!(
                free_bytes_for_path(&visible, &all, Path::new("/run/user/10000/m")),
                Some(50)
            );
        }

        /// Windows-style strings are static fixtures here: on Linux they are a
        /// single relative component, so nothing matches. This is NOT a
        /// Windows run and proves nothing about Windows behaviour.
        #[test]
        fn windows_style_strings_are_static_fixture_only_on_linux() {
            let visible = vec![d("C:\\", 5)];
            let p = Path::new("C:\\Users\\me\\models");
            assert!(!p.is_absolute());
            assert_eq!(free_bytes_for_path(&visible, &visible, p), None);
        }
    }
}
