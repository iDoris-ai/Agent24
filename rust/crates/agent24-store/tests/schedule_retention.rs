#![allow(clippy::unwrap_used)]

use agent24_protocol::{Run, RunInput, RunMode, RunStatus, Schedule, ScheduleSpec};
use agent24_store::{StandingGrant, Store};

const CUTOFF: &str = "2026-10-21T00:00:00Z";

fn fixture(id: &str) -> (Schedule, Run) {
    let schedule = Schedule {
        id: id.into(),
        name: id.into(),
        enabled: true,
        spec: ScheduleSpec::At {
            ts: "2026-10-01T02:00:00.500+02:00".into(),
        },
        action: Some(agent24_protocol::ScheduleAction::AgentRun {
            prompt: "wake".into(),
            session_id: None,
            model_override: None,
        }),
        delivery: vec![],
        last_run_at: None,
        next_run_at: None,
        consecutive_failures: 0,
        owner: None,
        user_suspended: false,
        system_disabled_reason: None,
        effective_enabled: true,
        disabled_by: None,
    };
    let run = Run {
        id: format!("run_{id}"),
        schedule_id: Some(id.into()),
        status: RunStatus::Completed,
        session_id: None,
        workspace_id: None,
        input: RunInput {
            prompt: "wake".into(),
            workspace_id: None,
            model_override: None,
            mode: RunMode::Normal,
            knowledge_binding_id: None,
        },
        output: None,
        error: None,
        usage: Default::default(),
        created_at: "2026-10-01T00:00:00Z".into(),
        started_at: None,
        ended_at: Some("2026-10-20T23:59:59Z".into()),
    };
    (schedule, run)
}

#[tokio::test]
async fn retention_reuses_completed_status_and_preserves_ineligible_schedules() {
    for (case, completed, reclaim) in [
        ("expired", true, true),
        ("within_retention", true, false),
        ("at_cutoff", true, false),
        ("offset_end", true, true),
        ("queued", true, false),
        ("running", true, false),
        ("awaiting_approval", true, false),
        ("failed_run", true, false),
        ("failed_trigger", false, false),
        ("failed_schedule", true, false),
        ("no_run", false, false),
        ("older_run", false, false),
        ("different_schedule", false, false),
        ("next_run", false, false),
        ("future_spec", false, false),
        ("cron", false, false),
        ("every", false, false),
        ("missing_end", true, false),
        ("invalid_end", true, false),
        ("recent_second_run", true, false),
        ("old_active_run", true, false),
    ] {
        let store = Store::open_memory().await.unwrap();
        let (mut schedule, mut run) = fixture(case);
        match case {
            "within_retention" => run.ended_at = Some("2026-10-22T00:00:00Z".into()),
            "at_cutoff" => run.ended_at = Some(CUTOFF.into()),
            "offset_end" => run.ended_at = Some("2026-10-21T01:59:59+02:00".into()),
            "queued" => run.status = RunStatus::Queued,
            "running" => run.status = RunStatus::Running,
            "awaiting_approval" => run.status = RunStatus::AwaitingApproval,
            "failed_run" => run.status = RunStatus::Failed,
            "failed_trigger" | "failed_schedule" => schedule.consecutive_failures = 1,
            "older_run" => run.created_at = "2026-09-30T23:59:59Z".into(),
            "different_schedule" => run.schedule_id = Some("other".into()),
            "next_run" => schedule.next_run_at = Some("2026-11-01T00:00:00Z".into()),
            "future_spec" => {
                schedule.spec = ScheduleSpec::At {
                    ts: "2026-11-01T00:00:00Z".into(),
                }
            }
            "cron" => {
                schedule.spec = ScheduleSpec::Cron {
                    expr: "0 * * * * *".into(),
                    tz: Some("UTC".into()),
                }
            }
            "every" => schedule.spec = ScheduleSpec::Every { secs: 60 },
            "missing_end" => run.ended_at = None,
            "invalid_end" => run.ended_at = Some("invalid".into()),
            _ => {}
        }
        store.upsert_schedule(&schedule).await.unwrap();
        if !matches!(case, "no_run" | "failed_trigger") {
            store.insert_run(&run).await.unwrap();
        }
        if matches!(case, "recent_second_run" | "old_active_run") {
            let mut second = run.clone();
            second.id = "second".into();
            if case == "recent_second_run" {
                second.ended_at = Some(CUTOFF.into());
            } else {
                second.created_at = "2026-09-01T00:00:00Z".into();
                second.status = RunStatus::Running;
            }
            store.insert_run(&second).await.unwrap();
        }
        let grant = StandingGrant {
            id: "grant".into(),
            scope_kind: "schedule".into(),
            scope_id: case.into(),
            tool: "send".into(),
            target: "inbox".into(),
            created_at: CUTOFF.into(),
        };
        store.insert_standing_grant(&grant).await.unwrap();
        assert_eq!(
            store
                .get_schedule_status(case)
                .await
                .unwrap()
                .unwrap()
                .completed,
            completed,
            "{case}"
        );
        assert_eq!(
            store.list_schedule_statuses().await.unwrap()[0].completed,
            completed,
            "{case}"
        );
        assert_eq!(
            store.reclaim_completed_schedules(CUTOFF).await.unwrap(),
            u64::from(reclaim),
            "{case}"
        );
        assert_eq!(
            store.get_schedule(case).await.unwrap().is_none(),
            reclaim,
            "{case}"
        );
        assert_eq!(
            store.list_standing_grants().await.unwrap().is_empty(),
            reclaim,
            "{case}"
        );
        if reclaim {
            assert!(
                store.get_run(&run.id).await.unwrap().is_some(),
                "run history must survive"
            );
        }
    }
}

#[tokio::test]
async fn retention_limits_each_batch_to_200_and_drains_on_later_passes() {
    let store = Store::open_memory().await.unwrap();
    for n in 0..201 {
        let (schedule, run) = fixture(&format!("schedule_{n:03}"));
        store.upsert_schedule(&schedule).await.unwrap();
        store.insert_run(&run).await.unwrap();
    }
    assert_eq!(
        store.reclaim_completed_schedules(CUTOFF).await.unwrap(),
        200
    );
    assert_eq!(store.list_schedules().await.unwrap().len(), 1);
    assert_eq!(store.reclaim_completed_schedules(CUTOFF).await.unwrap(), 1);
    assert_eq!(store.reclaim_completed_schedules(CUTOFF).await.unwrap(), 0);
}
