//! L7-K1: READONLY R3.7(b), the Provider's merge of a search whose knowledge
//! bases sit in more than one score space (R3.6). The server refuses such a
//! request as `mixed_score_space` and names the groups; each group is asked
//! alone (`DaemonKnowledgeBackend::search_groups`) and the answers are put
//! together here: groups take turns by rank, a chunk is kept once, the total
//! is cut to the caller's `max_results` and ranked again. Scores come from
//! different answers and cannot be compared (R3.6), so none is used to order.

use std::collections::HashSet;

use super::{Failure, LocalReason, empty_scope, local, positive};
use crate::{
    Completeness, ErrorDetails, ErrorEnvelope, ForbiddenReason, InvalidRequestReason, NonEmpty,
    PartialFailure, PartialFailureCode, PartialFailureReason, ProtocolVersion, Scope, SearchHit,
    SearchRequest, SearchResponse, UnavailableReason,
};

/// The groups a `mixed_score_space` envelope names, each a list of ids.
pub(super) fn score_groups(envelope: &ErrorEnvelope) -> Option<Vec<Vec<String>>> {
    let ErrorDetails::InvalidRequest(d) = &envelope.error.details else {
        return None;
    };
    if d.reason != Some(InvalidRequestReason::MixedScoreSpace) {
        return None;
    }
    let groups = d.groups.as_ref()?;
    Some(
        groups
            .as_slice()
            .iter()
            .map(|g| g.knowledge_base_ids.as_slice().to_vec())
            .collect(),
    )
}

/// The groups to ask, in the order of the scope asked (R3.7(b): the group
/// whose knowledge base comes first goes first). `None` when a group names a
/// knowledge base that was not asked, or one twice: nothing is sent then.
pub(super) fn checked(asked: &[String], groups: Vec<Vec<String>>) -> Option<Vec<Vec<String>>> {
    let mut seen = HashSet::new();
    for id in groups.iter().flatten() {
        if !asked.contains(id) || !seen.insert(id.as_str()) {
            return None;
        }
    }
    let mut ordered = groups;
    ordered.sort_by_key(|g| {
        g.iter()
            .filter_map(|id| asked.iter().position(|a| a == id))
            .min()
            .unwrap_or(usize::MAX)
    });
    Some(ordered)
}

/// What comes back stays inside what was asked (R2.1, R3.8).
pub(super) fn inside(asked: &[String], response: &SearchResponse) -> bool {
    let effective = response.effective_scope.knowledge_base_ids.as_slice();
    let failed = response
        .partial_failures
        .as_ref()
        .map_or(&[][..], NonEmpty::as_slice);
    effective.iter().all(|id| asked.contains(id))
        && response
            .hits
            .iter()
            .all(|hit| effective.contains(&hit.knowledge_base_id))
        && failed.iter().all(|f| asked.contains(&f.knowledge_base_id))
}

#[derive(Default)]
pub(super) struct Collected {
    answers: Vec<SearchResponse>,
    failures: Vec<PartialFailure>,
}

fn failure_for(
    ids: &[String],
    code: PartialFailureCode,
    reason: PartialFailureReason,
    retryable: bool,
) -> Vec<PartialFailure> {
    ids.iter()
        .map(|id| PartialFailure {
            knowledge_base_id: id.clone(),
            code,
            reason,
            retryable,
        })
        .collect()
}

impl Collected {
    pub(super) fn answered(&mut self, response: SearchResponse) {
        self.answers.push(response);
    }

    /// A group the call's budget had no request left for: said as retryable
    /// `overloaded`, so the answer is `partial` and the user can ask again.
    pub(super) fn unsent(&mut self, group: &[String]) {
        self.failures.extend(failure_for(
            group,
            PartialFailureCode::Unavailable,
            PartialFailureReason::Overloaded,
            true,
        ));
    }

