//! `GET /api/v1/decide/profile` (D0-3, `docs/agent/PLAN-DECIDE.md` §1.1):
//! reports the local hardware profile plus the hardware/effective decision
//! tier. Read-only, no state — probing is cheap enough (a few syscalls) to
//! do fresh on every call rather than cache it in [`crate::server::AppState`]
//! and risk serving a stale tier after a hardware change (plug in a GPU,
//! plug out AC power).
//!
//! Download consent is not yet a per-user setting (`PLAN-DECIDE.md` §1.1
//! "用户可见、可覆盖" is D1+ UI work) — D0-3 reads it from
//! `A24_DECIDE_DOWNLOAD_CONSENT`, defaulting to **not granted** when unset
//! or unrecognized, matching `PLAN-DECIDE.md` §0's "不静默降级" posture: an
//! ambiguous env var must not be read as "the user said yes".

use agent24_decide::{DownloadConsent, HardwareProbe, SystemProbe, TierPolicy};
use axum::response::{IntoResponse, Json, Response};
use serde::Serialize;

const CONSENT_ENV_VAR: &str = "A24_DECIDE_DOWNLOAD_CONSENT";

/// Route-local v1 wire projection of [`agent24_decide::HardwareProfile`]
/// (`IMPLEMENT-V1-COMPAT-SPEC.md` §2): flattens the v2 profile and restores
/// the historical required `free_disk_bytes` field so pre-HW-1 clients keep
/// decoding. Exists only at serialization time — `free_disk_bytes` never
/// feeds `TierPolicy`/`Budget`/models-dir admission, and `profile`'s own
/// `models_dir_free_bytes` is a separate field with a separate meaning.
#[derive(Debug, Serialize)]
struct HardwareProfileV1 {
    #[serde(flatten)]
    profile: agent24_decide::HardwareProfile,
    free_disk_bytes: u64,
}

#[derive(Debug, Serialize)]
struct DecideProfileResponse {
    profile: HardwareProfileV1,
    hardware_tier: agent24_decide::Tier,
    effective_tier: agent24_decide::Tier,
    reasons: Vec<String>,
}

/// Only an exact `"1"` or case-insensitive `"true"` counts as granted.
/// Anything else — unset, empty, `"0"`, `"false"`, a typo — is NotGranted.
/// There is no separate "yes" variant that defaults open.
///
/// Takes the already-read value rather than reading `std::env` itself, on
/// purpose: `std::env::set_var`/`remove_var` are `unsafe` as of the 2024
/// edition, and this workspace forbids `unsafe_code` crate-wide, so a test
/// cannot set the process environment to exercise this. Splitting "read the
/// env var" (one untested line, [`get_decide_profile`]) from "parse what it
/// said" (this function) keeps the parsing fully testable without needing
/// `unsafe` anywhere.
fn parse_download_consent(raw: Option<&str>) -> DownloadConsent {
    match raw {
        Some(value) if value == "1" || value.eq_ignore_ascii_case("true") => {
            DownloadConsent::Granted
        }
        _ => DownloadConsent::NotGranted,
    }
}

