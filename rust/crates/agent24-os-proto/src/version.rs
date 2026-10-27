//! Version-range negotiation for the `initialize` handshake.
//!
//! The rules are SPEC-ME3 §8 (ME-3b row), quoted rather than paraphrased because
//! the test table below is asserted against them — and quoted AGAIN inline, next
//! to each group of cases, so that "these answers were not chosen by whoever
//! wrote the code" is something a reader can check here rather than take:
//!
//! > 双方各报区间 `[min,max]` 取交集，交集为空 → 握手失败并把两边区间都放进错误
//! > （`-32000` + `version_mismatch`），交集非空 → 选定唯一版本
//! > `min(模块.max, 内核.max)` 并在响应里回显，**内核不得回一个模块没声明支持的
//! > 版本**；模块**不报区间** → 视为不兼容，握手失败。
//!
//! Five rules, and the fifth is the one an implementation drifts on: a module
//! that declares NO range is incompatible, not "assume v1". Treating silence as
//! agreement is the same failure shape as a lease-less credential downgrade —
//! a missing declaration answered with a default instead of a refusal.
//!
//! # This module has no production caller yet
//!
//! Its consumer is ME-3b-2b (the `initialize` wire shape), which in turn needs
//! ME-3b-1 (framing). So this is 🟢 library-only in the sense SPEC-MD-ME defines,
//! and it must not be recorded as ✅ anywhere.
//!
//! **The condition for removing it from `main` is an EVENT, not a date**: if
//! 3b-2b is abandoned or routed around — if something other than this function
//! ends up deciding the handshake's version — this file leaves with it. A
//! deadline in days would be a clock running against work two slices away, so it
//! would fire for a scheduling reason rather than for the reason this expiry
//! exists (the code was orphaned). An expiry that fires for the wrong reason gets
//! extended, and after one extension it is no longer an expiry, it is a comment.
//! The `TEMPORARY` marker in `agent24-os-packages` had force precisely because
//! the day it fired, its condition was genuinely met.
//!
//! See `stub` below for the call site that consumer is expected to use.

use std::fmt;

/// An inclusive range of protocol versions a side can speak.
///
/// Constructed through [`VersionRange::new`], which refuses `min > max`. An
/// inverted range is not a range that fails to overlap — it is a range that
/// cannot be true of anybody, and letting one exist means every function
/// downstream has to decide what it means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VersionRange {
    min: u32,
    max: u32,
}

impl VersionRange {
    /// `None` when `min > max`.
    #[must_use]
    pub fn new(min: u32, max: u32) -> Option<Self> {
        (min <= max).then_some(Self { min, max })
    }

    /// The lowest version this side speaks.
    #[must_use]
    pub fn min(&self) -> u32 {
        self.min
    }

    /// The highest version this side speaks.
    #[must_use]
    pub fn max(&self) -> u32 {
        self.max
    }

    /// Whether `v` is inside this range. Used by callers to check a negotiated
    /// version against BOTH declarations — see the note on rule 4 in [`negotiate`].
    #[must_use]
    pub fn contains(&self, v: u32) -> bool {
        self.min <= v && v <= self.max
    }
}

/// Serialised as `{"min":m,"max":n}`.
impl serde::Serialize for VersionRange {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("VersionRange", 2)?;
        st.serialize_field("min", &self.min)?;
        st.serialize_field("max", &self.max)?;
        st.end()
    }
}

/// Deserialised **through [`VersionRange::new`]**, not straight into the fields.
///
/// `#[derive(Deserialize)]` would have written directly to `min` and `max`,
/// which means a peer could put an inverted range on the wire and get a value
/// that `new` refuses to construct in this process. The private fields and the
/// checked constructor would then be a rule that holds everywhere except at the
/// one place the value comes from someone else — which is the only place it
/// matters.
impl<'de> serde::Deserialize<'de> for VersionRange {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            min: u32,
            max: u32,
        }
        let w = Wire::deserialize(d)?;
        VersionRange::new(w.min, w.max).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "inverted protocol version range [{}, {}]",
                w.min, w.max
            ))
        })
    }
}

