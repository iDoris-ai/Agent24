//! Agent24 hardware probe + memory budget (HW-1).
//!
//! One machine, one prober, one memory ledger (ADR-036): this crate owns
//! [`HardwareProfile`] v2, the [`HardwareProbe`] seam, and the pure
//! [`Budget`]/[`HwLevel`] computation. It depends on no other internal crate
//! so iDoris can depend on it directly.
//!
//! External commands (`sysctl`, `sw_vers`, `pmset`, `nvidia-smi`) all run
//! with a 2 s timeout and degrade to `None` on any failure — a field being
//! `None` is a first-class state (design §3.2), never a panic.

mod budget;
mod disks;
mod legacy_disk;
mod parse;
mod probe;

use serde::{Deserialize, Serialize};

pub use budget::{APP_OVERHEAD_BYTES, BUDGET_CAP_ENV, Budget, HwLevel, usable_for_ram};
pub use disks::{disk_for_path, free_bytes_for_path, is_user_visible_volume};
pub use legacy_disk::legacy_cwd_free_bytes;
pub use parse::{
    PowerSupply, on_battery_from_power_supplies, parse_brand_string, parse_nvidia_smi,
    parse_pmset_batt, parse_product_version, parse_wired_limit_mb,
};
pub use probe::{FakeHardwareProbe, HardwareProbe, SystemProbe, default_models_dir};

/// Whether a local inference accelerator is available. Coarse heuristic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Accelerator {
    Metal,
    Cuda,
    None,
}

/// A discrete GPU (Linux/Windows).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuInfo {
    pub name: String,
    pub vram_bytes: Option<u64>,
}

/// One mounted, user-visible volume (internal or external).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiskInfo {
    pub name: String,
    pub mount_point: String,
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub removable: bool,
    pub read_only: bool,
}

/// A detected inference runtime (not probed in HW-1; reserved).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeInfo {
    pub name: String,
    pub version: Option<String>,
}

/// Snapshot of the local machine (v2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HardwareProfile {
    pub total_mem_bytes: u64,
    pub avail_mem_bytes: u64,
    pub arch: String,
    pub cpu_cores: u32,
    pub accelerator: Accelerator,
    pub os: String,
    #[serde(default)]
    pub os_version: Option<String>,
    #[serde(default)]
    pub chip: Option<String>,
    #[serde(default)]
    pub unified_memory: bool,
    #[serde(default)]
    pub gpu_wired_limit_bytes: Option<u64>,
    #[serde(default)]
    pub discrete_gpus: Vec<GpuInfo>,
    /// All user-visible mounted volumes (read-only system volumes and APFS
    /// helper volumes filtered out).
    #[serde(default)]
    pub disks: Vec<DiskInfo>,
    /// Free bytes on the disk holding the models directory; `None` when it
    /// cannot be determined (new downloads must then be refused).
    #[serde(default)]
    pub models_dir_free_bytes: Option<u64>,
    /// `None` = unknown; must not be read as "on AC power" nor as battery.
    #[serde(default)]
    pub on_battery: Option<bool>,
    #[serde(default)]
    pub runtimes: Vec<RuntimeInfo>,
}

/// What `agent24d` persists to `hardware.json` and `agent24 hw probe` prints:
/// the profile plus the derived level / usable budget and the reasons.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HardwareReport {
    pub profile: HardwareProfile,
    pub level: HwLevel,
    pub usable_bytes: u64,
    pub reasons: Vec<String>,
}

impl HardwareReport {
    /// Build from a profile and an (already read) `A24_HW_BUDGET_CAP_GB`.
    pub fn new(profile: HardwareProfile, cap_gb_raw: Option<&str>) -> Self {
        let b = Budget::compute(&profile, cap_gb_raw);
        Self {
            profile,
            level: b.level,
            usable_bytes: b.usable_bytes,
            reasons: b.reasons,
        }
    }

    /// Probe now and read the cap from the process environment.
    pub fn from_env(probe: &dyn HardwareProbe) -> Self {
        let raw = std::env::var(BUDGET_CAP_ENV).ok();
        Self::new(probe.probe(), raw.as_deref())
    }
}
