//! Hardware → tier policy (D0-3, `docs/agent/PLAN-DECIDE.md` §1.1). A pure
//! function: [`TierPolicy::decide`] takes a [`HardwareProfile`], a
//! [`DownloadConsent`], and an optional user override, and returns a
//! [`TierDecision`] — no I/O, no clock, no global state, so it is exactly as
//! testable as the boundary table in the plan doc.
//!
//! **Draft rules** (`PLAN-DECIDE.md` §1.1 explicitly marks the table as a
//! draft, finalized after D0-6's two-machine measurement): the thresholds
//! below are the ones in that table today. They are expected to change once
//! D0-6 has real P95/RSS numbers — that is exactly why they are constants in
//! one place, not scattered through call sites.

use serde::{Deserialize, Serialize};

use crate::hw::{Accelerator, HardwareProfile};

/// Below this free disk space, D0-3's rule is "don't even try" — see
/// `PLAN-DECIDE.md` §1.1 T0 row ("磁盘不足").
const MIN_FREE_DISK_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Below this total RAM, T0 (rules only) regardless of anything else.
const MIN_MEM_T1_BYTES: u64 = 8 * 1024 * 1024 * 1024;

/// At/above this total RAM (plus an accelerator), T2 is reachable.
const MIN_MEM_T2_BYTES: u64 = 16 * 1024 * 1024 * 1024;

/// At/above this total RAM (plus an accelerator), T3 is reachable.
const MIN_MEM_T3_BYTES: u64 = 32 * 1024 * 1024 * 1024;

/// A hardware/capability tier. Order matters: `T0 < T1 < T2 < T3`, so
/// "user override can only downgrade" is just a comparison, not a lookup
/// table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    T0,
    T1,
    T2,
    T3,
}

/// Whether the user has agreed to let Agent24 download decision-model
/// components. Not a `bool` parameter — `PLAN-DECIDE.md` §0 treats this as a
/// first-class, explicit gate (default is "not granted", see the
/// `A24_DECIDE_DOWNLOAD_CONSENT` env var in `agent24d`), and a stray `true`
/// at a call site should not be able to pass as "the user said yes" by
/// accident the way a bare `bool` argument can.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DownloadConsent {
    Granted,
    NotGranted,
}

/// The result of running [`TierPolicy::decide`]: what the hardware alone
/// could support, what will actually be used once consent and the user's
/// own override are applied, and a human-readable trail of why.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TierDecision {
    pub hardware_tier: Tier,
    pub effective_tier: Tier,
    pub reasons: Vec<String>,
}

/// Pure hardware → tier mapping. No [`Default`] impl, deliberately — every
/// call must pass the three inputs explicitly; there's nothing left over
/// that would make sense to default (unlike `ThresholdBands`, this isn't a
/// per-decision-point config, it's one function of the whole machine's
/// state, so there is no instance to construct in the first place).
pub struct TierPolicy;

impl TierPolicy {
    pub fn decide(
        profile: &HardwareProfile,
        consent: DownloadConsent,
        user_override: Option<Tier>,
    ) -> TierDecision {
        let (hardware_tier, mut reasons) = Self::hardware_tier(profile);

        let mut effective_tier = match consent {
            DownloadConsent::NotGranted => {
                reasons.push("未同意下载组件，锁定 T0".to_owned());
                Tier::T0
            }
            DownloadConsent::Granted => hardware_tier,
        };

        if let Some(requested) = user_override {
            if requested < effective_tier {
                reasons.push(format!("用户覆盖降档：{effective_tier:?} → {requested:?}"));
                effective_tier = requested;
            } else if requested > effective_tier {
                reasons.push(format!(
                    "用户覆盖请求 {requested:?} 高于允许档位 {effective_tier:?}：忽略（覆盖只能降档，不能升档）"
                ));
            }
        }

        TierDecision {
            hardware_tier,
            effective_tier,
            reasons,
        }
    }

