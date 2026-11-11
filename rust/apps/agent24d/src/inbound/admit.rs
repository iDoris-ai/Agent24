//! Deterministic inbound admission (plan §3.3) and the policy-version hash (§3.1).
//!
//! [`admit`] walks the seven rows top to bottom and returns on the first hit.
//! `accept_mode` is not an argument: §3.4 stores it and feeds it to
//! [`policy_version`] only. There is no `mode_off` reason. Explicit still
//! admits under every stored mode (ADR-041 D4); this slice simply does not
//! branch on the mode.
//!
//! 评审 N2：本函数只判断滑动窗口速率（[`AdmitInput::window_count`] 与
//! [`AdmitInput::rate_limit`]）。「同一 `source+actor` 在途最多 1 个 explicit」
//! 不在这里判断，由 `inbound_events` 的部分唯一索引 `idx_inbound_events_inflight`
//!（`disposition = 'explicit' AND run_state IN ('starting','started')`）强制。
//! 撞上该索引后由服务改写成 `ignore(rate_limited)`（§3.4，X11-2c 接线）。
//! 调用方不要把在途个数加进 `window_count`。

use sha2::{Digest, Sha256};

/// §3.3 table version. Bump when a row's condition or result changes.
/// Mixed into [`policy_version`] so a rule change invalidates stored decisions.
pub(crate) const ADMIT_RULE_VERSION: u32 = 1;

/// Wire `source`. `scheduler` and `self` do not go through [`admit`] (§3.4);
/// they exist so a policy hash can name the same sources as the table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    Hyphae,
    Wechat,
    Remote,
    Scheduler,
    Self_,
}

impl Source {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Hyphae => "hyphae",
            Self::Wechat => "wechat",
            Self::Remote => "remote",
            Self::Scheduler => "scheduler",
            Self::Self_ => "self",
        }
    }
}

/// Wire `category`. Machine protocol (f4/1 envelope, ack, `hyphae-behavior`
/// query/response, canary) and friend-protocol messages are both
/// [`Category::Protocol`]; the adapter classifies them (X11-2e). [`admit`]
/// does not inspect the body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Category {
    Direct,
    Group,
    GroupMention,
    Stranger,
    DevBridge,
    Protocol,
    Internal,
}

/// Wire `addressed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Addressed {
    None,
    Mention,
    Direct,
}

/// Stored on `inbound_policy`. Not read by [`admit`] (§3.4 F5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AcceptMode {
    Off,
    Observe,
    Auto,
}

impl AcceptMode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Observe => "observe",
            Self::Auto => "auto",
        }
    }
}

/// One allowlist row. Order in the slice does not affect [`policy_version`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AllowlistEntry {
    pub source: Source,
    pub actor: String,
}

/// Everything [`admit`] needs. Membership and the window count are already
/// resolved; this slice does not look at the X11-2a store types.
///
/// Times are unix seconds. `arrived_unix_secs` is the event time compared
/// with the source watermark and with `now_unix_secs` (§3.3 row 2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AdmitInput {
    pub source: Source,
    pub actor: String,
    pub category: Category,
    pub human: bool,
    pub addressed: Addressed,
    /// Caller-resolved: actor is in `inbound_allowlist` for `source`.
    pub allowlisted: bool,
    pub arrived_unix_secs: i64,
    /// Source activation watermark (`activated_at`). Strictly earlier is stale.
    pub activated_unix_secs: i64,
    /// Max delay. Age strictly greater than this is stale. The 10-minute
    /// placeholder in §3.4 is chosen by the caller (X11-2c), not here.
    pub max_age_secs: u64,
    pub now_unix_secs: i64,
    /// Events for this `source+actor` already inside the sliding window,
    /// excluding the one being judged (it is not inserted yet). Not an
    /// in-flight count. See the module comment (评审 N2).
    pub window_count: u32,
    /// Window cap. `window_count >= rate_limit` is over the cap (a cap of 3
    /// rejects when 3 are already in the window). The "3 per minute"
    /// placeholder is chosen by the caller, not here.
    pub rate_limit: u32,
}

/// `explicit` carries no reason. `ignore` always does. `filtered` and
/// `internal` are not produced here (`internal` is the scheduler path, which
/// skips admission).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Disposition {
    Explicit,
    Ignore,
}

/// Reasons [`admit`] can emit. Names are the §3.1 / §3.3 wire values.
/// `not_addressed` is only an example in the column comment; row 7 is
/// `no_rule`. There is no `mode_off`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reason {
    Protocol,
    Stale,
    Bot,
    NotAllowlisted,
    RateLimited,
    NoRule,
}

