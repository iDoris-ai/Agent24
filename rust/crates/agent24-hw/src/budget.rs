//! Memory budget + HwLevel (design §3.3). Pure functions; the only env
//! access is the thin [`Budget::from_env`] wrapper.

use serde::{Deserialize, Serialize};

use crate::HardwareProfile;

const GIB: u64 = 1024 * 1024 * 1024;

/// Memory Agent24 itself (daemon + desktop) is assumed to use. Draft value,
/// to be replaced by a measured one (design §3.3).
pub const APP_OVERHEAD_BYTES: u64 = GIB;

/// Env var that clamps the effective memory (GiB) — simulates smaller
/// machines on one box.
pub const BUDGET_CAP_ENV: &str = "A24_HW_BUDGET_CAP_GB";

/// User-facing hardware level 1..6. Deliberately not called `Tier` (routing
/// `Tier` / decision `T0..T3`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HwLevel {
    L1,
    L2,
    L3,
    L4,
    L5,
    L6,
}

impl HwLevel {
    /// Level by total memory: <8→L1, 8–<16→L2, 16–<24→L3, 24–<32→L4,
    /// 32–<64→L5, ≥64→L6 (GiB).
    pub fn from_total_bytes(total: u64) -> Self {
        let gib = total / GIB; // floor: 7.99 GiB → 7
        match gib {
            0..=7 => Self::L1,
            8..=15 => Self::L2,
            16..=23 => Self::L3,
            24..=31 => Self::L4,
            32..=63 => Self::L5,
            _ => Self::L6,
        }
    }

    pub fn number(self) -> u8 {
        match self {
            Self::L1 => 1,
            Self::L2 => 2,
            Self::L3 => 3,
            Self::L4 => 4,
            Self::L5 => 5,
            Self::L6 => 6,
        }
    }
}

/// Result of the budget computation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budget {
    pub level: HwLevel,
    /// Memory (bytes) actually considered after any env cap.
    pub effective_mem_bytes: u64,
    pub usable_bytes: u64,
    pub reasons: Vec<String>,
}

impl Budget {
    /// Compute from a profile, reading [`BUDGET_CAP_ENV`] from the process
    /// environment.
    pub fn from_env(profile: &HardwareProfile) -> Self {
        let raw = std::env::var(BUDGET_CAP_ENV).ok();
        Self::compute(profile, raw.as_deref())
    }

    /// Pure core. `cap_gb_raw` is the unparsed value of
    /// `A24_HW_BUDGET_CAP_GB`; unparsable values are ignored and recorded
    /// in `reasons`.
    pub fn compute(profile: &HardwareProfile, cap_gb_raw: Option<&str>) -> Self {
        let mut reasons = Vec::new();
        let mut ram = profile.total_mem_bytes;

        if let Some(raw) = cap_gb_raw {
            match raw.trim().parse::<u64>() {
                Ok(gb) if gb > 0 => match gb.checked_mul(GIB) {
                    Some(cap) if cap < ram => {
                        reasons.push(format!(
                            "{BUDGET_CAP_ENV}={gb}：有效内存由 {ram} 压到 {cap} bytes"
                        ));
                        ram = cap;
                    }
                    _ => reasons.push(format!("{BUDGET_CAP_ENV}={gb} 不低于实际内存：无效果")),
                },
                _ => reasons.push(format!("{BUDGET_CAP_ENV}={raw:?} 不是正整数：已忽略")),
            }
        }

        let level = HwLevel::from_total_bytes(ram);
        let mut usable = usable_for_ram(ram);

        if let Some(wired) = profile.gpu_wired_limit_bytes.filter(|w| *w > 0) {
            let capped = wired.saturating_sub(APP_OVERHEAD_BYTES);
            if capped < usable {
                reasons.push(format!(
                    "GPU wired 上限 {wired} bytes 低于默认预算：usable 压到 {capped}"
                ));
                usable = capped;
            }
        }

        Self {
            level,
            effective_mem_bytes: ram,
            usable_bytes: usable,
            reasons,
        }
    }
}

