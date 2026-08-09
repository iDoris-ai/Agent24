//! The "AI does not write the DB" gate as a pure, testable validator.
//!
//! Everything an AI (Local/Executive brain) or a Rule produces is a
//! [`Sin90Proposal`] — a batch of atomic [`Sin90Op`]s. It is persisted `pending`
//! and, on accept, validated by [`validate`] and applied in ONE `sin90.db`
//! transaction with CAS idempotency (SIN90-domain.md §2.3). This module is the
//! validation half: pure, no DB. The store reads current state under a write
//! lock and hands it in via [`ValidationCtx`], so `validate` never touches I/O.

use serde::{Deserialize, Serialize};

use crate::transitions::{TransitionError, check_task_transition, week_is_open};
use crate::types::{Alloc, DirectionId, RhythmId, TaskId, TaskStatus, WeekId, WeekStatus};

/// A new task to create inside a week (fields the AI proposes; ids/timestamps
/// are minted by the store on apply).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewTask {
    pub title: String,
    pub direction_id: Option<DirectionId>,
}

/// One atomic change. A proposal is an ordered batch of these.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Sin90Op {
    CreateDirection {
        title: String,
        target_window: String,
    },
    TransitionTask {
        task_id: TaskId,
        to: TaskStatus,
    },
    CreateTasks {
        week_id: WeekId,
        tasks: Vec<NewTask>,
    },
    ReorderTasks {
        week_id: WeekId,
        order: Vec<TaskId>,
    },
    AdjustRhythm {
        rhythm_id: RhythmId,
        new_alloc: Vec<Alloc>,
    },
    /// Atomic: close the source task (→ carried_over) and create a fresh task
    /// in `to_week` linked via `carried_from`.
    CarryOverTask {
        task_id: TaskId,
        to_week: WeekId,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalSource {
    LocalBrain,
    Executive,
    Rule,
}

/// Persistent proposal lifecycle (backs `sin90_proposals.status`). `applying`
/// is the CAS-claimed state that makes a re-tried accept idempotent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalStatus {
    Pending,
    Applying,
    Applied,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sin90Proposal {
    /// Client-stable id: re-submitting the same id is idempotent.
    pub id: String,
    pub status: ProposalStatus,
    pub source: ProposalSource,
    pub ops: Vec<Sin90Op>,
    pub rationale: Option<String>,
}

/// Read-only current-state lookup the store provides (under its write lock)
/// so validation stays pure. `None` means "no such entity".
pub trait ValidationCtx {
    fn task_status(&self, id: &TaskId) -> Option<TaskStatus>;
    fn week_status(&self, id: &WeekId) -> Option<WeekStatus>;
    fn rhythm_is_retired(&self, id: &RhythmId) -> Option<bool>;
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProposalError {
    #[error("proposal has no ops")]
    Empty,
    #[error("unknown {entity}: {id}")]
    UnknownEntity { entity: &'static str, id: String },
    #[error(transparent)]
    IllegalTransition(#[from] TransitionError),
    #[error("week {week_id} is not open (status {status:?})")]
    WeekNotOpen { week_id: WeekId, status: WeekStatus },
    #[error("rhythm {rhythm_id} is retired; cannot adjust")]
    RhythmRetired { rhythm_id: RhythmId },
    #[error("reorder for week {week_id} has an empty order list")]
    EmptyReorder { week_id: WeekId },
    #[error("allocation percentages sum to {sum_pct} (must be <= 100)")]
    InvalidAlloc { sum_pct: u32 },
}

/// Pure validation: structural checks + every state-changing op must be a legal
/// transition from the entity's *current* status (looked up via `ctx`). Returns
/// on the FIRST offending op, leaving the store to reject the whole proposal —
/// apply is all-or-nothing (SIN90-domain.md §2.3).
pub fn validate(p: &Sin90Proposal, ctx: &dyn ValidationCtx) -> Result<(), ProposalError> {
    if p.ops.is_empty() {
        return Err(ProposalError::Empty);
    }
    for op in &p.ops {
        validate_op(op, ctx)?;
    }
    Ok(())
}

fn validate_op(op: &Sin90Op, ctx: &dyn ValidationCtx) -> Result<(), ProposalError> {
    match op {
        // Structurally always valid — a brand-new entity has no prior state.
        Sin90Op::CreateDirection { .. } => Ok(()),

        Sin90Op::TransitionTask { task_id, to } => {
            let from = task_status(ctx, task_id)?;
            check_task_transition(from, *to)?;
            Ok(())
        }

        Sin90Op::CreateTasks { week_id, .. } => {
            require_open_week(ctx, week_id)?;
            Ok(())
        }

        Sin90Op::ReorderTasks { week_id, order } => {
            require_open_week(ctx, week_id)?;
            if order.is_empty() {
                return Err(ProposalError::EmptyReorder {
                    week_id: week_id.clone(),
                });
            }
            Ok(())
        }

        Sin90Op::AdjustRhythm {
            rhythm_id,
            new_alloc,
        } => {
            let retired =
                ctx.rhythm_is_retired(rhythm_id)
                    .ok_or_else(|| ProposalError::UnknownEntity {
                        entity: "rhythm",
                        id: rhythm_id.clone(),
                    })?;
            if retired {
                return Err(ProposalError::RhythmRetired {
                    rhythm_id: rhythm_id.clone(),
                });
            }
            check_alloc(new_alloc)?;
            Ok(())
        }

        Sin90Op::CarryOverTask { task_id, to_week } => {
            let from = task_status(ctx, task_id)?;
            // Closing side of the carry-over must itself be a legal task transition.
            check_task_transition(from, TaskStatus::CarriedOver)?;
            require_open_week(ctx, to_week)?;
            Ok(())
        }
    }
}

fn task_status(ctx: &dyn ValidationCtx, id: &TaskId) -> Result<TaskStatus, ProposalError> {
    ctx.task_status(id)
        .ok_or_else(|| ProposalError::UnknownEntity {
            entity: "task",
            id: id.clone(),
        })
}

fn require_open_week(ctx: &dyn ValidationCtx, id: &WeekId) -> Result<(), ProposalError> {
    let status = ctx
        .week_status(id)
        .ok_or_else(|| ProposalError::UnknownEntity {
            entity: "week",
            id: id.clone(),
        })?;
    if week_is_open(status) {
        Ok(())
    } else {
        Err(ProposalError::WeekNotOpen {
            week_id: id.clone(),
            status,
        })
    }
}

fn check_alloc(alloc: &[Alloc]) -> Result<(), ProposalError> {
    // Saturating so a maliciously huge value can't wrap; > 100 is rejected anyway.
    let sum = alloc.iter().fold(0u32, |acc, a| acc.saturating_add(a.pct));
    if sum > 100 {
        Err(ProposalError::InvalidAlloc { sum_pct: sum })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::collections::HashMap;

    use super::*;

    #[derive(Default)]
    struct MockCtx {
        tasks: HashMap<TaskId, TaskStatus>,
        weeks: HashMap<WeekId, WeekStatus>,
        rhythms_retired: HashMap<RhythmId, bool>,
    }

    impl ValidationCtx for MockCtx {
        fn task_status(&self, id: &TaskId) -> Option<TaskStatus> {
            self.tasks.get(id).copied()
        }
        fn week_status(&self, id: &WeekId) -> Option<WeekStatus> {
            self.weeks.get(id).copied()
        }
        fn rhythm_is_retired(&self, id: &RhythmId) -> Option<bool> {
            self.rhythms_retired.get(id).copied()
        }
    }

    fn proposal(ops: Vec<Sin90Op>) -> Sin90Proposal {
        Sin90Proposal {
            id: "p1".into(),
            status: ProposalStatus::Pending,
            source: ProposalSource::LocalBrain,
            ops,
            rationale: None,
        }
    }

    #[test]
    fn empty_proposal_rejected() {
        let ctx = MockCtx::default();
        assert_eq!(validate(&proposal(vec![]), &ctx), Err(ProposalError::Empty));
    }

    #[test]
    fn create_direction_always_ok() {
        let ctx = MockCtx::default();
        let p = proposal(vec![Sin90Op::CreateDirection {
            title: "iDoris site".into(),
            target_window: "2026-08".into(),
        }]);
        assert!(validate(&p, &ctx).is_ok());
    }

    #[test]
    fn transition_task_legal_and_illegal() {
        let mut ctx = MockCtx::default();
        ctx.tasks.insert("t1".into(), TaskStatus::Planned);

        let ok = proposal(vec![Sin90Op::TransitionTask {
            task_id: "t1".into(),
            to: TaskStatus::InProgress,
        }]);
        assert!(validate(&ok, &ctx).is_ok());

        let bad = proposal(vec![Sin90Op::TransitionTask {
            task_id: "t1".into(),
            to: TaskStatus::Done, // planned -> done is illegal (must go through in_progress)
        }]);
        assert!(matches!(
            validate(&bad, &ctx),
            Err(ProposalError::IllegalTransition(
                TransitionError::Task { .. }
            ))
        ));
    }

    #[test]
    fn transition_unknown_task_rejected() {
        let ctx = MockCtx::default();
        let p = proposal(vec![Sin90Op::TransitionTask {
            task_id: "ghost".into(),
            to: TaskStatus::InProgress,
        }]);
        assert_eq!(
            validate(&p, &ctx),
            Err(ProposalError::UnknownEntity {
                entity: "task",
                id: "ghost".into()
            })
        );
    }

    #[test]
    fn create_tasks_requires_open_week() {
        let mut ctx = MockCtx::default();
        ctx.weeks.insert("w_open".into(), WeekStatus::Planning);
        ctx.weeks.insert("w_closed".into(), WeekStatus::Closed);

        let ok = proposal(vec![Sin90Op::CreateTasks {
            week_id: "w_open".into(),
            tasks: vec![NewTask {
                title: "draft".into(),
                direction_id: None,
            }],
        }]);
        assert!(validate(&ok, &ctx).is_ok());

        let bad = proposal(vec![Sin90Op::CreateTasks {
            week_id: "w_closed".into(),
            tasks: vec![],
        }]);
        assert!(matches!(
            validate(&bad, &ctx),
            Err(ProposalError::WeekNotOpen { .. })
        ));
    }

    #[test]
    fn reorder_empty_order_rejected() {
        let mut ctx = MockCtx::default();
        ctx.weeks.insert("w".into(), WeekStatus::Active);
        let p = proposal(vec![Sin90Op::ReorderTasks {
            week_id: "w".into(),
            order: vec![],
        }]);
        assert!(matches!(
            validate(&p, &ctx),
            Err(ProposalError::EmptyReorder { .. })
        ));
    }

    #[test]
    fn adjust_rhythm_retired_and_alloc_rules() {
        let mut ctx = MockCtx::default();
        ctx.rhythms_retired.insert("r_live".into(), false);
        ctx.rhythms_retired.insert("r_dead".into(), true);

        let good = proposal(vec![Sin90Op::AdjustRhythm {
            rhythm_id: "r_live".into(),
            new_alloc: vec![
                Alloc {
                    direction_id: "d1".into(),
                    pct: 60,
                },
                Alloc {
                    direction_id: "d2".into(),
                    pct: 40,
                },
            ],
        }]);
        assert!(validate(&good, &ctx).is_ok());

        let over = proposal(vec![Sin90Op::AdjustRhythm {
            rhythm_id: "r_live".into(),
            new_alloc: vec![Alloc {
                direction_id: "d1".into(),
                pct: 101,
            }],
        }]);
        assert_eq!(
            validate(&over, &ctx),
            Err(ProposalError::InvalidAlloc { sum_pct: 101 })
        );

        let dead = proposal(vec![Sin90Op::AdjustRhythm {
            rhythm_id: "r_dead".into(),
            new_alloc: vec![],
        }]);
        assert!(matches!(
            validate(&dead, &ctx),
            Err(ProposalError::RhythmRetired { .. })
        ));
    }

    #[test]
    fn carry_over_needs_carryable_task_and_open_week() {
        let mut ctx = MockCtx::default();
        ctx.tasks.insert("t_prog".into(), TaskStatus::InProgress);
        ctx.tasks.insert("t_done".into(), TaskStatus::Done);
        ctx.weeks.insert("next".into(), WeekStatus::Planning);

        let ok = proposal(vec![Sin90Op::CarryOverTask {
            task_id: "t_prog".into(),
            to_week: "next".into(),
        }]);
        assert!(validate(&ok, &ctx).is_ok());

        // A done task is terminal — carrying it over is an illegal transition.
        let bad = proposal(vec![Sin90Op::CarryOverTask {
            task_id: "t_done".into(),
            to_week: "next".into(),
        }]);
        assert!(matches!(
            validate(&bad, &ctx),
            Err(ProposalError::IllegalTransition(
                TransitionError::Task { .. }
            ))
        ));
    }

    #[test]
    fn first_offending_op_stops_validation() {
        let mut ctx = MockCtx::default();
        ctx.tasks.insert("t1".into(), TaskStatus::Planned);
        // Second op references a ghost task; the batch must fail as a whole.
        let p = proposal(vec![
            Sin90Op::TransitionTask {
                task_id: "t1".into(),
                to: TaskStatus::InProgress,
            },
            Sin90Op::TransitionTask {
                task_id: "ghost".into(),
                to: TaskStatus::InProgress,
            },
        ]);
        assert!(matches!(
            validate(&p, &ctx),
            Err(ProposalError::UnknownEntity { entity: "task", .. })
        ));
    }

    #[test]
    fn op_json_tag_is_snake_case() {
        let op = Sin90Op::TransitionTask {
            task_id: "t1".into(),
            to: TaskStatus::Done,
        };
        let j = serde_json::to_string(&op).unwrap();
        assert!(j.contains("\"op\":\"transition_task\""), "{j}");
        assert!(j.contains("\"to\":\"done\""), "{j}");
    }
}
