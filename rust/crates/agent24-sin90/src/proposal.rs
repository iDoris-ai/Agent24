//! The "AI does not write the DB" gate as a pure, testable validator.
//!
//! Everything an AI (Local/Executive brain) or a Rule produces is a
//! [`Sin90Proposal`] — a batch of atomic [`Sin90Op`]s. It is persisted `pending`
//! and, on accept, validated by [`validate`] and applied in ONE `sin90.db`
//! transaction with CAS idempotency (SIN90-domain.md §2.3). This module is the
//! validation half: pure, no DB. The store reads current state under a write
//! lock and hands it in via [`ValidationCtx`], so `validate` never touches I/O.

use serde::{Deserialize, Serialize};

use std::collections::HashSet;

use crate::transitions::{TransitionError, check_task_transition, week_is_open};
use crate::types::{
    Alloc, DirectionId, ProposalStatus, RhythmId, TaskId, TaskStatus, WeekId, WeekStatus,
};

/// A new task to create inside a week (fields the AI proposes; ids/timestamps
/// are minted by the store on apply). `deny_unknown_fields`: model output with a
/// stray/mistyped key must fail loudly, not silently drop it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sin90Proposal {
    /// Client-stable id: re-submitting the same id is idempotent.
    pub id: String,
    pub status: ProposalStatus,
    pub source: ProposalSource,
    pub ops: Vec<Sin90Op>,
    pub rationale: Option<String>,
}