impl fmt::Display for VersionRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}, {}]", self.min, self.max)
    }
}

/// Why a handshake cannot proceed. Carries the inputs, because the operator
/// reading this error is trying to find out which side to upgrade — an error that
/// says only "version mismatch" sends them to read two changelogs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionMismatch {
    /// The module declared no range at all. Not the same as declaring one that
    /// does not overlap: nothing was claimed, so nothing can be relied on.
    NotDeclared { kernel: VersionRange },
    /// Both declared, and the ranges do not intersect.
    NoOverlap {
        module: VersionRange,
        kernel: VersionRange,
    },
}

impl VersionMismatch {
    /// The `error.data.kind` this maps to on the wire (SPEC-ME3 §3's closed set).
    /// Both variants are the same kind: the distinction between them is for the
    /// human reading the message, not for the module branching on the code.
    pub const KIND: &'static str = "version_mismatch";
}

impl fmt::Display for VersionMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotDeclared { kernel } => write!(
                f,
                "the module declared no protocol version range; the kernel speaks {kernel}"
            ),
            Self::NoOverlap { module, kernel } => write!(
                f,
                "no shared protocol version: the module speaks {module}, the kernel speaks {kernel}"
            ),
        }
    }
}

impl std::error::Error for VersionMismatch {}

/// Choose the protocol version for one handshake, or refuse it.
///
/// `module` is `None` when the module declared no range — rule 5, and the reason
/// this takes an `Option` rather than a range with a default.
///
/// The chosen version is `min(module.max, kernel.max)`, which is the TOP of the
/// intersection whenever the intersection is non-empty. That is what makes rule 4
/// ("the kernel must not answer with a version the module never declared") hold
/// by construction rather than by a later check: the answer is one of the
/// module's own endpoints, or below it. The property is asserted anyway in the
/// tests, because "holds by construction" is a claim about today's expression.
///
/// # Errors
///
/// [`VersionMismatch::NotDeclared`] when `module` is `None`;
/// [`VersionMismatch::NoOverlap`] when the two ranges do not intersect.
pub fn negotiate(
    module: Option<VersionRange>,
    kernel: VersionRange,
) -> Result<u32, VersionMismatch> {
    let Some(module) = module else {
        return Err(VersionMismatch::NotDeclared { kernel });
    };
    // Empty intersection. Written as the comparison of the two endpoints rather
    // than as `!contains(...)` on either side: a range wholly above the other and
    // one wholly below are the same failure, and expressing it once keeps them so.
    if module.max < kernel.min || kernel.max < module.min {
        return Err(VersionMismatch::NoOverlap { module, kernel });
    }
    Ok(module.max.min(kernel.max))
}

/// The oldest protocol version this kernel still speaks. Version 2 only added
/// a field a module may send (ME4-S2b's `temperature`), so a v2 kernel serves
/// a v1 module exactly as before, and need not tell the two apart on a
/// connection: the gate is the manifest's `min_daemon_protocol`.
pub const KERNEL_PROTOCOL_MIN: u32 = 1;
/// The newest: `agent24_domain::DAEMON_PROTOCOL_VERSION`, said here for a
/// module's SDK, which does not depend on the domain crate.
pub const KERNEL_PROTOCOL_MAX: u32 = agent24_domain::DAEMON_PROTOCOL_VERSION;

