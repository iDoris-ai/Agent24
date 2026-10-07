//! The decision service: cascades a [`DecisionRequest`] through an ordered
//! list of [`DecisionBackend`]s. See `docs/research/DECISION-MODELS.md` §7.3
//! and `docs/agent/PLAN-DECIDE.md` §0 for the constraints this enforces:
//!
//! - a rule hit decides immediately (short-circuit, no further layer run);
//! - a layer with no conclusion falls through to the next layer, but may
//!   leave behind a non-final `floor` (see [`crate::backend::BackendOutcome::NoConclusion`]);
//! - a layer that is *unavailable* stops the cascade and reports
//!   `Outcome::Unavailable`, carrying forward the most recent non-empty
//!   `floor` a prior layer actually left behind (empty if none did) — it
//!   never silently jumps to a different backend, and never invents a
//!   result (`unavailable_layer_carries_the_last_floor_forward` locks this);
//! - every answer a backend reports is re-stamped with that backend's own
//!   `kind()` via [`crate::types::Answer::normalized_for`] right after
//!   `evaluate()` returns — the service does not trust a backend's
//!   self-reported `BackendKind` on each `Answer` (see that function's docs
//!   for why `Answer::new`'s own check is not sufficient on its own).

use std::sync::Arc;
use std::time::Instant;

use crate::backend::{BackendOutcome, DecisionBackend};
use crate::types::{Answer, BackendKind, Decision, DecisionRequest, Outcome};

/// Runs a [`DecisionRequest`] through an ordered stack of backends.
pub struct DecisionService {
    backends: Vec<Arc<dyn DecisionBackend>>,
}

impl DecisionService {
    pub fn new(backends: Vec<Arc<dyn DecisionBackend>>) -> Self {
        Self { backends }
    }

    pub async fn decide(&self, request: &DecisionRequest) -> Decision {
        let start = Instant::now();
        // The most recent non-empty floor a layer actually left behind,
        // carried forward so an `Unavailable`/exhausted-cascade ending can
        // report it instead of reporting nothing — see module docs.
        let mut floor_answers: Vec<Answer> = Vec::new();
        let mut floor_backend: Option<BackendKind> = None;

        for backend in &self.backends {
            let kind = backend.kind();
            match backend.evaluate(request).await {
                BackendOutcome::Decided(answers) => {
                    let answers = normalize(answers, kind);
                    return Decision {
                        answers,
                        backend: kind,
                        model: None,
                        latency_ms: elapsed_ms(start),
                        outcome: Outcome::Decided,
                    };
                }
                BackendOutcome::NoConclusion { floor } => {
                    if !floor.is_empty() {
                        floor_answers = normalize(floor, kind);
                        floor_backend = Some(kind);
                    }
                }
                BackendOutcome::Unavailable { reason } => {
                    return Decision {
                        answers: floor_answers,
                        backend: floor_backend.unwrap_or(kind),
                        model: None,
                        latency_ms: elapsed_ms(start),
                        outcome: Outcome::Unavailable { reason },
                    };
                }
            }
        }

        Decision {
            answers: floor_answers,
            backend: floor_backend.unwrap_or(BackendKind::Rule),
            model: None,
            latency_ms: elapsed_ms(start),
            outcome: Outcome::Abstain {
                reason: "no backend reached a conclusion".to_owned(),
            },
        }
    }
}

/// Re-stamps every answer against the backend's own `kind()` — see
/// [`crate::types::Answer::normalized_for`] for why this, not `Answer::new`,
/// is the actual enforcement point for the `llm_simulation` invariant.
fn normalize(answers: Vec<Answer>, kind: BackendKind) -> Vec<Answer> {
    answers
        .into_iter()
        .map(|a| a.normalized_for(kind))
        .collect()
}

