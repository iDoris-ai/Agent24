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

#[derive(Debug, Serialize)]
struct DecideProfileResponse {
    profile: agent24_decide::HardwareProfile,
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
    let decision = TierPolicy::decide(&profile, consent, None);

    Json(DecideProfileResponse {
        profile,
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
}