/// What this kernel build speaks: [`KERNEL_PROTOCOL_MIN`] up to
/// `agent24_domain::DAEMON_PROTOCOL_VERSION`, which remains the definition of
/// "current"; this is the same number said in the form the handshake needs.
#[must_use]
pub fn kernel_range() -> VersionRange {
    let v = agent24_domain::DAEMON_PROTOCOL_VERSION;
    // Cannot fail: the assertion below holds `min <= max`. Written without
    // `expect` so a daemon can never panic here.
    VersionRange::new(KERNEL_PROTOCOL_MIN, v).unwrap_or(VersionRange { min: v, max: v })
}
const _: () = assert!(KERNEL_PROTOCOL_MIN <= agent24_domain::DAEMON_PROTOCOL_VERSION);

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn r(min: u32, max: u32) -> VersionRange {
        VersionRange::new(min, max).expect("test range is not inverted")
    }

    /// The table is transcribed from SPEC-ME3 §8 (the ME-3b row), not invented
    /// here. When a row and the implementation disagree, the implementation is
    /// what changes — that is the whole reason this slice was built before
    /// anything that calls it.
    ///
    /// **The SPEC's own sentences are quoted inline, next to the cases they
    /// govern.** Without that, "the expected answers are not the author's to
    /// choose" quietly stops being true at the moment the prose is turned into a
    /// table: the translation is itself a decision by whoever wrote the test.
    /// Quoted, a reader can check the claim against the source without leaving
    /// this file. (Review, PR-Daemon, before this slice was opened.)
    #[test]
    fn the_spec_table() {
        // (module range, kernel range, expected)
        let cases: [(Option<VersionRange>, VersionRange, Result<u32, ()>); 9] = [
            // SPEC: 「交集非空 → 选定唯一版本 `min(模块.max, 内核.max)` 并在
            //        响应里回显」
            (Some(r(1, 1)), r(1, 1), Ok(1)),
            (Some(r(1, 3)), r(1, 3), Ok(3)),
            // …the kernel's ceiling is lower, so it wins.
            (Some(r(1, 5)), r(1, 3), Ok(3)),
            // …the module's ceiling is lower, so it wins. This is the row that
            // catches an implementation that always answers with the kernel's max
            // — and it is also the row that carries SPEC's 「内核不得回一个模块
            // 没声明支持的版本」in its most likely failure form.
            (Some(r(1, 2)), r(1, 7), Ok(2)),
            // Overlap of exactly one version, at the top of one and the bottom of
            // the other. An off-by-one in the emptiness test lands here. (Not a
            // SPEC row: the SPEC says "intersection", and these are the two
            // one-element intersections that a strict `<` would lose.)
            (Some(r(3, 9)), r(1, 3), Ok(3)),
            (Some(r(1, 3)), r(3, 9), Ok(3)),
            // SPEC: 「双方各报区间 `[min,max]` 取交集，交集为空 → 握手失败并把
            //        两边区间都放进错误（`-32000` + `version_mismatch`）」
            // Both directions, because a comparison written for one of them
            // passes half this table.
            (Some(r(1, 2)), r(3, 4), Err(())),
            (Some(r(5, 9)), r(1, 4), Err(())),
            // SPEC: 「模块**不报区间** → 视为不兼容，握手失败」
            (None, r(1, 9), Err(())),
        ];
        for (module, kernel, want) in cases {
            let got = negotiate(module, kernel);
            match want {
                Ok(v) => assert_eq!(got, Ok(v), "module={module:?} kernel={kernel:?}"),
                Err(()) => assert!(
                    got.is_err(),
                    "module={module:?} kernel={kernel:?} → {got:?}"
                ),
            }
        }
    }

    /// Rule 4, stated as the property rather than as a row: whatever comes out
    /// must be a version BOTH sides declared.
    ///
    /// Swept over every pair of small ranges rather than sampled, because the way
    /// this rule breaks is at an endpoint, and a hand-picked case is exactly what
    /// misses an endpoint. The sweep also supplies its own positive control: it
    /// asserts that some pairs succeeded and some failed, so a bug that made
    /// `negotiate` always fail could not pass by making the loop body unreachable.
    #[test]
    fn the_kernel_never_answers_with_a_version_the_module_did_not_declare() {
        let (mut agreed, mut refused) = (0u32, 0u32);
        for a in 0..6 {
            for b in a..6 {
                for c in 0..6 {
                    for d in c..6 {
                        let (module, kernel) = (r(a, b), r(c, d));
                        match negotiate(Some(module), kernel) {
                            Ok(v) => {
                                agreed += 1;
                                assert!(module.contains(v), "{module} answered {v}");
                                assert!(kernel.contains(v), "{kernel} answered {v}");
                            }
                            Err(_) => refused += 1,
                        }
                    }
                }
            }
        }
        assert!(
            agreed > 0 && refused > 0,
            "{agreed} agreed, {refused} refused"
        );
    }

    /// The two refusals are distinguishable, and both name the ranges. An
    /// operator reading this is trying to learn which side to upgrade.
    #[test]
    fn a_refusal_says_which_two_things_disagree() {
        let no_overlap = negotiate(Some(r(1, 2)), r(7, 9)).unwrap_err();
        let text = no_overlap.to_string();
        assert!(text.contains("[1, 2]") && text.contains("[7, 9]"), "{text}");

        let undeclared = negotiate(None, r(7, 9)).unwrap_err();
        assert_ne!(
            std::mem::discriminant(&no_overlap),
            std::mem::discriminant(&undeclared),
            "declaring nothing must not be reported as an empty intersection"
        );
        assert!(undeclared.to_string().contains("[7, 9]"));
    }

    /// `KIND` is a string that goes on the wire, and it was the one answer in
    /// this file the author picked freely: every other expected value is pinned
    /// by a quotation from the SPEC, but nothing looked at this one. A mutation
    /// proved it — misspelling it as `verison_mistmatch` turned the whole
    /// workspace red 0 times, while the same measurement on `negotiate` turned it
    /// red twice, so the count was not vacuous.
    ///
    /// It matters here more than its size suggests: this slice is the one that
    /// sets the pattern for the four after it. A precedent that "a constant is
    /// not an answer" is how 3b-1's `-32700` and 3b-2b's `-32600` /
    /// `auth_failed` / `manifest_mismatch` would arrive unguarded too — and it is
    /// why the assertion is a whole-word one. A half-open form copied four times
    /// is worse than no form at all: every copy carries a name saying it was
    /// checked.
    #[test]
    fn the_wire_kind_is_the_one_the_spec_names() {
        // SPEC-ME3 §8 (ME-3b row), verbatim — the same sentence quoted beside the
        // empty-intersection cases above:
        const SPEC: &str =
            "交集为空 → 握手失败并把两边区间都放进错误（`-32000` + `version_mismatch`）";
        // WHOLE WORD, not substring. The SPEC writes the kind inside backticks,
        // and those backticks are what supply the word boundary — without them
        // the assertion passes for any TRUNCATION of the real string. Not a
        // contrived worry: `KIND = "version"` is the most likely wrong value this
        // could ever hold, and the first version of this test let it through. It
        // excluded one specific misspelling, which is not the same as requiring
        // the right answer. Measured on that version: `"version"` and
        // `"mismatch"` each turned the workspace red 0 times, while the
        // misspelling turned it red once — so the test was real, and half open.
        let word = format!("`{}`", VersionMismatch::KIND);
        assert!(
            SPEC.contains(&word),
            "KIND is {:?}, which does not appear as a whole word in the SPEC sentence that names it",
            VersionMismatch::KIND
        );
        // The sentence contains TWO backticked words — `-32000` and
        // `version_mismatch` — so the assertion above accepts either. That is not
        // a contrived gap: the code and the kind travel together, and mistaking
        // one for the other is precisely the error their adjacency invites.
        //
        // The difference between them is not in the spelling, it is in the TYPE:
        // a code parses as an integer, a kind does not. Asserting the shape
        // narrows the sentence's acceptance set to one.
        assert!(
            VersionMismatch::KIND.parse::<i32>().is_err(),
            "KIND is {:?}, which is a numeric CODE, not an `error.data.kind`",
            VersionMismatch::KIND
        );
    }

    /// The constructor's rule has to survive the wire, because the wire is the
    /// only place the value comes from someone else.
    #[test]
    fn an_inverted_range_cannot_arrive_over_the_wire_either() {
        let bad: Result<VersionRange, _> = serde_json::from_str(r#"{"min":3,"max":1}"#);
        assert!(bad.is_err(), "an inverted range deserialised: {bad:?}");
        // Control: the same shape with a legal range must work, or the assertion
        // above is satisfied by the format being wrong rather than the range.
        let good: VersionRange = serde_json::from_str(r#"{"min":1,"max":3}"#).unwrap();
        assert_eq!(good, r(1, 3));
        // And it round-trips, so the kernel's echo of the chosen range is the
        // same shape the module sent.
        let text = serde_json::to_string(&good).unwrap();
        assert_eq!(serde_json::from_str::<VersionRange>(&text).unwrap(), good);
    }

    #[test]
    fn an_inverted_range_cannot_be_constructed() {
        // Not a range that fails to overlap — a range that cannot be true of
        // anybody. Refusing it at construction is what stops every function
        // downstream from having to decide what it means.
        assert_eq!(VersionRange::new(3, 1), None);
        assert!(
            VersionRange::new(1, 1).is_some(),
            "a single version is fine"
        );
    }

    #[test]
    fn this_build_declares_the_version_the_domain_crate_calls_current() {
        // The two must not be able to drift: `DAEMON_PROTOCOL_VERSION` is the
        // definition, and this is meant to be the same number in another shape.
        let k = kernel_range();
        assert_eq!(k.max(), agent24_domain::DAEMON_PROTOCOL_VERSION);
        // And a module that speaks exactly it can connect today.
        assert_eq!(
            negotiate(Some(k), k),
            Ok(agent24_domain::DAEMON_PROTOCOL_VERSION)
        );
    }

    /// ME4-S2b §4 (B): a v2 kernel speaks 1 to a module that only knows 1,
    /// and 2 to one that knows 2.
    #[test]
    fn a_v1_module_still_connects_and_a_v2_one_gets_2() {
        let k = kernel_range();
        assert_eq!((k.min(), k.max()), (1, 2));
        assert_eq!(negotiate(Some(r(1, 1)), k), Ok(1));
        assert_eq!(negotiate(Some(r(1, 2)), k), Ok(2));
        assert_eq!(negotiate(Some(r(2, 9)), k), Ok(2));
        assert!(negotiate(Some(r(3, 9)), k).is_err());
    }
}

/// The call site ME-3b-2b is expected to use, written now so that this
/// function's signature meets a consumer before one exists.
///
/// Not exported and not called: it exists to be COMPILED. A signature that has
/// never been used from the shape that will use it is a guess, and the cheapest
/// moment to find out it was the wrong guess is before there is anything to
/// rewrite. Its body is the part 3b-2b owns (reading the module's declaration off
/// the wire, and turning a refusal into a `-32000` frame); its edges are this
/// slice's.
#[cfg(test)]
mod stub {
    use super::{VersionMismatch, VersionRange, kernel_range, negotiate};

    /// What 3b-2b will hold after parsing the module's `initialize` params.
    struct InitializeParams {
        /// `None` when the module sent no range — rule 5. Modelled as an
        /// `Option` here, at the boundary, so the absence survives all the way to
        /// `negotiate` instead of being defaulted on the way in.
        protocol_versions: Option<VersionRange>,
    }

    #[allow(dead_code)]
    fn handshake(params: &InitializeParams) -> Result<u32, VersionMismatch> {
        negotiate(params.protocol_versions, kernel_range())
    }

    #[test]
    fn the_stub_compiles_against_the_shape_a_handshake_will_have() {
        // The assertion is weak on purpose — the point of this test is that the
        // code above type-checks. What it does assert is the one thing the stub
        // could get wrong while still compiling: that an absent declaration
        // reaches `negotiate` as `None` rather than as some default.
        let refused = handshake(&InitializeParams {
            protocol_versions: None,
        });
        assert!(matches!(refused, Err(VersionMismatch::NotDeclared { .. })));
        let agreed = handshake(&InitializeParams {
            protocol_versions: Some(kernel_range()),
        });
        assert_eq!(agreed, Ok(agent24_domain::DAEMON_PROTOCOL_VERSION));
    }
}
