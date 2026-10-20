//! Historical (pre-HW-1) cwd-disk selection, kept only for v1 wire
//! compatibility (`IMPLEMENT-V1-COMPAT-SPEC.md` §3). This is not part of
//! `HardwareProfile` v2 and the value it returns must never feed
//! `TierPolicy`/`Budget` or models-dir download admission — it exists
//! solely so `agent24d`'s route can keep serializing the old required
//! `free_disk_bytes` field for clients that have not moved to
//! `models_dir_free_bytes`.

use std::path::{Path, PathBuf};

use sysinfo::Disks;

/// Disk-only enumeration (no `System`, no process/command collection) plus
/// the exact selection the removed `agent24-decide::hw::free_disk_for_cwd`
/// used: the disk whose mount point is the longest component-wise prefix of
/// the cwd wins; same-length prefixes keep the first one seen; no match
/// (including a failed `current_dir()`) falls back to the disk with the
/// most available space across the *unfiltered* list; an empty disk list is
/// `0`. `0` is also a legitimate "disk is full" value here — the v1 wire
/// never had a separate sentinel for "unknown".
pub fn legacy_cwd_free_bytes() -> u64 {
    let cwd = std::env::current_dir().ok();
    let disks = Disks::new_with_refreshed_list();
    let entries: Vec<(PathBuf, u64)> = disks
        .list()
        .iter()
        .map(|d| (d.mount_point().to_path_buf(), d.available_space()))
        .collect();
    select_legacy_free_bytes(cwd.as_deref(), &entries)
}

fn select_legacy_free_bytes(cwd: Option<&Path>, disks: &[(PathBuf, u64)]) -> u64 {
    let cwd = cwd.unwrap_or_else(|| Path::new(""));
    let mut best: Option<(usize, u64)> = None;
    for (mount, available) in disks {
        if cwd.starts_with(mount) {
            let depth = mount.as_os_str().len();
            if best.is_none_or(|(best_depth, _)| depth > best_depth) {
                best = Some((depth, *available));
            }
        }
    }
    best.map(|(_, space)| space)
        .unwrap_or_else(|| disks.iter().map(|(_, space)| *space).max().unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(mount: &str, free: u64) -> (PathBuf, u64) {
        (PathBuf::from(mount), free)
    }

    #[test]
    fn longest_prefix_wins_and_fallback_uses_max() {
        let disks = [d("/a", 5), d("/a/deep", 2), d("/ab", 99)];
        assert_eq!(
            select_legacy_free_bytes(Some(Path::new("/a/deep/x")), &disks),
            2
        );
        // "/ax" shares no component-wise prefix with "/a" or "/a/deep" ("ax"
        // != "a"), and not with "/ab" either ("ax" != "ab") — no match, so
        // it falls back to the max across the whole (unfiltered) list.
        assert_eq!(select_legacy_free_bytes(Some(Path::new("/ax")), &disks), 99);
    }

    #[test]
    fn cwd_none_falls_back_to_max_available() {
        let disks = [d("/a", 5), d("/a/deep", 2), d("/ab", 99)];
        assert_eq!(select_legacy_free_bytes(None, &disks), 99);
    }

    #[test]
    fn empty_disk_list_is_zero() {
        assert_eq!(select_legacy_free_bytes(None, &[]), 0);
        assert_eq!(select_legacy_free_bytes(Some(Path::new("/a")), &[]), 0);
    }

    #[test]
    fn tie_keeps_first_seen() {
        let disks = [d("/a", 7), d("/a", 9)];
        assert_eq!(select_legacy_free_bytes(Some(Path::new("/a")), &disks), 7);
    }

    #[test]
    fn prefix_is_component_wise_not_string_wise() {
        // "/Volumes/ExtraDisk" must not match "/Volumes/Ext" as a prefix —
        // mirrors `disks::prefix_is_component_wise_not_string_wise`, but
        // this selector takes raw (mount, free) pairs with no filtering.
        let disks = [d("/", 1), d("/Volumes/Ext", 2)];
        let p = Path::new("/Volumes/ExtraDisk/models");
        assert_eq!(select_legacy_free_bytes(Some(p), &disks), 1);
    }

    #[test]
    fn unfiltered_list_includes_would_be_filtered_mounts() {
        // Unlike `disks::is_user_visible_volume`, this selector never
        // filters — a read-only/system/zero-size mount still participates
        // if its path prefix matches, matching the pre-HW-1 behavior.
        let disks = [d("/System/Volumes/Preboot", 123)];
        assert_eq!(
            select_legacy_free_bytes(Some(Path::new("/System/Volumes/Preboot/x")), &disks),
            123
        );
    }

    #[test]
    fn real_helper_does_not_panic() {
        // Sanity only, like `probe::tests::system_probe_returns_plausible_values`:
        // disk-only enumeration against the real machine, no assertion on
        // the exact value since it depends on the runner's filesystem.
        let _ = legacy_cwd_free_bytes();
    }
}
