//! Hardware probing (D0-3, `docs/agent/PLAN-DECIDE.md` §1.1): deterministic
//! code, not a model. [`HardwareProbe`] is the seam — [`SystemProbe`] is the
//! real implementation (sysinfo, MIT-licensed, checked before adding), and
//! [`FakeHardwareProbe`] lets [`crate::tier::TierPolicy`] be tested against
//! exact boundary values without touching the real machine.

use serde::{Deserialize, Serialize};
use sysinfo::{Disks, System};

/// Whether a local inference accelerator is available. Detection is a coarse
/// heuristic, not a capability negotiation — good enough to pick a tier, not
/// good enough to promise a specific runtime will use it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Accelerator {
    Metal,
    Cuda,
    None,
}

/// A snapshot of the local machine, as far as [`crate::tier::TierPolicy`]
/// needs to know it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HardwareProfile {
    pub total_mem_bytes: u64,
    pub avail_mem_bytes: u64,
    pub arch: String,
    pub cpu_cores: u32,
    pub accelerator: Accelerator,
    pub free_disk_bytes: u64,
    pub os: String,
    /// `None` when the platform/probe cannot tell — callers must not treat
    /// `None` as "definitely on AC power", see `TierPolicy` docs for how it
    /// is actually handled.
    pub on_battery: Option<bool>,
}

/// The seam between [`crate::tier::TierPolicy`] (a pure function) and the
/// real machine. Exists so tests can inject exact boundary values instead of
/// depending on whatever hardware happens to run the test suite.
pub trait HardwareProbe {
    fn probe(&self) -> HardwareProfile;
}

/// Real probe, backed by `sysinfo` (MIT license, checked 2026-10-07 against
/// `docs/agent/PLAN-DECIDE.md`'s "核对其许可证为 MIT/Apache 才用").
///
/// Known gap: `on_battery` is always `None` here — `sysinfo` does not expose
/// power-source state and this crate does not shell out to a platform tool
/// for it. Not implemented, not pretended to be.
pub struct SystemProbe;

impl SystemProbe {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SystemProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl HardwareProbe for SystemProbe {
    fn probe(&self) -> HardwareProfile {
        let sys = System::new_all();
        let disks = Disks::new_with_refreshed_list();
        let free_disk_bytes = free_disk_for_cwd(&disks);

        HardwareProfile {
            total_mem_bytes: sys.total_memory(),
            avail_mem_bytes: sys.available_memory(),
            arch: std::env::consts::ARCH.to_owned(),
            cpu_cores: u32::try_from(sys.cpus().len()).unwrap_or(0),
            accelerator: detect_accelerator(),
            free_disk_bytes,
            os: std::env::consts::OS.to_owned(),
            on_battery: None,
        }
    }
}

/// macOS aarch64 is treated as having Metal; Linux with an nvidia kernel
/// module loaded (`/proc/driver/nvidia` present) is treated as having Cuda;
/// everything else (including macOS x86_64, Windows, Linux without nvidia)
/// is `None`. This is the exact mapping `PLAN-DECIDE.md` §1.1 asks for — it
/// is not a general GPU inventory.
fn detect_accelerator() -> Accelerator {
    if std::env::consts::OS == "macos" && std::env::consts::ARCH == "aarch64" {
        return Accelerator::Metal;
    }
    if std::env::consts::OS == "linux" && std::path::Path::new("/proc/driver/nvidia").exists() {
        return Accelerator::Cuda;
    }
    Accelerator::None
}

/// Finds the disk whose mount point is the longest matching prefix of the
/// current working directory (the classic "which filesystem is this path
/// on" walk), falling back to the disk with the most available space if
/// nothing matches (e.g. `current_dir()` failed).
fn free_disk_for_cwd(disks: &Disks) -> u64 {
    let cwd = std::env::current_dir().unwrap_or_default();
    let mut best: Option<(usize, u64)> = None;
    for disk in disks.list() {
        let mount = disk.mount_point();
        if cwd.starts_with(mount) {
            let depth = mount.as_os_str().len();
            if best.is_none_or(|(best_depth, _)| depth > best_depth) {
                best = Some((depth, disk.available_space()));
            }
        }
    }
    best.map(|(_, space)| space).unwrap_or_else(|| {
        disks
            .list()
            .iter()
            .map(sysinfo::Disk::available_space)
            .max()
            .unwrap_or(0)
    })
}

/// Test double: returns a fixed [`HardwareProfile`] regardless of the real
/// machine. Not gated behind `#[cfg(test)]` — D1 integration tests outside
/// this crate (e.g. `agent24d`) need it too.
pub struct FakeHardwareProbe(pub HardwareProfile);

impl HardwareProbe for FakeHardwareProbe {
    fn probe(&self) -> HardwareProfile {
        self.0.clone()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn system_probe_returns_plausible_values() {
        // Sanity bounds only — this runs on whatever machine CI/dev happens
        // to use, so it cannot assert exact numbers. TierPolicy's own tests
        // use FakeHardwareProbe for exact boundary values.
        let profile = SystemProbe::new().probe();
        assert!(
            profile.cpu_cores >= 1,
            "every real machine has at least one core"
        );
        assert!(profile.total_mem_bytes > 0);
        assert!(!profile.arch.is_empty());
        assert!(!profile.os.is_empty());
    }

    #[test]
    fn fake_probe_returns_exactly_what_it_was_given() {
        let profile = HardwareProfile {
            total_mem_bytes: 8 * 1024 * 1024 * 1024,
            avail_mem_bytes: 4 * 1024 * 1024 * 1024,
            arch: "aarch64".to_owned(),
            cpu_cores: 8,
            accelerator: Accelerator::Metal,
            free_disk_bytes: 100 * 1024 * 1024 * 1024,
            os: "macos".to_owned(),
            on_battery: Some(false),
        };
        let probe = FakeHardwareProbe(profile.clone());
        assert_eq!(probe.probe(), profile);
    }
}
