//! Legal Sin90 status transitions as pure, exhaustive functions.
//!
//! Same shape as `agent24-core::transitions`: a `matches!` matrix per entity,
//! a `*_transition_allowed` predicate, a `check_*` wrapper returning a precise
//! error, and a `*_is_terminal` helper. These are the ONLY place the legal
//! Personal-OS state machines live — the store layer calls them under a write
//! lock before every UPDATE (SIN90-domain.md §2.2).

use crate::types::{
    DirectionStatus, ReviewStatus, RhythmStatus, ScheduleBlockStatus, TaskStatus, WeekStatus,
};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransitionError {
    #[error("illegal direction transition: {from:?} -> {to:?}")]
    Direction {
        from: DirectionStatus,
        to: DirectionStatus,
    },
    #[error("illegal task transition: {from:?} -> {to:?}")]
    Task { from: TaskStatus, to: TaskStatus },
    #[error("illegal week transition: {from:?} -> {to:?}")]
    Week { from: WeekStatus, to: WeekStatus },
    #[error("illegal schedule-block transition: {from:?} -> {to:?}")]
    ScheduleBlock {
        from: ScheduleBlockStatus,
        to: ScheduleBlockStatus,
    },
    #[error("illegal rhythm transition: {from:?} -> {to:?}")]
    Rhythm {
        from: RhythmStatus,
        to: RhythmStatus,
    },
    #[error("illegal review transition: {from:?} -> {to:?}")]
    Review {
        from: ReviewStatus,
        to: ReviewStatus,
    },
}

// ---------------------------------------------------------------------------
// Direction: draft → active → {achieved, abandoned, paused}; paused → active
// ---------------------------------------------------------------------------

pub fn direction_transition_allowed(from: DirectionStatus, to: DirectionStatus) -> bool {
    use DirectionStatus::*;
    matches!(
        (from, to),
        (Draft, Active)
            | (Active, Achieved)
            | (Active, Abandoned)
            | (Active, Paused)
            | (Paused, Active)
    )
}

pub fn check_direction_transition(
    from: DirectionStatus,
    to: DirectionStatus,
) -> Result<(), TransitionError> {
    if direction_transition_allowed(from, to) {
        Ok(())
    } else {
        Err(TransitionError::Direction { from, to })
    }
}

pub fn direction_is_terminal(s: DirectionStatus) -> bool {
    use DirectionStatus::*;
    matches!(s, Achieved | Abandoned)
}

// ---------------------------------------------------------------------------
// Task: backlog → planned → in_progress → {done, dropped, carried_over}
//       backlog → dropped ; planned → dropped ; planned → carried_over
//
// carried_over is terminal: the original task closes and a carry-over op
// atomically creates a fresh next-week task (SIN90-domain.md §2.2/§2.3).
// NOTE: planned → carried_over is included beyond the spec's §2.2 table so a
// planned-but-unstarted task can be carried at week close; documented so code
// and the spec stay reconciled.
// ---------------------------------------------------------------------------

pub fn task_transition_allowed(from: TaskStatus, to: TaskStatus) -> bool {
    use TaskStatus::*;
    matches!(
        (from, to),
        (Backlog, Planned)
            | (Backlog, Dropped)
            | (Planned, InProgress)
            | (Planned, Dropped)
            | (Planned, CarriedOver)
            | (InProgress, Done)
            | (InProgress, Dropped)
            | (InProgress, CarriedOver)
    )
}

pub fn check_task_transition(from: TaskStatus, to: TaskStatus) -> Result<(), TransitionError> {
    if task_transition_allowed(from, to) {
        Ok(())
    } else {
        Err(TransitionError::Task { from, to })
    }
}

pub fn task_is_terminal(s: TaskStatus) -> bool {
    use TaskStatus::*;
    matches!(s, Done | Dropped | CarriedOver)
}

// ---------------------------------------------------------------------------
// Week: planning → active → reviewing → closed (linear)
// ---------------------------------------------------------------------------

pub fn week_transition_allowed(from: WeekStatus, to: WeekStatus) -> bool {
    use WeekStatus::*;
    matches!(
        (from, to),
        (Planning, Active) | (Active, Reviewing) | (Reviewing, Closed)
    )
}