pub async fn get_decide_profile() -> Response {
    let profile = SystemProbe::new().probe();
    let consent = parse_download_consent(std::env::var(CONSENT_ENV_VAR).ok().as_deref());
    // Tier/consent decide against the original v2 profile, before it is
    // wrapped for the wire — the v1 projection below must never change the
    // decision.
    let decision = TierPolicy::decide(&profile, consent, None);
    let free_disk_bytes = agent24_hw::legacy_cwd_free_bytes();

    Json(DecideProfileResponse {
        profile: HardwareProfileV1 {
            profile,
            free_disk_bytes,
        },
        hardware_tier: decision.hardware_tier,
        effective_tier: decision.effective_tier,
        reasons: decision.reasons,
    })
    .into_response()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn unset_env_var_is_not_granted() {
        assert_eq!(parse_download_consent(None), DownloadConsent::NotGranted);
    }

    #[test]
    fn explicit_true_or_one_is_granted() {
        assert_eq!(
            parse_download_consent(Some("true")),
            DownloadConsent::Granted
        );
        assert_eq!(
            parse_download_consent(Some("TRUE")),
            DownloadConsent::Granted
        );
        assert_eq!(parse_download_consent(Some("1")), DownloadConsent::Granted);
    }

    #[test]
    fn anything_else_is_not_granted_including_typos() {
        assert_eq!(
            parse_download_consent(Some("0")),
            DownloadConsent::NotGranted
        );
        assert_eq!(
            parse_download_consent(Some("false")),
            DownloadConsent::NotGranted
        );
        assert_eq!(
            parse_download_consent(Some("yes")),
            DownloadConsent::NotGranted,
            "typos/near-misses must not default open"
        );
        assert_eq!(
            parse_download_consent(Some("")),
            DownloadConsent::NotGranted
        );
    }

    const GIB: u64 = 1 << 30;

    fn sample_profile() -> agent24_decide::HardwareProfile {
        agent24_decide::HardwareProfile {
            total_mem_bytes: 64 * GIB,
            avail_mem_bytes: GIB,
            arch: "aarch64".to_owned(),
            cpu_cores: 8,
            accelerator: agent24_decide::Accelerator::Metal,
            os: "macos".to_owned(),
            os_version: None,
            chip: None,
            unified_memory: false,
            gpu_wired_limit_bytes: None,
            discrete_gpus: Vec::new(),
            disks: Vec::new(),
            models_dir_free_bytes: Some(8 * GIB),
            on_battery: None,
            runtimes: Vec::new(),
        }
    }

    // Historical evidence, not a live regression target: `HardwareProfile`
    // (v2) intentionally never grows a `free_disk_bytes` field — that is
    // exactly why `HardwareProfileV1` exists as a route-local wrapper.
    // Before this slice, `DecideProfileResponse.profile` *was* a bare
    // `HardwareProfile`, so this was the route's real JSON output and it
    // was missing the v1-required key. Captured as a genuine RED run
    // (exit 101) against the exact pre-fix HEAD
    // `622be5d7df0387b91028820624a778cb99cd6d43` before this test suite was
    // added; see `/tmp/a24-hw-preview-hardening/v1-compat-implementation/red.log`.
    #[test]
    fn red_baseline_bare_v2_profile_has_no_legacy_free_disk_bytes() {
        let v = serde_json::to_value(sample_profile()).unwrap();
        assert!(v.get("free_disk_bytes").is_none());
    }

    #[test]
    fn v1_dto_restores_old_required_fields_and_keeps_all_v2_additions() {
        let v = serde_json::to_value(HardwareProfileV1 {
            profile: sample_profile(),
            free_disk_bytes: 3 * GIB,
        })
        .unwrap();
        for key in [
            "total_mem_bytes",
            "avail_mem_bytes",
            "arch",
            "cpu_cores",
            "accelerator",
            "free_disk_bytes",
            "os",
        ] {
            assert!(!v[key].is_null(), "{key} must be present and non-null");
        }
        for key in [
            "os_version",
            "chip",
            "unified_memory",
            "gpu_wired_limit_bytes",
            "discrete_gpus",
            "disks",
            "models_dir_free_bytes",
            "on_battery",
            "runtimes",
        ] {
            assert!(v.get(key).is_some(), "{key} must still be present");
        }
        // `on_battery` is nullable and the fixture leaves it unknown: it
        // must appear as an explicit `null`, not be skipped.
        assert_eq!(v["on_battery"], serde_json::Value::Null);
        assert_eq!(v["free_disk_bytes"], 3 * GIB);
    }

    #[test]
    fn legacy_client_can_still_decode_the_v1_required_fields() {
        // A pre-HW-1 client only ever knew about these seven fields; serde
        // ignores the rest by default (no `deny_unknown_fields`), so it
        // keeps decoding the v2+v1 combined wire payload.
        #[derive(serde::Deserialize)]
        struct OldClientHardwareProfile {
            total_mem_bytes: u64,
            #[allow(dead_code)]
            avail_mem_bytes: u64,
            #[allow(dead_code)]
            arch: String,
            #[allow(dead_code)]
            cpu_cores: u32,
            #[allow(dead_code)]
            accelerator: agent24_decide::Accelerator,
            #[allow(dead_code)]
            os: String,
            free_disk_bytes: u64,
        }
        let v = serde_json::to_value(HardwareProfileV1 {
            profile: sample_profile(),
            free_disk_bytes: 3 * GIB,
        })
        .unwrap();
        let decoded: OldClientHardwareProfile = serde_json::from_value(v).unwrap();
        assert_eq!(decoded.free_disk_bytes, 3 * GIB);
        assert_eq!(decoded.total_mem_bytes, 64 * GIB);
    }

    #[test]
    fn free_disk_bytes_and_models_dir_free_bytes_are_never_swapped() {
        // cwd on one disk (legacy), models dir on another (v2) — the two
        // must stay independent, wire values 3 GiB vs 8 GiB respectively.
        let v = serde_json::to_value(HardwareProfileV1 {
            profile: sample_profile(), // models_dir_free_bytes = Some(8 GiB)
            free_disk_bytes: 3 * GIB,
        })
        .unwrap();
        assert_eq!(v["free_disk_bytes"], 3 * GIB);
        assert_eq!(v["models_dir_free_bytes"], 8 * GIB);
    }

    #[test]
    fn legacy_value_never_changes_the_tier_or_consent_decision() {
        let profile = sample_profile();
        // Budget, like TierPolicy, only sees the original profile. The
        // legacy integer exists only on the wire wrapper.
        let budget_before = agent24_decide::Budget::compute(&profile, None);
        for consent in [DownloadConsent::NotGranted, DownloadConsent::Granted] {
            let decision_before = TierPolicy::decide(&profile, consent, None);
            for legacy_free in [0, 999 * GIB] {
                // The route computes `decision` from `profile` BEFORE
                // wrapping it into `HardwareProfileV1` (see
                // `get_decide_profile`) — `HardwareProfileV1` is not even
                // `Deserialize`, so there is no path back into
                // `TierPolicy::decide` or `Budget::compute`. Re-assert that
                // invariant directly: wrapping with two very different
                // legacy values must not change either decision.
                let wrapped = HardwareProfileV1 {
                    profile: profile.clone(),
                    free_disk_bytes: legacy_free,
                };
                let decision_from_wrapped_source =
                    TierPolicy::decide(&wrapped.profile, consent, None);
                assert_eq!(decision_before, decision_from_wrapped_source);
                assert_eq!(
                    budget_before,
                    agent24_decide::Budget::compute(&wrapped.profile, None),
                    "legacy free_disk_bytes must not change Budget"
                );
            }
        }
    }

    #[test]
    fn unknown_or_low_models_dir_free_bytes_stays_t0_regardless_of_legacy_value() {
        for models_free in [None, Some(0), Some(2 * GIB - 1)] {
            let mut profile = sample_profile();
            profile.models_dir_free_bytes = models_free;
            let decision = TierPolicy::decide(&profile, DownloadConsent::Granted, None);
            assert_eq!(decision.effective_tier, agent24_decide::Tier::T0);
            let v = serde_json::to_value(HardwareProfileV1 {
                profile,
                free_disk_bytes: 999 * GIB,
            })
            .unwrap();
            assert_eq!(
                v["models_dir_free_bytes"],
                serde_json::to_value(models_free).unwrap()
            );
        }
    }
}