fn elapsed_ms(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use serde_json::json;

    use super::*;
    use crate::backend::RuleBackend;
    use crate::points::RETAIN_INTENT;
    use crate::types::{AnswerValue, DecisionPoint, Question, SideEffectClass};

    fn request() -> DecisionRequest {
        DecisionRequest {
            point: DecisionPoint::new(RETAIN_INTENT),
            input: "你记住，我对花生过敏。".to_owned(),
            context: json!({}),
            questions: vec![Question::Noul {
                id: "is_remember".to_owned(),
                prompt: "是否要求记住？".to_owned(),
            }],
            side_effect: SideEffectClass::Reversible,
        }
    }

    /// A test-only backend that records whether it was ever called, so the
    /// cascade tests below can assert a later layer was never reached.
    struct CountingBackend {
        kind: BackendKind,
        calls: Arc<AtomicUsize>,
        outcome: BackendOutcome,
    }

    #[async_trait]
    impl DecisionBackend for CountingBackend {
        fn kind(&self) -> BackendKind {
            self.kind
        }

        async fn evaluate(&self, _request: &DecisionRequest) -> BackendOutcome {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.outcome.clone()
        }
    }

    #[tokio::test]
    async fn rule_hit_short_circuits_and_never_reaches_next_layer() {
        let rule = RuleBackend::new(|_req| {
            Some(vec![Answer::new(
                "is_remember",
                AnswerValue::Bool(true),
                Some(0.99),
                BackendKind::Rule,
                true,
            )])
        });
        let next_calls = Arc::new(AtomicUsize::new(0));
        let next = CountingBackend {
            kind: BackendKind::Encoder,
            calls: next_calls.clone(),
            outcome: BackendOutcome::Decided(vec![]),
        };

        let service = DecisionService::new(vec![Arc::new(rule), Arc::new(next)]);
        let decision = service.decide(&request()).await;

        assert_eq!(decision.outcome, Outcome::Decided);
        assert_eq!(decision.backend, BackendKind::Rule);
        assert_eq!(decision.answers.len(), 1);
        assert_eq!(
            next_calls.load(Ordering::SeqCst),
            0,
            "encoder layer must not run after a rule hit"
        );
    }

    #[tokio::test]
    async fn rule_miss_falls_through_to_next_layer() {
        let rule = RuleBackend::new(|_req| None);
        let decided = Answer::new(
            "is_remember",
            AnswerValue::Bool(false),
            Some(0.8),
            BackendKind::Encoder,
            true,
        );
        let next = CountingBackend {
            kind: BackendKind::Encoder,
            calls: Arc::new(AtomicUsize::new(0)),
            outcome: BackendOutcome::Decided(vec![decided]),
        };

        let service = DecisionService::new(vec![Arc::new(rule), Arc::new(next)]);
        let decision = service.decide(&request()).await;

        assert_eq!(decision.outcome, Outcome::Decided);
        assert_eq!(decision.backend, BackendKind::Encoder);
    }

    #[tokio::test]
    async fn unavailable_layer_stops_the_cascade_without_downgrading() {
        let rule = RuleBackend::new(|_req| None);
        let unavailable_calls = Arc::new(AtomicUsize::new(0));
        let unavailable = CountingBackend {
            kind: BackendKind::Encoder,
            calls: unavailable_calls.clone(),
            outcome: BackendOutcome::Unavailable {
                reason: "model not downloaded".to_owned(),
            },
        };
        let never_reached_calls = Arc::new(AtomicUsize::new(0));
        let never_reached = CountingBackend {
            kind: BackendKind::Deep,
            calls: never_reached_calls.clone(),
            // If the cascade silently skipped the unavailable layer it would
            // land here and get a confident Decided — the test below fails
            // unless that is proven not to happen.
            outcome: BackendOutcome::Decided(vec![Answer::new(
                "is_remember",
                AnswerValue::Bool(true),
                Some(0.99),
                BackendKind::Deep,
                true,
            )]),
        };

        let service = DecisionService::new(vec![
            Arc::new(rule),
            Arc::new(unavailable),
            Arc::new(never_reached),
        ]);
        let decision = service.decide(&request()).await;

        assert_eq!(
            decision.outcome,
            Outcome::Unavailable {
                reason: "model not downloaded".to_owned()
            }
        );
        assert!(
            decision.answers.is_empty(),
            "no rule conclusion existed to carry forward"
        );
        assert_eq!(unavailable_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            never_reached_calls.load(Ordering::SeqCst),
            0,
            "service must not silently fall through to a backend after the layer it needed went unavailable"
        );
    }

    #[tokio::test]
    async fn unavailable_layer_carries_the_last_floor_forward() {
        // A real (not dead-code) exercise of `BackendOutcome::NoConclusion`'s
        // `floor`: this layer does not reach a FINAL decision — it reports
        // `NoConclusion` — but it leaves behind a non-empty floor answer. The
        // next layer is unavailable; the service must return that floor
        // verbatim, not drop it and not invent anything else.
        struct FloorThenNoConclusion;
        #[async_trait]
        impl DecisionBackend for FloorThenNoConclusion {
            fn kind(&self) -> BackendKind {
                BackendKind::Encoder
            }
            async fn evaluate(&self, _request: &DecisionRequest) -> BackendOutcome {
                BackendOutcome::NoConclusion {
                    floor: vec![Answer::new(
                        "is_remember",
                        AnswerValue::Bool(false),
                        Some(0.4),
                        BackendKind::Encoder,
                        true,
                    )],
                }
            }
        }

        let floor = FloorThenNoConclusion;
        let unavailable = CountingBackend {
            kind: BackendKind::Deep,
            calls: Arc::new(AtomicUsize::new(0)),
            outcome: BackendOutcome::Unavailable {
                reason: "runtime crashed".to_owned(),
            },
        };

        let service = DecisionService::new(vec![Arc::new(floor), Arc::new(unavailable)]);
        let decision = service.decide(&request()).await;

        assert_eq!(
            decision.backend,
            BackendKind::Encoder,
            "reports the layer that actually ran, not the one that failed"
        );
        assert_eq!(
            decision.outcome,
            Outcome::Unavailable {
                reason: "runtime crashed".to_owned()
            }
        );
        assert_eq!(
            decision.answers.len(),
            1,
            "the non-empty floor must be carried forward, not dropped"
        );
        assert_eq!(decision.answers[0].question_id(), "is_remember");
        assert_eq!(decision.answers[0].p(), Some(0.4));
    }

    #[tokio::test]
    async fn an_empty_floor_carries_forward_as_empty_not_as_a_fabricated_answer() {
        // The counterpart to the test above: `NoConclusion { floor: vec![] }`
        // (RuleBackend's own miss case) must not somehow leave behind
        // anything for a later `Unavailable` to report.
        let rule = RuleBackend::new(|_req| None);
        let unavailable = CountingBackend {
            kind: BackendKind::Encoder,
            calls: Arc::new(AtomicUsize::new(0)),
            outcome: BackendOutcome::Unavailable {
                reason: "model not downloaded".to_owned(),
            },
        };

        let service = DecisionService::new(vec![Arc::new(rule), Arc::new(unavailable)]);
        let decision = service.decide(&request()).await;

        assert!(decision.answers.is_empty());
        assert_eq!(
            decision.outcome,
            Outcome::Unavailable {
                reason: "model not downloaded".to_owned()
            }
        );
    }

    #[tokio::test]
    async fn decide_normalizes_calibrated_by_the_backends_real_kind_not_its_self_report() {
        // Regression for review #688 M1 (R2/R4): a backend whose
        // `DecisionBackend::kind()` is `LlmSimulation` but whose
        // `evaluate()` builds its `Answer` with a DIFFERENT `BackendKind`
        // (here `Encoder`), as if lying to `Answer::new` about itself. Before
        // the fix, `decide()` trusted the answer's self-reported calibration
        // as-is and returned `calibrated() == true`; `DecisionService` must
        // instead re-stamp every answer using the backend's OWN `kind()`.
        struct SelfMislabelingBackend;
        #[async_trait]
        impl DecisionBackend for SelfMislabelingBackend {
            fn kind(&self) -> BackendKind {
                BackendKind::LlmSimulation
            }
            async fn evaluate(&self, _request: &DecisionRequest) -> BackendOutcome {
                BackendOutcome::Decided(vec![Answer::new(
                    "is_remember",
                    AnswerValue::Bool(true),
                    Some(0.97),
                    BackendKind::Encoder, // lies about its own kind
                    true,
                )])
            }
        }

        let service = DecisionService::new(vec![Arc::new(SelfMislabelingBackend)]);
        let decision = service.decide(&request()).await;

        assert_eq!(
            decision.backend,
            BackendKind::LlmSimulation,
            "reports the backend's real kind()"
        );
        assert_eq!(decision.answers.len(), 1);
        assert!(
            !decision.answers[0].calibrated(),
            "service must normalize by kind() — a backend cannot report calibrated=true for an llm_simulation answer just by mislabeling it on the way in"
        );
    }

    #[tokio::test]
    async fn no_backend_reaching_a_conclusion_abstains_rather_than_guessing() {
        let rule = RuleBackend::new(|_req| None);
        let service = DecisionService::new(vec![Arc::new(rule)]);
        let decision = service.decide(&request()).await;

        assert!(matches!(decision.outcome, Outcome::Abstain { .. }));
        assert!(decision.answers.is_empty());
    }
}
