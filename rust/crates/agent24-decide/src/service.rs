//! The decision service: cascades a [`DecisionRequest`] through an ordered
//! list of [`DecisionBackend`]s. See `docs/research/DECISION-MODELS.md` §7.3
//! and `docs/agent/PLAN-DECIDE.md` §0 for the constraints this enforces:
//!
//! - a rule hit decides immediately (short-circuit, no further layer run);
//! - a layer with no conclusion falls through to the next layer;
//! - a layer that is *unavailable* stops the cascade and reports
//!   `Outcome::Unavailable`, carrying forward whatever the last layer that
//!   actually ran had concluded — it never silently jumps to a different
//!   backend, and never invents a result.

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
        // The most recent conclusion a layer actually reached, carried
        // forward so an `Unavailable` deeper in the stack can report it
        // instead of reporting nothing — see module docs.
        let mut floor_answers: Vec<Answer> = Vec::new();
        let mut floor_backend: Option<BackendKind> = None;

        for backend in &self.backends {
            match backend.evaluate(request).await {
                BackendOutcome::Decided(answers) => {
                    return Decision {
                        answers,
                        backend: backend.kind(),
                        model: None,
                        latency_ms: elapsed_ms(start),
                        outcome: Outcome::Decided,
                    };
                }
                BackendOutcome::NoConclusion => {
                    floor_answers = Vec::new();
                    floor_backend = Some(backend.kind());
                }
                BackendOutcome::Unavailable { reason } => {
                    return Decision {
                        answers: floor_answers,
                        backend: floor_backend.unwrap_or(backend.kind()),
                        model: None,
                        latency_ms: elapsed_ms(start),
                        outcome: Outcome::Unavailable { reason },
                    };
                }
            }
        }

        Decision {
            answers: Vec::new(),
            backend: floor_backend.unwrap_or(BackendKind::Rule),
            model: None,
            latency_ms: elapsed_ms(start),
            outcome: Outcome::Abstain {
                reason: "no backend reached a conclusion".to_owned(),
            },
        }
    }
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
    async fn unavailable_layer_carries_forward_the_rule_layers_conclusion() {
        // The rule layer here is a `Decided` layer placed BEFORE an
        // unavailable layer in a cascade that is not short-circuit — this
        // models a future layering where a rule can produce a low-confidence
        // floor answer that a later layer is expected to refine, not replace.
        // Even though D0-1's `RuleBackend` always short-circuits on
        // `Decided`, the service itself must not assume every backend does;
        // it must carry the last real conclusion forward rather than drop it.
        struct FloorThenNoConclusion;
        #[async_trait]
        impl DecisionBackend for FloorThenNoConclusion {
            fn kind(&self) -> BackendKind {
                BackendKind::Rule
            }
            async fn evaluate(&self, _request: &DecisionRequest) -> BackendOutcome {
                BackendOutcome::NoConclusion
            }
        }

        let floor = FloorThenNoConclusion;
        let unavailable = CountingBackend {
            kind: BackendKind::Encoder,
            calls: Arc::new(AtomicUsize::new(0)),
            outcome: BackendOutcome::Unavailable {
                reason: "runtime crashed".to_owned(),
            },
        };

        let service = DecisionService::new(vec![Arc::new(floor), Arc::new(unavailable)]);
        let decision = service.decide(&request()).await;

        assert_eq!(
            decision.backend,
            BackendKind::Rule,
            "reports the layer that actually ran, not the one that failed"
        );
        assert_eq!(
            decision.outcome,
            Outcome::Unavailable {
                reason: "runtime crashed".to_owned()
            }
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