pub fn check_week_transition(from: WeekStatus, to: WeekStatus) -> Result<(), TransitionError> {
    if week_transition_allowed(from, to) {
        Ok(())
    } else {
        Err(TransitionError::Week { from, to })
    }
}

pub fn week_is_terminal(s: WeekStatus) -> bool {
    matches!(s, WeekStatus::Closed)
}

/// A week accepts new tasks / carry-overs only while it is still being shaped.
pub fn week_is_open(s: WeekStatus) -> bool {
    use WeekStatus::*;
    matches!(s, Planning | Active)
}

// ---------------------------------------------------------------------------
// ScheduleBlock: planned → started → {completed, skipped}; planned → skipped
// ---------------------------------------------------------------------------

pub fn schedule_block_transition_allowed(
    from: ScheduleBlockStatus,
    to: ScheduleBlockStatus,
) -> bool {
    use ScheduleBlockStatus::*;
    matches!(
        (from, to),
        (Planned, Started) | (Planned, Skipped) | (Started, Completed) | (Started, Skipped)
    )
}

pub fn check_schedule_block_transition(
    from: ScheduleBlockStatus,
    to: ScheduleBlockStatus,
) -> Result<(), TransitionError> {
    if schedule_block_transition_allowed(from, to) {
        Ok(())
    } else {
        Err(TransitionError::ScheduleBlock { from, to })
    }
}

pub fn schedule_block_is_terminal(s: ScheduleBlockStatus) -> bool {
    use ScheduleBlockStatus::*;
    matches!(s, Completed | Skipped)
}

// ---------------------------------------------------------------------------
// Rhythm: active → adjusted → retired; active → retired
// ---------------------------------------------------------------------------

pub fn rhythm_transition_allowed(from: RhythmStatus, to: RhythmStatus) -> bool {
    use RhythmStatus::*;
    matches!(
        (from, to),
        (Active, Adjusted) | (Active, Retired) | (Adjusted, Retired)
    )
}

pub fn check_rhythm_transition(
    from: RhythmStatus,
    to: RhythmStatus,
) -> Result<(), TransitionError> {
    if rhythm_transition_allowed(from, to) {
        Ok(())
    } else {
        Err(TransitionError::Rhythm { from, to })
    }
}

pub fn rhythm_is_terminal(s: RhythmStatus) -> bool {
    matches!(s, RhythmStatus::Retired)
}

// ---------------------------------------------------------------------------
// Review: draft → finalized
// ---------------------------------------------------------------------------

pub fn review_transition_allowed(from: ReviewStatus, to: ReviewStatus) -> bool {
    use ReviewStatus::*;
    matches!((from, to), (Draft, Finalized))
}

pub fn check_review_transition(
    from: ReviewStatus,
    to: ReviewStatus,
) -> Result<(), TransitionError> {
    if review_transition_allowed(from, to) {
        Ok(())
    } else {
        Err(TransitionError::Review { from, to })
    }
}

