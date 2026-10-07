//! Wire-ish value types for the decision service. These are plain data —
//! no backend logic lives here (that is [`crate::backend`] / [`crate::service`]).
//!
//! Naming follows TypeSafe/Jev's `/v1/systemone` shape (choice / noul / score +
//! calibrated probability) per `docs/research/DECISION-MODELS.md` §7.3 — Jev
//! itself is closed-source and not used, only its interface vocabulary is
//! borrowed.

use serde::{Deserialize, Serialize};

/// A decision point ID, e.g. `retain.intent`, `recall.gate`, `guardian.risk`.
///
/// Deliberately an open newtype over `String`, not a closed enum — new
/// decision points are added across D1–D3 without needing a crate release.
/// See [`crate::points`] for the IDs already in use.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DecisionPoint(String);

impl DecisionPoint {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for DecisionPoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for DecisionPoint {
    fn from(id: &str) -> Self {
        Self::new(id)
    }
}

impl From<String> for DecisionPoint {
    fn from(id: String) -> Self {
        Self::new(id)
    }
}

/// One question put to a backend. Three shapes only — this is deliberately
/// not an open-ended schema: `Choice` is a closed label set, `Noul` is a
/// yes/no question (the Jev interface's own name for it, not a typo of
/// "bool"), `Score` is a bounded numeric range.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Question {
    Choice {
        id: String,
        prompt: String,
        labels: Vec<String>,
    },
    Noul {
        id: String,
        prompt: String,
    },
    Score {
        id: String,
        prompt: String,
        min: f32,
        max: f32,
    },
}

impl Question {
    pub fn id(&self) -> &str {
        match self {
            Question::Choice { id, .. }
            | Question::Noul { id, .. }
            | Question::Score { id, .. } => id,
        }
    }
}

/// How reversible the action gated by this decision is. Drives whether an
/// [`crate::threshold::Action::Escalate`] is even survivable to get wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SideEffectClass {
    None,
    Reversible,
    Irreversible,
}

/// A single request to the decision service.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionRequest {
    pub point: DecisionPoint,
    /// Caller's original text. Stored/logged locally only (D0-2); never
    /// leaves the device, never goes in telemetry.
    pub input: String,
    /// Structured context features — NOT the whole conversation (see
    /// `docs/agent/PLAN-DECIDE.md` §2.1).
    pub context: serde_json::Value,
    pub questions: Vec<Question>,
    pub side_effect: SideEffectClass,
}

/// Which layer produced a [`Decision`]. `LlmSimulation` means a general LLM
/// was asked to *simulate* a calibrated classifier's output shape — its
/// numbers are not calibrated probabilities, see [`Answer::calibrated`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    Rule,
    Encoder,
    Deep,
    LlmSimulation,
}

/// Identifies a model version precisely enough to reproduce a decision later
/// (pin + sha256 live in the model catalog, D0-3; this is just the reference).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRef {
    pub id: String,
    pub revision: String,
}

/// The value side of an [`Answer`], shaped like the [`Question`] it answers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum AnswerValue {
    Label(String),
    Bool(bool),
    Score(f32),
}

/// One backend's answer to one [`Question`].
///
/// `calibrated` is the load-bearing field: a caller must never be able to
/// claim a `BackendKind::LlmSimulation` answer is a calibrated probability
/// just by passing `calibrated: true`. [`Answer::new`] takes the backend kind
/// and overrides the flag itself — the invariant holds regardless of what the
/// caller asks for, it is not left to caller discipline.
///
/// Fields are private on purpose: the only way to build one is through
/// [`Answer::new`], so the override can never be bypassed by a struct
/// literal. There is deliberately no `Deserialize` impl — this type is not
/// (yet) accepted from outside the process; D0-2's decision log serializes it
/// one-way.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Answer {
    question_id: String,
    value: AnswerValue,
    p: Option<f32>,
    calibrated: bool,
}