impl Reason {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Protocol => "protocol",
            Self::Stale => "stale",
            Self::Bot => "bot",
            Self::NotAllowlisted => "not_allowlisted",
            Self::RateLimited => "rate_limited",
            Self::NoRule => "no_rule",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Decision {
    pub disposition: Disposition,
    pub reason: Option<Reason>,
}

impl Decision {
    pub(crate) fn explicit() -> Self {
        Self {
            disposition: Disposition::Explicit,
            reason: None,
        }
    }

    pub(crate) fn ignore(reason: Reason) -> Self {
        Self {
            disposition: Disposition::Ignore,
            reason: Some(reason),
        }
    }
}

/// First matching §3.3 row wins.
pub(crate) fn admit(input: &AdmitInput) -> Decision {
    // 1. Protocol, including a friend-protocol message labeled as such.
    if input.category == Category::Protocol {
        return Decision::ignore(Reason::Protocol);
    }
    // 2. Before this source's watermark, or older than the max delay.
    //    Equal to the watermark is not "早于". Age equal to the cap is not "超过".
    if input.arrived_unix_secs < input.activated_unix_secs || exceeds_max_age(input) {
        return Decision::ignore(Reason::Stale);
    }
    // 3. Non-human. No `inferred` in this milestone.
    if !input.human {
        return Decision::ignore(Reason::Bot);
    }
    // 4. Off the allowlist, strangers included. No reply.
    if !input.allowlisted {
        return Decision::ignore(Reason::NotAllowlisted);
    }
    // 5. Sliding-window rate only. In-flight uniqueness is the partial index.
    if input.window_count >= input.rate_limit {
        return Decision::ignore(Reason::RateLimited);
    }
    // 6. Allowlisted and addressed (direct or mention) → explicit.
    if matches!(input.addressed, Addressed::Direct | Addressed::Mention) {
        return Decision::explicit();
    }
    // 7. Allowlisted but not addressed. `inferred` collapses to this row.
    Decision::ignore(Reason::NoRule)
}

/// `sha256` hex of the sorted allowlist, `accept_mode`, and [`ADMIT_RULE_VERSION`].
///
/// Canonical preimage (UTF-8, `\n` terminated lines):
///
/// ```text
/// A24-INBOUND-ADMIT
/// rule:<ADMIT_RULE_VERSION>
/// mode:<off|observe|auto>
/// <count>
/// <source_byte_len>:<source>\t<actor_byte_len>:<actor>
/// ...
/// ```
///
/// Entries are sorted by source wire name, then actor bytes. Duplicate rows
/// are kept. Length prefixes keep an actor that contains `\n` or `\t` from
/// colliding with another list.
pub(crate) fn policy_version(entries: &[AllowlistEntry], mode: AcceptMode) -> String {
    let mut order: Vec<&AllowlistEntry> = entries.iter().collect();
    order.sort_by(|left, right| {
        left.source
            .as_str()
            .cmp(right.source.as_str())
            .then_with(|| left.actor.as_str().cmp(right.actor.as_str()))
    });
    let mut preimage = format!(
        "A24-INBOUND-ADMIT\nrule:{ADMIT_RULE_VERSION}\nmode:{}\n{}\n",
        mode.as_str(),
        order.len()
    );
    for entry in order {
        let source = entry.source.as_str();
        preimage.push_str(&format!(
            "{}:{source}\t{}:{}\n",
            source.len(),
            entry.actor.len(),
            entry.actor
        ));
    }
    hex32(&Sha256::digest(preimage.as_bytes()))
}

fn exceeds_max_age(input: &AdmitInput) -> bool {
    let age = i128::from(input.now_unix_secs) - i128::from(input.arrived_unix_secs);
    age > i128::from(input.max_age_secs)
}

fn hex32(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[usize::from(byte >> 4)] as char);
        out.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn baseline() -> AdmitInput {
        AdmitInput {
            source: Source::Hyphae,
            actor: "npub1alice".to_string(),
            category: Category::Direct,
            human: true,
            addressed: Addressed::Direct,
            allowlisted: true,
            arrived_unix_secs: 1_700_000_000,
            activated_unix_secs: 1_700_000_000,
            max_age_secs: 600,
            now_unix_secs: 1_700_000_000,
            window_count: 0,
            rate_limit: 3,
        }
    }

    fn apply(patch: fn(&mut AdmitInput)) -> AdmitInput {
        let mut input = baseline();
        patch(&mut input);
        input
    }

    struct Case {
        name: &'static str,
        patch: fn(&mut AdmitInput),
        expect: Decision,
    }

