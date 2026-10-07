//! Per-decision-point thresholds. `docs/agent/PLAN-DECIDE.md` §0 is explicit:
//! "不得有全局 0.5 默认" (no global 0.5 default) — so [`ThresholdBands`]
//! deliberately has no [`Default`] impl and no zero-arg constructor. Every
//! decision point must configure its own bands, with misjudgement cost in
//! mind (e.g. `guardian.risk` wants a low `escalate_below`, a false "low
//! risk" is expensive; a low-stakes classification can tolerate a wider
//! abstain band).

use serde::{Deserialize, Serialize};

/// What to do with a decision once its confidence is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Confidence at/above `execute_at`: act on the decision directly.
    Execute,
    /// Confidence between the two bands: don't guess — abstain, or ask the
    /// user a clarifying question.
    AbstainOrAsk,
    /// Confidence below `escalate_below`: hand to a human (approval) rather
    /// than let the backend's guess stand.
    Escalate,
}

#[derive(Debug, Clone, Copy, PartialEq, thiserror::Error)]
pub enum ThresholdError {
    #[error("execute_at must be within [0, 1], got {0}")]
    ExecuteAtOutOfRange(f32),
    #[error("escalate_below must be within [0, 1], got {0}")]
    EscalateBelowOutOfRange(f32),
    #[error("escalate_below ({escalate_below}) must be <= execute_at ({execute_at})")]
    Inverted {
        escalate_below: f32,
        execute_at: f32,
    },
}

/// The three-section threshold for one decision point: `[0, escalate_below)`
/// → [`Action::Escalate`], `[escalate_below, execute_at)` →
/// [`Action::AbstainOrAsk`], `[execute_at, 1]` → [`Action::Execute`].
///
/// Boundaries are inclusive on the upper edge of each band they open:
/// `p == execute_at` already executes, `p == escalate_below` is the start of
/// the abstain band (not escalate — escalate is strictly *below* it).
///
/// `Deserialize` goes through [`ThresholdBands::new`] via
/// `#[serde(try_from = "RawThresholdBands")]`, **not** a plain derive —
/// a plain `#[derive(Deserialize)]` fills the private fields directly and
/// bypasses the range/order checks entirely (review #688 M2, reproduced
/// with an independent probe crate: inverted bounds `{execute_at: 0.2,
/// escalate_below: 0.9}` deserialized successfully, and because
/// `action_for` checks `p >= execute_at` first, every `p` in `[0, 1]` then
/// came out `Execute` — exactly the direction `lib.rs`'s "only ever
/// stricter, never laxer" constraint forbids). Private fields alone only
/// block a struct literal and setters; they do nothing against
/// `Deserialize`, which is a different bypass. See the `*_json_is_rejected`
/// tests below for the regression coverage.
///
/// No [`Default`] either (see module docs for why) — enforced at compile
/// time, not just by a runtime test that happens to construct explicitly:
///
/// ```compile_fail
/// let _ = agent24_decide::ThresholdBands::default();
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawThresholdBands")]
pub struct ThresholdBands {
    execute_at: f32,
    escalate_below: f32,
}

/// Unvalidated wire shape `ThresholdBands::deserialize` parses into before
/// handing it to [`ThresholdBands::new`] via the `TryFrom` below — the one
/// and only path JSON can reach this type through.
#[derive(Debug, Deserialize)]
pub struct RawThresholdBands {
    execute_at: f32,
    escalate_below: f32,
}

impl TryFrom<RawThresholdBands> for ThresholdBands {
    type Error = ThresholdError;

    fn try_from(raw: RawThresholdBands) -> Result<Self, Self::Error> {
        ThresholdBands::new(raw.execute_at, raw.escalate_below)
    }
}

impl ThresholdBands {
    pub fn new(execute_at: f32, escalate_below: f32) -> Result<Self, ThresholdError> {
        if !(0.0..=1.0).contains(&execute_at) {
            return Err(ThresholdError::ExecuteAtOutOfRange(execute_at));
        }
        if !(0.0..=1.0).contains(&escalate_below) {
            return Err(ThresholdError::EscalateBelowOutOfRange(escalate_below));
        }
        if escalate_below > execute_at {
            return Err(ThresholdError::Inverted {
                escalate_below,
                execute_at,
            });
        }
        Ok(Self {
            execute_at,
            escalate_below,
        })
    }

    pub fn execute_at(&self) -> f32 {
        self.execute_at
    }

    pub fn escalate_below(&self) -> f32 {
        self.escalate_below
    }

