//! A decision backend is one layer of the cascade (`docs/research/DECISION-MODELS.md`
//! §7.3: 规则层 → 快速模型层 → 深度模型层). This crate ships only
//! [`RuleBackend`] — Encoder/Deep/LlmSimulation backends are D0-5/D1+ work,
//! wired in once a candidate has passed the eval harness (`eval/decide/`).

use async_trait::async_trait;

use crate::types::{Answer, BackendKind, DecisionRequest};

/// What one backend layer produced for a [`DecisionRequest`].
///
/// `NoConclusion` and `Unavailable` are deliberately distinct: a rule that
/// simply didn't match is a normal, healthy outcome (fall through to the
/// next layer); a backend that could not run at all (model not downloaded,
/// runtime crashed, network down) is not — the cascade must not treat the
/// two the same way (`docs/agent/PLAN-DECIDE.md` §0: "不静默降级").
///
/// `NoConclusion`'s `floor` is how a layer that did not reach a FINAL
/// decision can still hand something forward: empty when it truly has
/// nothing to offer ([`RuleBackend`] on a miss always sends `vec![]`), or a
/// partial/non-final answer set a future (D1+) backend chooses to report —
/// e.g. a low-confidence guess it does not want to stand on its own, but
/// that is worth keeping if the next layer turns out to be unavailable
/// rather than discarding it outright. [`crate::service::DecisionService`]
/// carries the most recent non-empty `floor` forward and returns it attached
/// to `Outcome::Unavailable`/`Outcome::Abstain` if a later layer cannot
/// produce its own `Decided` — see its module docs and the
/// `unavailable_layer_carries_the_last_floor_forward` test.
#[derive(Debug, Clone, PartialEq)]
pub enum BackendOutcome {
    Decided(Vec<Answer>),
    NoConclusion { floor: Vec<Answer> },
    Unavailable { reason: String },
}

/// One layer of the decision cascade. See [`crate::service::DecisionService`]
/// for how layers are combined.
#[async_trait]
pub trait DecisionBackend: Send + Sync {
    fn kind(&self) -> BackendKind;

    async fn evaluate(&self, request: &DecisionRequest) -> BackendOutcome;
}

type RuleFn = dyn Fn(&DecisionRequest) -> Option<Vec<Answer>> + Send + Sync;

/// A deterministic-code floor backend. Wraps a plain closure/fn so call
/// sites can lift their existing rule logic (e.g. `retain.rs`'s
/// `explicit_remember`) in without inventing a trait object just for this.
///
/// Per `docs/agent/PLAN-DECIDE.md` §3 D0-1: a hit returns `Decided`
/// immediately (the rule is precise by construction); a miss returns
/// `NoConclusion { floor: vec![] }`, never `Unavailable` — a rule backend
/// that is *present* has, by definition, run, and a rule is binary by
/// design (it does not deal in partial/low-confidence guesses), so it never
/// has a non-empty floor to offer either.
pub struct RuleBackend {
    rule: Box<RuleFn>,
}

impl RuleBackend {
    pub fn new(
        rule: impl Fn(&DecisionRequest) -> Option<Vec<Answer>> + Send + Sync + 'static,
    ) -> Self {
        Self {
            rule: Box::new(rule),
        }
    }
}

#[async_trait]
impl DecisionBackend for RuleBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Rule
    }

    async fn evaluate(&self, request: &DecisionRequest) -> BackendOutcome {
        match (self.rule)(request) {
            Some(answers) => BackendOutcome::Decided(answers),
            None => BackendOutcome::NoConclusion { floor: Vec::new() },
        }
    }
}