    fn case(name: &'static str, patch: fn(&mut AdmitInput), expect: Decision) -> Case {
        Case {
            name,
            patch,
            expect,
        }
    }

    /// §3.3：每行一条命中、一条不命中的正对照，外加优先级。
    #[test]
    fn section_3_3_each_row_hits_and_misses() {
        let explicit = Decision::explicit();
        let cases = [
            // 1 protocol
            case(
                "1 hit protocol even when allowlisted and addressed",
                |input| input.category = Category::Protocol,
                Decision::ignore(Reason::Protocol),
            ),
            case("1 miss ordinary direct is not protocol", |_| {}, explicit),
            // 2 stale — watermark
            case(
                "2 hit arrived before watermark",
                |input| input.arrived_unix_secs -= 1,
                Decision::ignore(Reason::Stale),
            ),
            case("2 miss equal to watermark is not 早于", |_| {}, explicit),
            // 2 stale — max delay
            case(
                "2 hit age exceeds max_age_secs",
                |input| input.now_unix_secs += 601,
                Decision::ignore(Reason::Stale),
            ),
            case(
                "2 miss age equal to max_age_secs is not 超过",
                |input| input.now_unix_secs += 600,
                explicit,
            ),
            // 3 bot
            case(
                "3 hit human false",
                |input| input.human = false,
                Decision::ignore(Reason::Bot),
            ),
            case("3 miss human true", |input| input.human = true, explicit),
            // 4 allowlist
            case(
                "4 hit off the allowlist",
                |input| input.allowlisted = false,
                Decision::ignore(Reason::NotAllowlisted),
            ),
            case(
                "4 hit stranger off the allowlist",
                |input| {
                    input.category = Category::Stranger;
                    input.allowlisted = false;
                },
                Decision::ignore(Reason::NotAllowlisted),
            ),
            case(
                "4 miss on the allowlist",
                |input| input.allowlisted = true,
                explicit,
            ),
            // 5 sliding window only
            case(
                "5 hit window already full",
                |input| input.window_count = 3,
                Decision::ignore(Reason::RateLimited),
            ),
            case(
                "5 miss one under the cap",
                |input| input.window_count = 2,
                explicit,
            ),
            // 6 explicit
            case(
                "6 hit direct",
                |input| input.addressed = Addressed::Direct,
                explicit,
            ),
            case(
                "6 hit mention",
                |input| {
                    input.category = Category::GroupMention;
                    input.addressed = Addressed::Mention;
                },
                explicit,
            ),
            case(
                "6 miss not addressed",
                |input| {
                    input.category = Category::Group;
                    input.addressed = Addressed::None;
                },
                Decision::ignore(Reason::NoRule),
            ),
            // 7 no_rule
            case(
                "7 hit allowlisted but not addressed",
                |input| input.addressed = Addressed::None,
                Decision::ignore(Reason::NoRule),
            ),
            case(
                "7 miss mention is explicit rather than no_rule",
                |input| input.addressed = Addressed::Mention,
                explicit,
            ),
        ];

        for case in cases {
            let got = admit(&apply(case.patch));
            assert_eq!(got, case.expect, "{}", case.name);
            if let Some(reason) = got.reason {
                assert!(
                    matches!(
                        reason.as_str(),
                        "protocol"
                            | "stale"
                            | "bot"
                            | "not_allowlisted"
                            | "rate_limited"
                            | "no_rule"
                    ),
                    "{} reason wire",
                    case.name
                );
            }
        }
    }

    #[test]
    fn protocol_beats_every_later_row_even_when_allowlisted() {
        let mut input = baseline();
        input.category = Category::Protocol;
        input.human = false;
        input.allowlisted = true;
        input.arrived_unix_secs -= 86_400;
        input.window_count = 100;
        input.addressed = Addressed::None;
        let got = admit(&input);
        assert_eq!(got, Decision::ignore(Reason::Protocol));
        assert_eq!(got.reason.map(Reason::as_str), Some("protocol"));
    }

    #[test]
    fn earlier_rows_beat_later_ones() {
        let stale_and_bot_and_off_list = apply(|input| {
            input.arrived_unix_secs -= 1;
            input.human = false;
            input.allowlisted = false;
        });
        assert_eq!(
            admit(&stale_and_bot_and_off_list),
            Decision::ignore(Reason::Stale)
        );

        let bot_and_off_list = apply(|input| {
            input.human = false;
            input.allowlisted = false;
            input.window_count = 9;
        });
        assert_eq!(admit(&bot_and_off_list), Decision::ignore(Reason::Bot));

        let off_list_and_over_rate = apply(|input| {
            input.allowlisted = false;
            input.window_count = 9;
            input.addressed = Addressed::None;
        });
        assert_eq!(
            admit(&off_list_and_over_rate),
            Decision::ignore(Reason::NotAllowlisted)
        );

        let over_rate_but_addressed = apply(|input| input.window_count = 3);
        assert_eq!(
            admit(&over_rate_but_addressed),
            Decision::ignore(Reason::RateLimited)
        );
    }