    /// A group's error. `empty_scope` leaves the group out (it is no failure);
    /// `timeout` and `unavailable` make the answer partial; any other error
    /// fails the whole call and what the other groups returned is dropped.
    pub(super) fn refused(&mut self, group: &[String], envelope: ErrorEnvelope) -> Option<Failure> {
        match &envelope.error.details {
            ErrorDetails::Forbidden(d) if d.reason == ForbiddenReason::EmptyScope => None,
            ErrorDetails::Timeout(_) => {
                self.failures.extend(failure_for(
                    group,
                    PartialFailureCode::Timeout,
                    PartialFailureReason::Timeout,
                    true,
                ));
                None
            }
            ErrorDetails::Unavailable(d) => {
                let reason = match d.reason {
                    Some(UnavailableReason::NotReady) => PartialFailureReason::NotReady,
                    Some(UnavailableReason::Overloaded) => PartialFailureReason::Overloaded,
                    Some(UnavailableReason::IndexBuilding | UnavailableReason::IndexPending) => {
                        PartialFailureReason::IndexBuilding
                    }
                    Some(UnavailableReason::UnsupportedKbType) => {
                        PartialFailureReason::UnsupportedKbType
                    }
                    Some(UnavailableReason::IndexFailed) => PartialFailureReason::IndexFailed,
                    Some(UnavailableReason::Dependency) | None => PartialFailureReason::Dependency,
                };
                self.failures.extend(failure_for(
                    group,
                    PartialFailureCode::Unavailable,
                    reason,
                    d.retryable,
                ));
                None
            }
            _ => Some(Failure::Envelope(envelope)),
        }
    }

    /// The merged answer to the request asked first.
    pub(super) fn merge(
        self,
        first: SearchRequest,
    ) -> Result<(SearchRequest, SearchResponse), Failure> {
        if self.answers.is_empty() {
            // Every group had no knowledge base to search: the same as a scope
            // that is empty. Any other group failure: nothing came back.
            return Err(if self.failures.is_empty() {
                empty_scope(&first.request_id, first.generation)
            } else {
                local(LocalReason::Dependency, true)
            });
        }
        let max = usize::try_from(first.budget.max_results.get()).unwrap_or(usize::MAX);
        let mut turns: Vec<std::vec::IntoIter<SearchHit>> = self
            .answers
            .iter()
            .map(|a| a.hits.clone().into_iter())
            .collect();
        let mut seen = HashSet::new();
        let mut hits: Vec<SearchHit> = Vec::new();
        'rounds: loop {
            let mut any = false;
            for turn in &mut turns {
                if let Some(hit) = turn.next() {
                    any = true;
                    if seen.insert(hit.chunk_id.clone()) {
                        hits.push(hit);
                        if hits.len() >= max {
                            break 'rounds;
                        }
                    }
                }
            }
            if !any {
                break;
            }
        }
        for (i, hit) in hits.iter_mut().enumerate() {
            hit.rank = positive(i as u64 + 1);
        }
        let mut effective: Vec<String> = Vec::new();
        let mut failures = self.failures;
        let mut budget = self.answers[0].applied_budget;
        let mut partial = !failures.is_empty();
        for answer in &self.answers {
            for id in answer.effective_scope.knowledge_base_ids.as_slice() {
                if !effective.contains(id) {
                    effective.push(id.clone());
                }
            }
            if let Some(f) = &answer.partial_failures {
                failures.extend(f.as_slice().iter().cloned());
            }
            partial |= answer.completeness == Completeness::Partial;
            let b = answer.applied_budget;
            budget.deadline_ms = budget.deadline_ms.min(b.deadline_ms);
            budget.max_results = budget.max_results.min(b.max_results);
            budget.max_concurrency = budget.max_concurrency.min(b.max_concurrency);
        }
        let Ok(knowledge_base_ids) = NonEmpty::try_from(effective) else {
            return Err(local(LocalReason::Incompatible, false));
        };
        let response = SearchResponse {
            protocol_version: ProtocolVersion,
            request_id: first.request_id.clone(),
            generation: first.generation,
            completeness: if partial {
                Completeness::Partial
            } else {
                Completeness::Complete
            },
            effective_scope: Scope { knowledge_base_ids },
            applied_budget: budget,
            hits,
            partial_failures: NonEmpty::try_from(failures).ok(),
        };
        // The merged answer too stays inside what was asked.
        if !inside(first.scope.knowledge_base_ids.as_slice(), &response) {
            return Err(local(LocalReason::Incompatible, false));
        }
        Ok((first, response))
    }
}