/// Read-only current-state lookup the store provides (under its write lock) so
/// validation stays pure. `None` means "no such entity".
///
/// SCOPE (the boundary, decided deliberately — SIN90-domain.md §2.3): this trait
/// exposes only per-entity *existence + status*. Validation here therefore
/// covers structural well-formedness and transition legality. It does NOT check
/// **relational** invariants that an FK cannot express — e.g. that a
/// `TransitionTask`'s task belongs to an *open* week, or that `CarryOverTask`'s
/// `to_week` differs from the task's current week. Those are enforced by the
/// store inside the apply transaction, under the same write lock. The trait is
/// kept narrow on purpose: every widening becomes a breaking change across every
/// implementor once the store lands.
pub trait ValidationCtx {
    fn task_status(&self, id: &str) -> Option<TaskStatus>;
    fn week_status(&self, id: &str) -> Option<WeekStatus>;
    fn rhythm_is_retired(&self, id: &str) -> Option<bool>;
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
    #[error("{op} for week {week_id} has an empty list")]
    EmptyList { op: &'static str, week_id: WeekId },
    #[error("{op} references {id} more than once")]
    DuplicateRef { op: &'static str, id: String },
    #[error("allocation percentages sum to {sum_pct} (must be <= 100)")]
    InvalidAlloc { sum_pct: u32 },
    #[error("{field} must not be blank")]
    BlankField { field: &'static str },
}

/// A working view that overlays the pending effects of earlier ops in the SAME
/// batch on top of the store snapshot. This makes a proposal validate as the
/// ordered sequence it is, not as N independent reads of the pre-batch state —
/// so `[t→InProgress, t→Done]` passes and `[t→InProgress, t→InProgress]` fails,
/// exactly as apply would behave under the write lock.
struct Working<'c> {
    ctx: &'c dyn ValidationCtx,
    task: std::collections::HashMap<String, TaskStatus>,
}

impl<'c> Working<'c> {
    fn new(ctx: &'c dyn ValidationCtx) -> Self {
        Self {
            ctx,
            task: std::collections::HashMap::new(),
        }
    }
    fn task_status(&self, id: &str) -> Option<TaskStatus> {
        self.task
            .get(id)
            .copied()
            .or_else(|| self.ctx.task_status(id))
    }
    fn set_task(&mut self, id: &str, s: TaskStatus) {
        self.task.insert(id.to_string(), s);
    }
}

/// Pure validation: structural well-formedness + every state-changing op must be
/// a legal transition from the entity's status *as of this point in the batch*
/// (store snapshot overlaid with earlier ops). Returns on the FIRST offending
/// op; the store rejects the whole proposal — apply is all-or-nothing.
/// Relational invariants are the store's job (see [`ValidationCtx`]).
pub fn validate(p: &Sin90Proposal, ctx: &dyn ValidationCtx) -> Result<(), ProposalError> {
    if p.ops.is_empty() {
        return Err(ProposalError::Empty);
    }
    let mut w = Working::new(ctx);
    for op in &p.ops {
        validate_op(op, &mut w)?;
    }
    Ok(())
}

fn validate_op(op: &Sin90Op, w: &mut Working<'_>) -> Result<(), ProposalError> {
    match op {
        Sin90Op::CreateDirection {
            title,
            target_window,
        } => {
            non_blank("title", title)?;
            non_blank("target_window", target_window)?;
            Ok(())
        }

        Sin90Op::TransitionTask { task_id, to } => {
            let from = task_status(w, task_id)?;
            check_task_transition(from, *to)?;
            w.set_task(task_id, *to);
            Ok(())
        }

        Sin90Op::CreateTasks { week_id, tasks } => {
            if tasks.is_empty() {
                return Err(ProposalError::EmptyList {
                    op: "create_tasks",
                    week_id: week_id.clone(),
                });
            }
            require_open_week(w, week_id)
        }

        Sin90Op::ReorderTasks { week_id, order } => {
            require_open_week(w, week_id)?;
            if order.is_empty() {
                return Err(ProposalError::EmptyList {
                    op: "reorder_tasks",
                    week_id: week_id.clone(),
                });
            }
            reject_dupes("reorder_tasks", order.iter())
        }

        Sin90Op::AdjustRhythm {
            rhythm_id,
            new_alloc,
        } => {
            let retired =
                w.ctx
                    .rhythm_is_retired(rhythm_id)
                    .ok_or_else(|| ProposalError::UnknownEntity {
                        entity: "rhythm",
                        id: rhythm_id.clone(),
                    })?;
            if retired {
                return Err(ProposalError::RhythmRetired {
                    rhythm_id: rhythm_id.clone(),
                });
            }
            check_alloc(new_alloc)
        }

        Sin90Op::CarryOverTask { task_id, to_week } => {
            let from = task_status(w, task_id)?;
            // Closing side of the carry-over must itself be a legal task transition.
            check_task_transition(from, TaskStatus::CarriedOver)?;
            require_open_week(w, to_week)?;
            w.set_task(task_id, TaskStatus::CarriedOver);
            Ok(())
        }
    }
}

fn non_blank(field: &'static str, value: &str) -> Result<(), ProposalError> {
    if value.trim().is_empty() {
        Err(ProposalError::BlankField { field })
    } else {
        Ok(())
    }
}

fn task_status(w: &Working<'_>, id: &str) -> Result<TaskStatus, ProposalError> {
    w.task_status(id)
        .ok_or_else(|| ProposalError::UnknownEntity {
            entity: "task",
            id: id.to_string(),
        })
}

fn require_open_week(w: &Working<'_>, id: &str) -> Result<(), ProposalError> {
    let status = w
        .ctx
        .week_status(id)
        .ok_or_else(|| ProposalError::UnknownEntity {
            entity: "week",
            id: id.to_string(),
        })?;
    if week_is_open(status) {
        Ok(())
    } else {
        Err(ProposalError::WeekNotOpen {
            week_id: id.to_string(),
            status,
        })
    }
}

fn reject_dupes<'a>(
    op: &'static str,
    ids: impl Iterator<Item = &'a String>,
) -> Result<(), ProposalError> {
    let mut seen = HashSet::new();
    for id in ids {
        if !seen.insert(id.as_str()) {
            return Err(ProposalError::DuplicateRef { op, id: id.clone() });
        }
    }
    Ok(())
}

fn check_alloc(alloc: &[Alloc]) -> Result<(), ProposalError> {
    // No ctx needed: structural checks a batch can fail on its own contents.
    let mut seen = HashSet::new();
    for a in alloc {
        if !seen.insert(a.direction_id.as_str()) {
            return Err(ProposalError::DuplicateRef {
                op: "adjust_rhythm",
                id: a.direction_id.clone(),
            });
        }
    }
    // Saturating so a huge value can't wrap; > 100 is rejected anyway.
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
        fn task_status(&self, id: &str) -> Option<TaskStatus> {
            self.tasks.get(id).copied()
        }
        fn week_status(&self, id: &str) -> Option<WeekStatus> {
            self.weeks.get(id).copied()
        }
        fn rhythm_is_retired(&self, id: &str) -> Option<bool> {
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
            tasks: vec![NewTask {
                title: "x".into(),
                direction_id: None,
            }],
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
            Err(ProposalError::EmptyList {
                op: "reorder_tasks",
                ..
            })
        ));
    }

    #[test]
    fn reorder_duplicate_ids_rejected() {
        let mut ctx = MockCtx::default();
        ctx.weeks.insert("w".into(), WeekStatus::Active);
        let p = proposal(vec![Sin90Op::ReorderTasks {
            week_id: "w".into(),
            order: vec!["t1".into(), "t2".into(), "t1".into()],
        }]);
        assert!(matches!(
            validate(&p, &ctx),
            Err(ProposalError::DuplicateRef {
                op: "reorder_tasks",
                ..
            })
        ));
    }

    #[test]
    fn create_tasks_empty_list_rejected() {
        let mut ctx = MockCtx::default();
        ctx.weeks.insert("w".into(), WeekStatus::Planning);
        let p = proposal(vec![Sin90Op::CreateTasks {
            week_id: "w".into(),
            tasks: vec![],
        }]);
        assert!(matches!(
            validate(&p, &ctx),
            Err(ProposalError::EmptyList {
                op: "create_tasks",
                ..
            })
        ));
    }

    #[test]
    fn create_direction_blank_title_rejected() {
        let ctx = MockCtx::default();
        let p = proposal(vec![Sin90Op::CreateDirection {
            title: "   ".into(),
            target_window: "2026-08".into(),
        }]);
        assert_eq!(
            validate(&p, &ctx),
            Err(ProposalError::BlankField { field: "title" })
        );
    }

    #[test]
    fn adjust_rhythm_duplicate_direction_rejected() {
        let mut ctx = MockCtx::default();
        ctx.rhythms_retired.insert("r".into(), false);
        // Sums to 100 but names d1 twice — must be caught before it hits the
        // UNIQUE(rhythm_id, direction_id) constraint at apply time.
        let p = proposal(vec![Sin90Op::AdjustRhythm {
            rhythm_id: "r".into(),
            new_alloc: vec![
                Alloc {
                    direction_id: "d1".into(),
                    pct: 50,
                },
                Alloc {
                    direction_id: "d1".into(),
                    pct: 50,
                },
            ],
        }]);
        assert!(matches!(
            validate(&p, &ctx),
            Err(ProposalError::DuplicateRef {
                op: "adjust_rhythm",
                ..
            })
        ));
    }

    // ---- intra-batch state threading (the overlay) ----

    #[test]
    fn intra_batch_sequential_transitions_pass() {
        // [planned→in_progress, in_progress→done] is a legal SEQUENCE; the old
        // snapshot-only validator wrongly rejected the second op as planned→done.
        let mut ctx = MockCtx::default();
        ctx.tasks.insert("t1".into(), TaskStatus::Planned);
        let p = proposal(vec![
            Sin90Op::TransitionTask {
                task_id: "t1".into(),
                to: TaskStatus::InProgress,
            },
            Sin90Op::TransitionTask {
                task_id: "t1".into(),
                to: TaskStatus::Done,
            },
        ]);
        assert!(validate(&p, &ctx).is_ok());
    }

    #[test]
    fn intra_batch_repeated_transition_fails() {
        // [planned→in_progress, planned→in_progress] must fail: after the first,
        // t1 is in_progress, so the second is in_progress→in_progress (illegal).
        let mut ctx = MockCtx::default();
        ctx.tasks.insert("t1".into(), TaskStatus::Planned);
        let p = proposal(vec![
            Sin90Op::TransitionTask {
                task_id: "t1".into(),
                to: TaskStatus::InProgress,
            },
            Sin90Op::TransitionTask {
                task_id: "t1".into(),
                to: TaskStatus::InProgress,
            },
        ]);
        assert!(matches!(
            validate(&p, &ctx),
            Err(ProposalError::IllegalTransition(
                TransitionError::Task { .. }
            ))
        ));
    }

    #[test]
    fn intra_batch_carry_over_then_transition_fails() {
        // [carry_over t1, t1→done]: after carry-over t1 is carried_over (terminal),
        // so the follow-up transition is illegal — caught at validate, not apply.
        let mut ctx = MockCtx::default();
        ctx.tasks.insert("t1".into(), TaskStatus::InProgress);
        ctx.weeks.insert("next".into(), WeekStatus::Planning);
        let p = proposal(vec![
            Sin90Op::CarryOverTask {
                task_id: "t1".into(),
                to_week: "next".into(),
            },
            Sin90Op::TransitionTask {
                task_id: "t1".into(),
                to: TaskStatus::Done,
            },
        ]);
        assert!(matches!(
            validate(&p, &ctx),
            Err(ProposalError::IllegalTransition(
                TransitionError::Task { .. }
            ))
        ));
    }

    #[test]
    fn deny_unknown_fields_on_new_task() {
        // A stray key in model output must fail loudly, not be silently dropped.
        let err = serde_json::from_str::<NewTask>(r#"{"title":"x","typo":1}"#);
        assert!(err.is_err(), "unknown field must be rejected");
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