impl Answer {
    /// `calibrated` is only honored for backends that can actually report a
    /// calibrated probability. For [`BackendKind::LlmSimulation`] it is
    /// always forced to `false`, no matter what the caller passes.
    pub fn new(
        question_id: impl Into<String>,
        value: AnswerValue,
        p: Option<f32>,
        backend: BackendKind,
        calibrated: bool,
    ) -> Self {
        let calibrated = match backend {
            BackendKind::LlmSimulation => false,
            BackendKind::Rule | BackendKind::Encoder | BackendKind::Deep => calibrated,
        };
        Self {
            question_id: question_id.into(),
            value,
            p,
            calibrated,
        }
    }

    pub fn question_id(&self) -> &str {
        &self.question_id
    }

    pub fn value(&self) -> &AnswerValue {
        &self.value
    }

    pub fn p(&self) -> Option<f32> {
        self.p
    }

    pub fn calibrated(&self) -> bool {
        self.calibrated
    }
}

/// What the decision service ultimately produced for a [`DecisionRequest`].
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Outcome {
    Decided,
    /// Some layer concluded "ask the user" / "do nothing" rather than guess.
    Abstain {
        reason: String,
    },
    /// A layer the cascade needed could not run. Never silently replaced by
    /// a guess or by skipping to a different backend — see
    /// `docs/agent/PLAN-DECIDE.md` §0.
    Unavailable {
        reason: String,
    },
}

/// The full result of running a [`DecisionRequest`] through a
/// [`crate::service::DecisionService`].
#[derive(Debug, Clone, Serialize)]
pub struct Decision {
    pub answers: Vec<Answer>,
    pub backend: BackendKind,
    pub model: Option<ModelRef>,
    pub latency_ms: u64,
    pub outcome: Outcome,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn choice_question_carries_its_id_and_labels() {
        let q = Question::Choice {
            id: "intent".to_owned(),
            prompt: "这句话是什么意图？".to_owned(),
            labels: vec![
                "remember".to_owned(),
                "forget".to_owned(),
                "unrelated".to_owned(),
            ],
        };
        assert_eq!(q.id(), "intent");
        match &q {
            Question::Choice { labels, .. } => assert_eq!(labels.len(), 3),
            _ => panic!("expected Choice"),
        }
    }

    #[test]
    fn noul_question_is_a_yes_no_question() {
        let q = Question::Noul {
            id: "is_remember".to_owned(),
            prompt: "你是想让我记住吗？".to_owned(),
        };
        assert_eq!(q.id(), "is_remember");
        assert!(matches!(q, Question::Noul { .. }));
    }

    #[test]
    fn score_question_carries_its_bounds() {
        let q = Question::Score {
            id: "risk".to_owned(),
            prompt: "这次工具调用的风险有多高？".to_owned(),
            min: 0.0,
            max: 1.0,
        };
        assert_eq!(q.id(), "risk");
        match q {
            Question::Score { min, max, .. } => {
                assert_eq!(min, 0.0);
                assert_eq!(max, 1.0);
            }
            _ => panic!("expected Score"),
        }
    }

    #[test]
    fn llm_simulation_answers_are_never_calibrated_even_if_the_caller_asks() {
        let answer = Answer::new(
            "is_remember",
            AnswerValue::Bool(true),
            Some(0.97),
            BackendKind::LlmSimulation,
            true, // caller claims calibrated — must be overridden
        );
        assert!(
            !answer.calibrated(),
            "llm_simulation must never report calibrated=true"
        );
    }

    #[test]
    fn other_backends_keep_the_calibrated_flag_the_caller_passed() {
        let calibrated = Answer::new(
            "x",
            AnswerValue::Bool(true),
            Some(0.9),
            BackendKind::Encoder,
            true,
        );
        assert!(calibrated.calibrated());

        let uncalibrated = Answer::new(
            "x",
            AnswerValue::Bool(true),
            Some(0.9),
            BackendKind::Encoder,
            false,
        );
        assert!(!uncalibrated.calibrated());

        let rule = Answer::new(
            "x",
            AnswerValue::Bool(true),
            Some(1.0),
            BackendKind::Rule,
            true,
        );
        assert!(rule.calibrated());
    }

    #[test]
    fn decision_point_is_an_open_newtype() {
        let a = DecisionPoint::new("retain.intent");
        let b: DecisionPoint = "retain.intent".into();
        assert_eq!(a, b);
        assert_eq!(a.as_str(), "retain.intent");
    }
}
