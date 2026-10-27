//! `agent24 schedules …`: the desktop's schedule operations (`apps/desktop/
//! src/renderer/pages/agent/api.ts`: list, create, update, delete, run now) on
//! the same daemon routes. A resident daemon only, like `agent24 runs`.

use agent24_protocol::{Schedule, ScheduleAction, ScheduleCreate, ScheduleSpec, ScheduleUpdate};
use clap::{Args, Subcommand};

use super::runs::{decode, pretty, resident, segment, send, short};
use super::{Endpoint, bearer, client, finish};

#[derive(Subcommand)]
pub(crate) enum SchedulesAction {
    /// List schedules (`GET /api/v1/schedules`)
    List {
        #[arg(long)]
        json: bool,
    },
    /// Create a schedule that starts an agent run
    Create(New),
    /// Change a schedule; only what is given changes
    Update {
        id: String,
        #[command(flatten)]
        change: Change,
    },
    /// Delete a schedule (its runs stay)
    Delete { id: String },
    /// Fire a schedule now
    RunNow { id: String },
}

#[derive(Args)]
pub(crate) struct New {
    #[arg(long)]
    name: String,
    #[arg(long)]
    prompt: String,
    #[command(flatten)]
    when: When,
    /// Run in this session instead of a new transient run each time
    #[arg(long)]
    session: Option<String>,
    #[arg(long)]
    model: Option<String>,
    /// Create it turned off
    #[arg(long)]
    disabled: bool,
}

#[derive(Args)]
pub(crate) struct Change {
    #[arg(long)]
    name: Option<String>,
    #[command(flatten)]
    when: When,
    /// New prompt; the session and model stay unless given too
    #[arg(long)]
    prompt: Option<String>,
    #[arg(long)]
    session: Option<String>,
    #[arg(long)]
    model: Option<String>,
    #[arg(long, conflicts_with = "disable")]
    enable: bool,
    #[arg(long)]
    disable: bool,
}

/// When a schedule fires: at most one of these.
#[derive(Args)]
pub(crate) struct When {
    /// Every N seconds (60 to 86400)
    #[arg(long, conflicts_with_all = ["cron", "at"])]
    every: Option<u32>,
    /// A 5- or 6-field cron expression
    #[arg(long, conflicts_with = "at")]
    cron: Option<String>,
    /// IANA timezone for --cron (default UTC; an update keeps the one it had)
    #[arg(long)]
    tz: Option<String>,
    /// Once, at this RFC 3339 time
    #[arg(long)]
    at: Option<String>,
}

impl When {
    /// Checked here, not by clap: `requires` does not hold across the
    /// conflicting flags of a flattened group.
    fn spec(self) -> Result<Option<ScheduleSpec>, String> {
        if self.tz.is_some() && self.cron.is_none() {
            return Err("--tz only goes with --cron".to_owned());
        }
        Ok(match (self.every, self.cron, self.at) {
            (Some(secs), ..) => Some(ScheduleSpec::Every { secs }),
            (_, Some(expr), _) => Some(ScheduleSpec::Cron { expr, tz: self.tz }),
            (.., at) => at.map(|ts| ScheduleSpec::At { ts }),
        })
    }
}

impl New {
    fn body(self) -> Result<ScheduleCreate, String> {
        let spec = (self.when.spec()?)
            .ok_or("say when it runs: --every <secs>, --cron <expr> or --at <time>")?;
        let action = ScheduleAction::AgentRun {
            prompt: self.prompt,
            session_id: self.session,
            model_override: self.model,
        };
        let (name, enabled, delivery) = (self.name, !self.disabled, Vec::new());
        Ok(ScheduleCreate {
            name,
            enabled,
            spec,
            action,
            delivery,
        })
    }
}

impl Change {
    fn touches_action(&self) -> bool {
        self.prompt.is_some() || self.session.is_some() || self.model.is_some()
    }

    /// Whether `patch` needs the schedule as it is now.
    fn needs_current(&self) -> bool {
        self.touches_action() || (self.when.cron.is_some() && self.when.tz.is_none())
    }