    /// `p` is a calibrated probability/confidence in `[0, 1]`. Values outside
    /// that range are not clamped — callers are expected to pass a
    /// calibrated figure; an out-of-range input simply falls through to
    /// whichever band the comparison lands in.
    pub fn action_for(&self, p: f32) -> Action {
        if p >= self.execute_at {
            Action::Execute
        } else if p < self.escalate_below {
            Action::Escalate
        } else {
            Action::AbstainOrAsk
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn rejects_out_of_range_bounds() {
        assert_eq!(
            ThresholdBands::new(1.5, 0.2),
            Err(ThresholdError::ExecuteAtOutOfRange(1.5))
        );
        assert_eq!(
            ThresholdBands::new(0.8, -0.1),
            Err(ThresholdError::EscalateBelowOutOfRange(-0.1))
        );
    }

    #[test]
    fn rejects_inverted_bounds() {
        let err = ThresholdBands::new(0.3, 0.8).unwrap_err();
        assert_eq!(
            err,
            ThresholdError::Inverted {
                escalate_below: 0.8,
                execute_at: 0.3
            }
        );
    }

    #[test]
    fn three_section_boundaries_are_exact() {
        let bands = ThresholdBands::new(0.8, 0.3).expect("valid bands");

        // execute_at boundary: >= executes.
        assert_eq!(bands.action_for(0.8), Action::Execute);
        assert_eq!(bands.action_for(1.0), Action::Execute);
        assert_eq!(bands.action_for(0.799_999), Action::AbstainOrAsk);

        // escalate_below boundary: == is already the abstain band, strictly
        // below is escalate.
        assert_eq!(bands.action_for(0.3), Action::AbstainOrAsk);
        assert_eq!(bands.action_for(0.299_999), Action::Escalate);
        assert_eq!(bands.action_for(0.0), Action::Escalate);

        // middle of the abstain band.
        assert_eq!(bands.action_for(0.5), Action::AbstainOrAsk);
    }

    #[test]
    fn degenerate_bands_at_equal_bounds_leave_no_abstain_room() {
        // escalate_below == execute_at is legal (just a knife-edge instead
        // of a band) — it collapses AbstainOrAsk to nothing, which is a
        // valid (if aggressive) per-point policy choice, not a bug here.
        let bands = ThresholdBands::new(0.5, 0.5).expect("equal bounds are valid");
        assert_eq!(bands.action_for(0.5), Action::Execute);
        assert_eq!(bands.action_for(0.499_999), Action::Escalate);
    }

    #[test]
    fn there_is_no_default_impl() {
        // Compile-time guard, not a runtime assertion: PLAN-DECIDE.md §0
        // forbids a global 0.5 default, so `ThresholdBands` must not
        // implement `Default`. If someone adds `impl Default` later, the
        // only way to catch it here is a doc note — see the module docs —
        // this test exists so the intent is written down next to the type.
        // The doc comment on `ThresholdBands` itself carries a
        // `compile_fail` doctest that actually enforces this (review #688
        // L5) — this runtime test only documents intent next to the type.
        let explicit = ThresholdBands::new(0.8, 0.3).expect("valid bands");
        assert_eq!(explicit.execute_at(), 0.8);
        assert_eq!(explicit.escalate_below(), 0.3);
    }

    #[test]
    fn valid_json_round_trips_through_deserialize() {
        let bands: ThresholdBands =
            serde_json::from_str(r#"{"execute_at": 0.8, "escalate_below": 0.3}"#)
                .expect("valid bands must deserialize");
        assert_eq!(bands.execute_at(), 0.8);
        assert_eq!(bands.escalate_below(), 0.3);

        let round_tripped: ThresholdBands =
            serde_json::from_str(&serde_json::to_string(&bands).expect("serialize"))
                .expect("deserialize again");
        assert_eq!(round_tripped, bands);
    }

    #[test]
    fn inverted_order_json_is_rejected_not_silently_accepted() {
        // Exactly review #688 M2's repro: fields present and in-range
        // individually, but escalate_below > execute_at.
        let result: Result<ThresholdBands, _> =
            serde_json::from_str(r#"{"execute_at": 0.2, "escalate_below": 0.9}"#);
        assert!(
            result.is_err(),
            "a plain #[derive(Deserialize)] would have accepted this and inverted the whole policy — see type docs"
        );
    }

    #[test]
    fn out_of_range_json_is_rejected() {
        let too_high: Result<ThresholdBands, _> =
            serde_json::from_str(r#"{"execute_at": 5.0, "escalate_below": 0.2}"#);
        assert!(too_high.is_err());

        let negative: Result<ThresholdBands, _> =
            serde_json::from_str(r#"{"execute_at": 0.8, "escalate_below": -3.0}"#);
        assert!(negative.is_err());
    }

    #[test]
    fn nan_json_is_rejected() {
        // Standard JSON has no `NaN` literal at all — serde_json fails to
        // even PARSE this as JSON (a syntax error), before `TryFrom` ever
        // runs. Included anyway because the review explicitly asked for a
        // "NaN JSON must fail to deserialize" case: it does fail, just at
        // the tokenizer rather than at `ThresholdBands::new`'s validation.
        // (A value that reaches `new` as an actual `f32::NAN` — e.g. built
        // in Rust, not via JSON — is already covered by `action_for`'s
        // documented NaN behavior, not by this constructor: NaN compares
        // false against every bound, including its own, so `new` cannot
        // detect it as "out of range" via `contains`. `action_for(NaN)`
        // still lands safely in `AbstainOrAsk`, see its doc comment.)
        let result: Result<ThresholdBands, _> =
            serde_json::from_str(r#"{"execute_at": NaN, "escalate_below": 0.2}"#);
        assert!(
            result.is_err(),
            "JSON has no NaN literal; this must fail to parse, not silently succeed"
        );
    }
}