/// `min(RAM*0.66, RAM - reserve) - app_overhead`, `reserve = clamp(RAM*0.30, 3GiB, 16GiB)`.
pub fn usable_for_ram(ram: u64) -> u64 {
    let r = u128::from(ram);
    let reserve = (r * 30 / 100).clamp(u128::from(3 * GIB), u128::from(16 * GIB));
    let by_ratio = r * 66 / 100;
    let by_reserve = r.saturating_sub(reserve);
    let usable = by_ratio
        .min(by_reserve)
        .saturating_sub(u128::from(APP_OVERHEAD_BYTES));
    u64::try_from(usable).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Accelerator;

    fn profile(total: u64) -> HardwareProfile {
        HardwareProfile {
            total_mem_bytes: total,
            avail_mem_bytes: total / 2,
            arch: "aarch64".to_owned(),
            cpu_cores: 8,
            accelerator: Accelerator::Metal,
            os: "macos".to_owned(),
            os_version: None,
            chip: None,
            unified_memory: true,
            gpu_wired_limit_bytes: None,
            discrete_gpus: vec![],
            disks: vec![],
            models_dir_free_bytes: None,
            on_battery: None,
            runtimes: vec![],
        }
    }

    fn gib_f(g: f64) -> u64 {
        (g * GIB as f64) as u64
    }

    #[test]
    fn level_boundaries() {
        let cases: &[(f64, HwLevel)] = &[
            (0.0, HwLevel::L1),
            (7.99, HwLevel::L1),
            (8.0, HwLevel::L2),
            (15.99, HwLevel::L2),
            (16.0, HwLevel::L3),
            (23.99, HwLevel::L3),
            (24.0, HwLevel::L4),
            (31.99, HwLevel::L4),
            (32.0, HwLevel::L5),
            (63.99, HwLevel::L5),
            (64.0, HwLevel::L6),
            (96.0, HwLevel::L6),
        ];
        for (g, expected) in cases {
            assert_eq!(HwLevel::from_total_bytes(gib_f(*g)), *expected, "{g} GiB");
        }
    }

    #[test]
    fn usable_table_matches_design() {
        // (GiB, expected usable GiB, tolerance)
        let cases: &[(u64, f64, HwLevel)] = &[
            (8, 4.0, HwLevel::L2),
            (16, 9.56, HwLevel::L3),
            (18, 10.88, HwLevel::L3),
            (24, 14.84, HwLevel::L4),
            (32, 20.12, HwLevel::L5),
            (36, 22.76, HwLevel::L5),
            (48, 30.68, HwLevel::L5),
            (64, 41.24, HwLevel::L6),
            (96, 62.36, HwLevel::L6),
        ];
        for (g, expected, level) in cases {
            let b = Budget::compute(&profile(g * GIB), None);
            assert_eq!(b.level, *level, "{g} GiB level");
            let got = b.usable_bytes as f64 / GIB as f64;
            assert!(
                (got - expected).abs() < 0.02,
                "{g} GiB: got {got}, want {expected}"
            );
        }
    }

    #[test]
    fn usable_never_underflows() {
        assert_eq!(usable_for_ram(0), 0);
        assert_eq!(usable_for_ram(GIB / 2), 0);
        assert!(usable_for_ram(u64::MAX) > 0);
    }

    #[test]
    fn wired_limit_lowers_usable() {
        let mut p = profile(64 * GIB);
        p.gpu_wired_limit_bytes = Some(20 * GIB);
        let b = Budget::compute(&p, None);
        assert_eq!(b.usable_bytes, 19 * GIB);
        assert!(b.reasons.iter().any(|r| r.contains("wired")));
        // A wired limit above the default budget changes nothing.
        p.gpu_wired_limit_bytes = Some(60 * GIB);
        assert_eq!(
            Budget::compute(&p, None).usable_bytes,
            Budget::compute(&profile(64 * GIB), None).usable_bytes
        );
        // 0 = system default.
        p.gpu_wired_limit_bytes = Some(0);
        assert_eq!(
            Budget::compute(&p, None).usable_bytes,
            Budget::compute(&profile(64 * GIB), None).usable_bytes
        );
    }

    #[test]
    fn cap_env() {
        let p = profile(64 * GIB);
        let b = Budget::compute(&p, Some("8"));
        assert_eq!(b.level, HwLevel::L2);
        assert_eq!(b.effective_mem_bytes, 8 * GIB);
        assert_eq!(b.usable_bytes, usable_for_ram(8 * GIB));
        // Cap above real RAM: no effect.
        let b = Budget::compute(&p, Some("128"));
        assert_eq!(b.level, HwLevel::L6);
        assert_eq!(b.effective_mem_bytes, 64 * GIB);
    }

    #[test]
    fn bad_cap_is_ignored_and_recorded() {
        let p = profile(16 * GIB);
        for raw in ["abc", "", "-3", "0", "1.5", "99999999999999999999"] {
            let b = Budget::compute(&p, Some(raw));
            assert_eq!(b.level, HwLevel::L3, "{raw:?}");
            assert_eq!(b.effective_mem_bytes, 16 * GIB);
            assert!(!b.reasons.is_empty(), "{raw:?} must be noted");
        }
    }
}