    /// What the hardware alone supports, ignoring consent and user override
    /// entirely — this is `TierDecision::hardware_tier`.
    fn hardware_tier(profile: &HardwareProfile) -> (Tier, Vec<String>) {
        let mut reasons = Vec::new();

        if profile.free_disk_bytes < MIN_FREE_DISK_BYTES {
            reasons.push(format!(
                "可用磁盘 {} bytes < {} bytes（2GB）：T0",
                profile.free_disk_bytes, MIN_FREE_DISK_BYTES
            ));
            return (Tier::T0, reasons);
        }

        if profile.total_mem_bytes < MIN_MEM_T1_BYTES {
            reasons.push(format!(
                "总内存 {} bytes < {} bytes（8GB）：T0",
                profile.total_mem_bytes, MIN_MEM_T1_BYTES
            ));
            return (Tier::T0, reasons);
        }

        let mem_tier = if profile.total_mem_bytes >= MIN_MEM_T3_BYTES {
            Tier::T3
        } else if profile.total_mem_bytes >= MIN_MEM_T2_BYTES {
            Tier::T2
        } else {
            Tier::T1
        };
        reasons.push(format!(
            "内存档位 {mem_tier:?}（总内存 {} bytes）",
            profile.total_mem_bytes
        ));

        let mut tier = mem_tier;

        if profile.accelerator == Accelerator::None {
            reasons.push("无加速器：上限 T1".to_owned());
            tier = tier.min(Tier::T1);
        }

        // `on_battery == None` (platform/probe can't tell) is treated the
        // same as `Some(false)` — the cap only fires on a *confirmed*
        // battery signal, not on "we don't know". See `HardwareProfile`
        // docs for why `SystemProbe` reports `None` today.
        if profile.on_battery == Some(true) {
            reasons.push("电池供电：上限 T1".to_owned());
            tier = tier.min(Tier::T1);
        }

        (tier, reasons)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::hw::Accelerator;

    fn profile(
        total_mem_gb: u64,
        accelerator: Accelerator,
        free_disk_gb: u64,
        on_battery: Option<bool>,
    ) -> HardwareProfile {
        HardwareProfile {
            total_mem_bytes: total_mem_gb * 1024 * 1024 * 1024,
            avail_mem_bytes: total_mem_gb * 1024 * 1024 * 1024 / 2,
            arch: "aarch64".to_owned(),
            cpu_cores: 8,
            accelerator,
            free_disk_bytes: free_disk_gb * 1024 * 1024 * 1024,
            os: "macos".to_owned(),
            on_battery,
        }
    }

    #[test]
    fn no_consent_is_stable_t0_regardless_of_hardware() {
        // A machine that would otherwise qualify for T3...
        let beefy = profile(64, Accelerator::Metal, 500, Some(false));
        let decision = TierPolicy::decide(&beefy, DownloadConsent::NotGranted, None);
        assert_eq!(
            decision.hardware_tier,
            Tier::T3,
            "hardware_tier reports capability, ignoring consent"
        );
        assert_eq!(
            decision.effective_tier,
            Tier::T0,
            "no consent must still lock effective_tier to T0"
        );
    }

    #[test]
    fn disk_below_2gb_is_t0() {
        let p = profile(64, Accelerator::Metal, 1, Some(false));
        let d = TierPolicy::decide(&p, DownloadConsent::Granted, None);
        assert_eq!(d.hardware_tier, Tier::T0);
        assert_eq!(d.effective_tier, Tier::T0);
    }

    #[test]
    fn disk_exactly_2gb_is_not_floored_to_t0_by_disk_alone() {
        let p = profile(64, Accelerator::Metal, 2, Some(false));
        let d = TierPolicy::decide(&p, DownloadConsent::Granted, None);
        assert_eq!(
            d.hardware_tier,
            Tier::T3,
            "2GB exactly satisfies >= the 2GB floor"
        );
    }

    #[test]
    fn mem_below_8gb_is_t0() {
        let p = profile(7, Accelerator::Metal, 100, Some(false));
        let d = TierPolicy::decide(&p, DownloadConsent::Granted, None);
        assert_eq!(d.hardware_tier, Tier::T0);
    }

    #[test]
    fn mem_exactly_8gb_with_accelerator_is_t1() {
        let p = profile(8, Accelerator::Metal, 100, Some(false));
        let d = TierPolicy::decide(&p, DownloadConsent::Granted, None);
        assert_eq!(d.hardware_tier, Tier::T1);
    }

    #[test]
    fn mem_just_below_16gb_is_t1() {
        let p = profile(15, Accelerator::Metal, 100, Some(false));
        let d = TierPolicy::decide(&p, DownloadConsent::Granted, None);
        assert_eq!(d.hardware_tier, Tier::T1);
    }

    #[test]
    fn mem_exactly_16gb_with_accelerator_is_t2() {
        let p = profile(16, Accelerator::Metal, 100, Some(false));
        let d = TierPolicy::decide(&p, DownloadConsent::Granted, None);
        assert_eq!(d.hardware_tier, Tier::T2);
    }

    #[test]
    fn mem_just_below_32gb_is_t2() {
        let p = profile(31, Accelerator::Metal, 100, Some(false));
        let d = TierPolicy::decide(&p, DownloadConsent::Granted, None);
        assert_eq!(d.hardware_tier, Tier::T2);
    }

    #[test]
    fn mem_exactly_32gb_with_accelerator_is_t3() {
        let p = profile(32, Accelerator::Metal, 100, Some(false));
        let d = TierPolicy::decide(&p, DownloadConsent::Granted, None);
        assert_eq!(d.hardware_tier, Tier::T3);
    }

    #[test]
    fn no_accelerator_caps_at_t1_even_with_64gb() {
        let p = profile(64, Accelerator::None, 100, Some(false));
        let d = TierPolicy::decide(&p, DownloadConsent::Granted, None);
        assert_eq!(d.hardware_tier, Tier::T1);
    }

    #[test]
    fn on_battery_caps_at_t1_even_with_64gb_and_accelerator() {
        let p = profile(64, Accelerator::Metal, 100, Some(true));
        let d = TierPolicy::decide(&p, DownloadConsent::Granted, None);
        assert_eq!(d.hardware_tier, Tier::T1);
    }

    #[test]
    fn unknown_battery_state_does_not_cap_the_tier() {
        let p = profile(64, Accelerator::Metal, 100, None);
        let d = TierPolicy::decide(&p, DownloadConsent::Granted, None);
        assert_eq!(
            d.hardware_tier,
            Tier::T3,
            "None must not be treated as on_battery=true"
        );
    }

    #[test]
    fn user_override_can_downgrade() {
        let p = profile(64, Accelerator::Metal, 100, Some(false));
        let d = TierPolicy::decide(&p, DownloadConsent::Granted, Some(Tier::T1));
        assert_eq!(d.hardware_tier, Tier::T3);
        assert_eq!(d.effective_tier, Tier::T1);
    }

    #[test]
    fn user_override_cannot_upgrade_past_hardware_tier() {
        let p = profile(8, Accelerator::None, 100, Some(false)); // hardware_tier T1
        let d = TierPolicy::decide(&p, DownloadConsent::Granted, Some(Tier::T3));
        assert_eq!(d.hardware_tier, Tier::T1);
        assert_eq!(
            d.effective_tier,
            Tier::T1,
            "override must not raise above hardware_tier"
        );
    }

    #[test]
    fn user_override_cannot_upgrade_past_a_consent_gated_t0() {
        let p = profile(64, Accelerator::Metal, 100, Some(false));
        let d = TierPolicy::decide(&p, DownloadConsent::NotGranted, Some(Tier::T3));
        assert_eq!(
            d.effective_tier,
            Tier::T0,
            "override must not raise above the consent gate either"
        );
    }

    #[test]
    fn tier_ordering_is_t0_lt_t1_lt_t2_lt_t3() {
        assert!(Tier::T0 < Tier::T1);
        assert!(Tier::T1 < Tier::T2);
        assert!(Tier::T2 < Tier::T3);
    }
}