    /// The daemon replaces action and spec whole, so `current` fills in what was
    /// not given (session, model, timezone). A write in between is lost: no revision.
    fn patch(mut self, current: Option<Schedule>) -> Result<ScheduleUpdate, String> {
        let (current, spec) = current.map_or((None, None), |s| (s.action, Some(s.spec)));
        if let (true, Some(ScheduleSpec::Cron { tz, .. })) = (self.when.tz.is_none(), spec) {
            self.when.tz = tz.filter(|_| self.when.cron.is_some());
        }
        let action = if self.touches_action() {
            let Some(ScheduleAction::AgentRun {
                prompt,
                session_id,
                model_override,
            }) = current
            else {
                return Err("a module's schedule has no prompt to change".into());
            };
            Some(ScheduleAction::AgentRun {
                prompt: self.prompt.unwrap_or(prompt),
                session_id: self.session.or(session_id),
                model_override: self.model.or(model_override),
            })
        } else {
            None
        };
        let enabled = (self.enable || self.disable).then_some(self.enable);
        let (name, spec, delivery) = (self.name, self.when.spec()?, None);
        let patch = ScheduleUpdate {
            name,
            enabled,
            spec,
            action,
            delivery,
        };
        if patch == ScheduleUpdate::default() {
            return Err("nothing to change; see `agent24 schedules update --help`".into());
        }
        Ok(patch)
    }
}

fn spec_text(spec: &ScheduleSpec) -> String {
    match spec {
        ScheduleSpec::Every { secs } => format!("every {secs}s"),
        ScheduleSpec::Cron { expr, tz } => {
            format!("cron {expr:?} {}", tz.as_deref().unwrap_or("UTC"))
        }
        ScheduleSpec::At { ts } => format!("at {ts}"),
    }
}

fn schedule_lines(schedule: &Schedule) -> Vec<String> {
    let state = match (schedule.effective_enabled, schedule.enabled) {
        (true, _) => "enabled",
        (false, true) => "not firing",
        (false, false) => "disabled",
    };
    let (id, name, spec) = (&schedule.id, &schedule.name, spec_text(&schedule.spec));
    let mut lines = vec![format!("{id}  {name:?}  [{state}]  {spec}")];
    if let Some(ScheduleAction::AgentRun { prompt, .. }) = &schedule.action {
        lines.push(format!("    prompt: {:?}", short(prompt, 60)));
    }
    if let Some(owner) = &schedule.owner {
        lines.push(format!("    module {}", owner.module));
    }
    if let Some(next) = &schedule.next_run_at {
        lines.push(format!("    next {next}"));
    }
    if let Some(reason) = &schedule.system_disabled_reason {
        lines.push(format!("    disabled by the kernel: {reason}"));
    }
    lines
}

pub(crate) async fn cmd_schedules(action: SchedulesAction) -> Result<(), String> {
    let ep = resident().await?;
    let out = schedules(&ep, action).await;
    finish(ep).await;
    println!("{}", out?);
    Ok(())
}