    /// 评审 N2：没有在途入参。窗口未满时即使调用方那边另有在途，纯函数仍放行。
    #[test]
    fn inflight_is_not_an_admit_input() {
        let under_cap = apply(|input| {
            input.window_count = 0;
            input.rate_limit = 3;
        });
        assert_eq!(admit(&under_cap), Decision::explicit());
        assert!(
            !format!("{under_cap:?}").contains("inflight"),
            "AdmitInput must not grow an in-flight field"
        );
    }

    #[test]
    fn clock_extremes_do_not_panic() {
        let mut at_bounds = baseline();
        at_bounds.arrived_unix_secs = i64::MIN;
        at_bounds.activated_unix_secs = i64::MIN;
        at_bounds.now_unix_secs = i64::MAX;
        at_bounds.max_age_secs = u64::MAX;
        assert_eq!(admit(&at_bounds), Decision::explicit());

        let mut future = baseline();
        future.arrived_unix_secs = i64::MAX;
        future.activated_unix_secs = i64::MIN;
        future.now_unix_secs = i64::MIN;
        assert_eq!(admit(&future), Decision::explicit());
    }

    #[test]
    fn dev_bridge_allowlisted_direct_is_explicit() {
        let input = apply(|input| input.category = Category::DevBridge);
        assert_eq!(admit(&input), Decision::explicit());
    }

    fn entry(source: Source, actor: &str) -> AllowlistEntry {
        AllowlistEntry {
            source,
            actor: actor.to_string(),
        }
    }

    #[test]
    fn policy_version_is_stable_and_order_independent() {
        let forward = [
            entry(Source::Wechat, "uid-b"),
            entry(Source::Hyphae, "npub1a"),
            entry(Source::Hyphae, "npub1b"),
        ];
        let reversed = [
            entry(Source::Hyphae, "npub1b"),
            entry(Source::Hyphae, "npub1a"),
            entry(Source::Wechat, "uid-b"),
        ];
        let once = policy_version(&forward, AcceptMode::Observe);
        assert_eq!(once, policy_version(&forward, AcceptMode::Observe));
        assert_eq!(once, policy_version(&reversed, AcceptMode::Observe));
        assert_eq!(once.len(), 64);
        assert!(once.chars().all(|ch| ch.is_ascii_hexdigit()));

        let preimage = "\
A24-INBOUND-ADMIT
rule:1
mode:observe
3
6:hyphae\t6:npub1a
6:hyphae\t6:npub1b
6:wechat\t5:uid-b
";
        assert_eq!(once, hex32(&Sha256::digest(preimage.as_bytes())));
    }

    #[test]
    fn policy_version_changes_with_mode_membership_and_actor_bytes() {
        let list = [entry(Source::Hyphae, "npub1a")];
        let observe = policy_version(&list, AcceptMode::Observe);
        assert_ne!(observe, policy_version(&list, AcceptMode::Off));
        assert_ne!(observe, policy_version(&list, AcceptMode::Auto));
        assert_ne!(observe, policy_version(&[], AcceptMode::Observe));
        assert_ne!(
            observe,
            policy_version(&[entry(Source::Hyphae, "npub1b")], AcceptMode::Observe)
        );
        // Same actor bytes under another source are a different list.
        assert_ne!(
            observe,
            policy_version(&[entry(Source::Wechat, "npub1a")], AcceptMode::Observe)
        );
    }

    #[test]
    fn policy_version_length_prefix_survives_newline_in_actor() {
        let embedded = policy_version(&[entry(Source::Hyphae, "a\nb")], AcceptMode::Auto);
        let split = policy_version(
            &[entry(Source::Hyphae, "a"), entry(Source::Hyphae, "b")],
            AcceptMode::Auto,
        );
        assert_ne!(embedded, split);
    }

    #[test]
    fn accept_mode_does_not_change_admit() {
        let input = baseline();
        let decision = admit(&input);
        assert_eq!(decision, Decision::explicit());
        // Three stored modes, one decision: mode is not on AdmitInput.
        for mode in [AcceptMode::Off, AcceptMode::Observe, AcceptMode::Auto] {
            let _ = policy_version(&[], mode);
        }
        assert_eq!(admit(&input), decision);
    }
}