pub fn review_is_terminal(s: ReviewStatus) -> bool {
    matches!(s, ReviewStatus::Finalized)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    const DIRECTION_ALL: [DirectionStatus; 5] = [
        DirectionStatus::Draft,
        DirectionStatus::Active,
        DirectionStatus::Paused,
        DirectionStatus::Achieved,
        DirectionStatus::Abandoned,
    ];
    const TASK_ALL: [TaskStatus; 6] = [
        TaskStatus::Backlog,
        TaskStatus::Planned,
        TaskStatus::InProgress,
        TaskStatus::Done,
        TaskStatus::Dropped,
        TaskStatus::CarriedOver,
    ];
    const WEEK_ALL: [WeekStatus; 4] = [
        WeekStatus::Planning,
        WeekStatus::Active,
        WeekStatus::Reviewing,
        WeekStatus::Closed,
    ];
    const BLOCK_ALL: [ScheduleBlockStatus; 4] = [
        ScheduleBlockStatus::Planned,
        ScheduleBlockStatus::Started,
        ScheduleBlockStatus::Completed,
        ScheduleBlockStatus::Skipped,
    ];
    const RHYTHM_ALL: [RhythmStatus; 3] = [
        RhythmStatus::Active,
        RhythmStatus::Adjusted,
        RhythmStatus::Retired,
    ];
    const REVIEW_ALL: [ReviewStatus; 2] = [ReviewStatus::Draft, ReviewStatus::Finalized];

    #[test]
    fn direction_matrix() {
        use DirectionStatus::*;
        let legal = [
            (Draft, Active),
            (Active, Achieved),
            (Active, Abandoned),
            (Active, Paused),
            (Paused, Active),
        ];
        let mut count = 0;
        for from in DIRECTION_ALL {
            for to in DIRECTION_ALL {
                let expected = legal.contains(&(from, to));
                assert_eq!(
                    direction_transition_allowed(from, to),
                    expected,
                    "({from:?} -> {to:?})"
                );
                assert_eq!(check_direction_transition(from, to).is_ok(), expected);
                if expected {
                    count += 1;
                }
            }
        }
        assert_eq!(count, legal.len());
    }

    #[test]
    fn task_matrix() {
        use TaskStatus::*;
        let legal = [
            (Backlog, Planned),
            (Backlog, Dropped),
            (Planned, InProgress),
            (Planned, Dropped),
            (Planned, CarriedOver),
            (InProgress, Done),
            (InProgress, Dropped),
            (InProgress, CarriedOver),
        ];
        let mut count = 0;
        for from in TASK_ALL {
            for to in TASK_ALL {
                let expected = legal.contains(&(from, to));
                assert_eq!(
                    task_transition_allowed(from, to),
                    expected,
                    "({from:?} -> {to:?})"
                );
                if expected {
                    count += 1;
                }
            }
        }
        assert_eq!(count, legal.len());
    }

    #[test]
    fn week_matrix() {
        use WeekStatus::*;
        let legal = [(Planning, Active), (Active, Reviewing), (Reviewing, Closed)];
        for from in WEEK_ALL {
            for to in WEEK_ALL {
                assert_eq!(
                    week_transition_allowed(from, to),
                    legal.contains(&(from, to)),
                    "({from:?} -> {to:?})"
                );
            }
        }
    }

    #[test]
    fn block_matrix() {
        use ScheduleBlockStatus::*;
        let legal = [
            (Planned, Started),
            (Planned, Skipped),
            (Started, Completed),
            (Started, Skipped),
        ];
        for from in BLOCK_ALL {
            for to in BLOCK_ALL {
                assert_eq!(
                    schedule_block_transition_allowed(from, to),
                    legal.contains(&(from, to)),
                    "({from:?} -> {to:?})"
                );
            }
        }
    }

    #[test]
    fn rhythm_matrix() {
        use RhythmStatus::*;
        let legal = [(Active, Adjusted), (Active, Retired), (Adjusted, Retired)];
        for from in RHYTHM_ALL {
            for to in RHYTHM_ALL {
                assert_eq!(
                    rhythm_transition_allowed(from, to),
                    legal.contains(&(from, to)),
                    "({from:?} -> {to:?})"
                );
            }
        }
    }

    #[test]
    fn review_matrix() {
        use ReviewStatus::*;
        for from in REVIEW_ALL {
            for to in REVIEW_ALL {
                let expected = from == Draft && to == Finalized;
                assert_eq!(review_transition_allowed(from, to), expected);
            }
        }
    }

    #[test]
    fn terminals_have_no_outgoing_edges() {
        for s in DIRECTION_ALL {
            if direction_is_terminal(s) {
                assert!(
                    DIRECTION_ALL
                        .iter()
                        .all(|&t| !direction_transition_allowed(s, t))
                );
            }
        }
        for s in TASK_ALL {
            if task_is_terminal(s) {
                assert!(TASK_ALL.iter().all(|&t| !task_transition_allowed(s, t)));
            }
        }
        for s in BLOCK_ALL {
            if schedule_block_is_terminal(s) {
                assert!(
                    BLOCK_ALL
                        .iter()
                        .all(|&t| !schedule_block_transition_allowed(s, t))
                );
            }
        }
        for s in RHYTHM_ALL {
            if rhythm_is_terminal(s) {
                assert!(RHYTHM_ALL.iter().all(|&t| !rhythm_transition_allowed(s, t)));
            }
        }
    }

    #[test]
    fn week_open_only_before_close() {
        assert!(week_is_open(WeekStatus::Planning));
        assert!(week_is_open(WeekStatus::Active));
        assert!(!week_is_open(WeekStatus::Reviewing));
        assert!(!week_is_open(WeekStatus::Closed));
    }
}
