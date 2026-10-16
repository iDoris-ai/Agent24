//! L2 answer validation (D1-2b). See
//! `docs/agent/PLAN-DECIDE-D1-2B-L2-VALIDATION.md` for the contract this
//! implements: the request is checked before any backend runs, and every
//! answer set a layer returns (`Decided` or a non-empty `NoConclusion.floor`)
//! is checked as a whole before the service trusts it.
//!
//! Only the first error is reported, as a stable `&'static str` code. Values
//! are compared exactly as given: nothing is trimmed (except to detect blank
//! IDs/labels), sorted, converted, truncated, filled in or renormalized.

use std::collections::HashSet;

use crate::types::{Answer, AnswerValue, DecisionRequest, Question};

/// Checks a request in question order: per question, ID blank/duplicate, then
/// its labels (in order) or bounds.
pub(crate) fn validate_request(request: &DecisionRequest) -> Result<(), &'static str> {
    if request.questions.is_empty() {
        return Err("empty_questions");
    }
    let mut ids: HashSet<&str> = HashSet::new();
    for question in &request.questions {
        let id = question.id();
        if id.trim().is_empty() {
            return Err("blank_question_id");
        }
        if !ids.insert(id) {
            return Err("duplicate_question_id");
        }
        match question {
            Question::Choice { labels, .. } => {
                if labels.is_empty() {
                    return Err("empty_labels");
                }
                let mut seen: HashSet<&str> = HashSet::new();
                for label in labels {
                    if label.trim().is_empty() {
                        return Err("blank_label");
                    }
                    if !seen.insert(label.as_str()) {
                        return Err("duplicate_label");
                    }
                }
            }
            Question::Noul { .. } => {}
            Question::Score { min, max, .. } => {
                if !(min.is_finite() && max.is_finite() && min <= max) {
                    return Err("invalid_score_bounds");
                }
            }
        }
    }
    Ok(())
}

/// Checks one layer's answer set against an already-validated request, in
/// received order: per answer, ID known/duplicate, kind, value, then `p`.
/// Only when `require_coverage` (a `Decided` set) is every question then
/// required to have exactly one answer.
pub(crate) fn validate_answers(
    request: &DecisionRequest,
    answers: &[Answer],
    require_coverage: bool,
) -> Result<(), &'static str> {
    let mut answered: HashSet<&str> = HashSet::new();
    for answer in answers {
        let id = answer.question_id();
        let Some(question) = request.questions.iter().find(|q| q.id() == id) else {
            return Err("unknown_question_id");
        };
        if !answered.insert(id) {
            return Err("duplicate_answer_id");
        }
        match (question, answer.value()) {
            (Question::Choice { labels, .. }, AnswerValue::Label(label)) => {
                if !labels.iter().any(|l| l == label) {
                    return Err("label_not_allowed");
                }
            }
            (Question::Noul { .. }, AnswerValue::Bool(_)) => {}
            (Question::Score { min, max, .. }, AnswerValue::Score(value)) => {
                if !(value.is_finite() && *min <= *value && *value <= *max) {
                    return Err("score_out_of_range");
                }
            }
            _ => return Err("answer_kind"),
        }
        if let Some(p) = answer.p()
            && !(p.is_finite() && (0.0..=1.0).contains(&p))
        {
            return Err("invalid_probability");
        }
    }
    if require_coverage && answered.len() != request.questions.len() {
        return Err("answer_coverage");
    }
    Ok(())
}