/// What to print for `action`.
async fn schedules(ep: &Endpoint, action: SchedulesAction) -> Result<String, String> {
    let base = &ep.base;
    let one = |id: &str| Ok::<_, String>(format!("{base}/api/v1/schedules/{}", segment(id)?));
    let schedule = match action {
        SchedulesAction::List { json } => {
            let body = send(bearer(ep, client().get(format!("{base}/api/v1/schedules")))).await?;
            let list: Vec<Schedule> = decode(body["schedules"].clone())?;
            let lines: Vec<String> = list.iter().flat_map(schedule_lines).collect();
            return Ok(match (json, lines.is_empty()) {
                (true, _) => pretty(&body),
                (false, true) => "(no schedules)".to_owned(),
                (false, false) => lines.join("\n"),
            });
        }
        SchedulesAction::Create(new) => {
            let req = client()
                .post(format!("{base}/api/v1/schedules"))
                .json(&new.body()?);
            send(bearer(ep, req)).await?
        }
        SchedulesAction::Update { id, change } => {
            let url = one(&id)?;
            let current = match change.needs_current() {
                true => Some(decode(send(bearer(ep, client().get(&url))).await?)?),
                false => None,
            };
            let patch = change.patch(current)?;
            send(bearer(ep, client().patch(&url).json(&patch))).await?
        }
        SchedulesAction::Delete { id } => {
            send(bearer(ep, client().delete(one(&id)?))).await?;
            return Ok(format!("deleted {id}"));
        }
        SchedulesAction::RunNow { id } => {
            let body = send(bearer(ep, client().post(format!("{}/run_now", one(&id)?)))).await?;
            // A module's schedule answers with a fire id, not a run (design §8.2).
            return match (body["run_id"].as_str(), body["fire_id"].as_str()) {
                (Some(run), _) => Ok(format!("started {run}")),
                (None, Some(fire)) => Ok(format!("fired {fire} (a module's schedule)")),
                _ => Err("unexpected answer from the daemon: no run_id or fire_id".to_owned()),
            };
        }
    };
    Ok(schedule_lines(&decode(schedule)?).join("\n"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::runs::tests::answer_each;
    use crate::{Cli, Command};
    use clap::Parser;

    fn parse(args: &[&str]) -> Result<Command, clap::Error> {
        Cli::try_parse_from(std::iter::once("agent24").chain(args.iter().copied()))
            .map(|c| c.command)
    }

    fn create(args: &[&str]) -> Result<ScheduleCreate, String> {
        let mut all = vec!["schedules", "create"];
        all.extend_from_slice(args);
        match parse(&all).map_err(|e| e.to_string())? {
            Command::Schedules {
                action: SchedulesAction::Create(new),
            } => new.body(),
            _ => panic!("not a create"),
        }
    }

    fn update(args: &[&str], current: Option<Schedule>) -> Result<ScheduleUpdate, String> {
        let mut all = vec!["schedules", "update", "sch_1"];
        all.extend_from_slice(args);
        match parse(&all).map_err(|e| e.to_string())? {
            Command::Schedules {
                action: SchedulesAction::Update { change, .. },
            } => change.patch(current),
            _ => panic!("not an update"),
        }
    }

    fn agent_run(prompt: &str, session: Option<&str>, model: Option<&str>) -> ScheduleAction {
        ScheduleAction::AgentRun {
            prompt: prompt.to_owned(),
            session_id: session.map(str::to_owned),
            model_override: model.map(str::to_owned),
        }
    }

    #[test]
    fn create_sends_what_was_asked() {
        let body = create(&["--name", "n", "--prompt", "p", "--every", "3600"]).unwrap();
        assert_eq!(body.name, "n");
        assert!(body.enabled);
        assert_eq!(body.spec, ScheduleSpec::Every { secs: 3600 });
        assert_eq!(body.action, agent_run("p", None, None));
        assert!(body.delivery.is_empty());

        let body = create(&[
            "--name",
            "n",
            "--prompt",
            "p",
            "--cron",
            "0 9 * * *",
            "--tz",
            "Asia/Shanghai",
            "--session",
            "sess_1",
            "--model",
            "m",
            "--disabled",
        ])
        .unwrap();
        assert!(!body.enabled);
        assert_eq!(
            body.spec,
            ScheduleSpec::Cron {
                expr: "0 9 * * *".to_owned(),
                tz: Some("Asia/Shanghai".to_owned())
            }
        );
        assert_eq!(body.action, agent_run("p", Some("sess_1"), Some("m")));

        let body = create(&[
            "--name",
            "n",
            "--prompt",
            "p",
            "--at",
            "2026-11-01T09:00:00Z",
        ])
        .unwrap();
        assert_eq!(
            body.spec,
            ScheduleSpec::At {
                ts: "2026-11-01T09:00:00Z".to_owned()
            }
        );
    }

    #[test]
    fn create_needs_exactly_one_time() {
        let none = create(&["--name", "n", "--prompt", "p"]).unwrap_err();
        assert!(none.contains("--every"), "{none}");
        for two in [
            ["--every", "60", "--cron", "* * * * *"],
            ["--every", "60", "--at", "2026-11-01T09:00:00Z"],
            ["--cron", "* * * * *", "--at", "2026-11-01T09:00:00Z"],
        ] {
            let mut args = vec!["--name", "n", "--prompt", "p"];
            args.extend_from_slice(&two);
            assert!(create(&args).is_err(), "{two:?}");
        }
        let tz = create(&[
            "--name", "n", "--prompt", "p", "--every", "60", "--tz", "UTC",
        ])
        .unwrap_err();
        assert!(tz.contains("--tz"), "{tz}");
        assert!(update(&["--tz", "UTC"], None).is_err());
    }

    #[test]
    fn update_changes_only_what_was_given() {
        let patch = update(&["--name", "x"], None).unwrap();
        assert_eq!(
            patch,
            ScheduleUpdate {
                name: Some("x".to_owned()),
                ..Default::default()
            }
        );
        let patch = update(&["--disable"], None).unwrap();
        assert_eq!(
            patch,
            ScheduleUpdate {
                enabled: Some(false),
                ..Default::default()
            }
        );
        let patch = update(&["--enable"], None).unwrap();
        assert_eq!(
            patch,
            ScheduleUpdate {
                enabled: Some(true),
                ..Default::default()
            }
        );
        let patch = update(&["--every", "120"], None).unwrap();
        assert_eq!(patch.spec, Some(ScheduleSpec::Every { secs: 120 }));
        assert!(update(&["--enable", "--disable"], None).is_err());
        let nothing = update(&[], None).unwrap_err();
        assert!(nothing.contains("nothing to change"), "{nothing}");
    }

    #[test]
    fn a_new_prompt_keeps_the_session_and_model() {
        let now = schedule(agent_run("old", Some("sess_1"), Some("m")), every());
        let patch = update(&["--prompt", "new"], Some(now.clone())).unwrap();
        assert_eq!(
            patch.action,
            Some(agent_run("new", Some("sess_1"), Some("m")))
        );
        let patch = update(&["--model", "m2"], Some(now)).unwrap();
        assert_eq!(
            patch.action,
            Some(agent_run("old", Some("sess_1"), Some("m2")))
        );
        let module_row = update(&["--prompt", "new"], None).unwrap_err();
        assert!(module_row.contains("module"), "{module_row}");
    }

    fn cron(expr: &str, tz: Option<&str>) -> ScheduleSpec {
        let (expr, tz) = (expr.to_owned(), tz.map(str::to_owned));
        ScheduleSpec::Cron { expr, tz }
    }

    #[test]
    fn a_new_cron_expression_keeps_the_timezone() {
        let shanghai = cron("0 9 * * *", Some("Asia/Shanghai"));
        let now = schedule(agent_run("p", None, None), shanghai);
        let patch = update(&["--cron", "0 10 * * *"], Some(now.clone())).unwrap();
        assert_eq!(patch.spec, Some(cron("0 10 * * *", Some("Asia/Shanghai"))));
        let patch = update(&["--cron", "0 10 * * *", "--tz", "UTC"], Some(now.clone())).unwrap();
        assert_eq!(patch.spec, Some(cron("0 10 * * *", Some("UTC"))));
        // A new prompt alone leaves the spec, timezone and all, untouched.
        assert_eq!(update(&["--prompt", "q"], Some(now)).unwrap().spec, None);
        // Only a cron schedule has a timezone to keep.
        let was_every = schedule(agent_run("p", None, None), every());
        let patch = update(&["--cron", "0 10 * * *"], Some(was_every)).unwrap();
        assert_eq!(patch.spec, Some(cron("0 10 * * *", None)));
    }

    /// A cron schedule changed to every N with a new prompt: it is read first
    /// (for the session and model), and its timezone does not follow.
    #[test]
    fn a_cron_schedule_can_become_every_n_with_a_new_prompt() {
        let now = schedule(
            agent_run("p", Some("sess_1"), Some("m")),
            cron("0 9 * * *", Some("Asia/Shanghai")),
        );
        let patch = update(&["--every", "120", "--prompt", "q"], Some(now)).unwrap();
        assert_eq!(patch.spec, Some(ScheduleSpec::Every { secs: 120 }));
        assert_eq!(
            patch.action,
            Some(agent_run("q", Some("sess_1"), Some("m")))
        );
    }

    fn every() -> ScheduleSpec {
        ScheduleSpec::Every { secs: 3600 }
    }

    fn schedule(action: ScheduleAction, spec: ScheduleSpec) -> Schedule {
        serde_json::from_str(&schedule_json(action, spec)).unwrap()
    }

    fn schedule_json(action: ScheduleAction, spec: ScheduleSpec) -> String {
        serde_json::json!({
            "id": "sch_1", "name": "daily", "enabled": true,
            "spec": spec, "action": action, "delivery": [],
            "last_run_at": null, "next_run_at": "2026-10-27T00:00:00Z", "consecutive_failures": 0,
            "owner": null, "user_suspended": false, "system_disabled_reason": null,
            "effective_enabled": true, "disabled_by": null
        })
        .to_string()
    }

    async fn run(action: &[&str], ep: &Endpoint) -> Result<String, String> {
        let mut all = vec!["schedules"];
        all.extend_from_slice(action);
        match parse(&all).unwrap() {
            Command::Schedules { action } => schedules(ep, action).await,
            _ => panic!("not schedules"),
        }
    }

    #[tokio::test]
    async fn a_new_prompt_is_read_then_patched_with_the_session_kept() {
        let now = schedule_json(agent_run("old", Some("sess_1"), None), every());
        let after = schedule_json(agent_run("new", Some("sess_1"), None), every());
        let (ep, seen) = answer_each(vec![("200 OK", now), ("200 OK", after)]);
        let out = run(&["update", "sch_1", "--prompt", "new"], &ep)
            .await
            .unwrap();
        assert!(out.contains("prompt: \"new\""), "{out}");
        let seen = seen.join().unwrap();
        assert!(
            seen[0].starts_with("GET /api/v1/schedules/sch_1 HTTP/1.1"),
            "{}",
            seen[0]
        );
        assert!(
            seen[1].starts_with("PATCH /api/v1/schedules/sch_1 HTTP/1.1"),
            "{}",
            seen[1]
        );
        let body = seen[1].split("\r\n\r\n").nth(1).unwrap();
        let patch: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(patch["action"]["session_id"], "sess_1", "{patch}");
        assert_eq!(patch["action"]["prompt"], "new", "{patch}");
    }

    #[tokio::test]
    async fn a_new_cron_expression_alone_reads_the_timezone_first() {
        let shanghai = cron("0 9 * * *", Some("Asia/Shanghai"));
        let now = schedule_json(agent_run("p", None, None), shanghai);
        let (ep, seen) = answer_each(vec![("200 OK", now.clone()), ("200 OK", now)]);
        run(&["update", "sch_1", "--cron", "0 10 * * *"], &ep)
            .await
            .unwrap();
        let seen = seen.join().unwrap();
        assert!(seen[0].starts_with("GET "), "{}", seen[0]);
        let body = seen[1].split("\r\n\r\n").nth(1).unwrap();
        let patch: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(patch["spec"]["tz"], "Asia/Shanghai", "{patch}");
        assert!(patch["action"].is_null(), "{patch}");
    }

    #[tokio::test]
    async fn delete_takes_the_204_and_run_now_tells_a_run_from_a_fire() {
        let (ep, seen) = answer_each(vec![("204 No Content", String::new())]);
        assert_eq!(
            run(&["delete", "sch_1"], &ep).await.unwrap(),
            "deleted sch_1"
        );
        let seen = seen.join().unwrap();
        assert!(
            seen[0].starts_with("DELETE /api/v1/schedules/sch_1 HTTP/1.1"),
            "{}",
            seen[0]
        );
        for (body, want) in [
            (r#"{"run_id":"run_9"}"#, Ok("started run_9".to_owned())),
            (
                r#"{"fire_id":"fire_1"}"#,
                Ok("fired fire_1 (a module's schedule)".to_owned()),
            ),
            (
                "{}",
                Err("unexpected answer from the daemon: no run_id or fire_id".to_owned()),
            ),
        ] {
            let (ep, seen) = answer_each(vec![("202 Accepted", body.to_owned())]);
            assert_eq!(run(&["run-now", "sch_1"], &ep).await, want);
            let seen = seen.join().unwrap();
            assert!(
                seen[0].starts_with("POST /api/v1/schedules/sch_1/run_now HTTP/1.1"),
                "{}",
                seen[0]
            );
        }
    }

    #[tokio::test]
    async fn list_reads_the_envelope_and_a_bad_id_sends_nothing() {
        let list = format!(
            r#"{{"schedules":[{}]}}"#,
            schedule_json(agent_run("p", None, None), every())
        );
        let (ep, _seen) = answer_each(vec![("200 OK", list)]);
        let out = run(&["list"], &ep).await.unwrap();
        assert!(
            out.starts_with("sch_1  \"daily\"  [enabled]  every 3600s"),
            "{out}"
        );
        let (ep, _seen) = answer_each(vec![("200 OK", r#"{"items":[]}"#.to_owned())]);
        assert!(run(&["list", "--json"], &ep).await.is_err());
        let ep = Endpoint {
            base: "http://127.0.0.1:9".to_owned(),
            token: String::new(),
            child: None,
        };
        for args in [
            ["delete", "../shutdown#"],
            ["run-now", "a/b"],
            ["update", "a?b"],
        ] {
            let mut all = args.to_vec();
            all.extend_from_slice(&["--name", "x"][..if args[0] == "update" { 2 } else { 0 }]);
            assert!(
                run(&all, &ep).await.unwrap_err().starts_with("not an id"),
                "{args:?}"
            );
        }
    }
}
