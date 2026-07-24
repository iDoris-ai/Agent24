//! TUI application state — the pure, testable core (no rendering, no I/O).
//!
//! Everything the UI shows and every keystroke's effect on state lives here so
//! the two hard approval contracts (codex's lesson) can be unit-tested without
//! a terminal:
//! - **explicit decision**: the only way a decision leaves the modal is the
//!   user actively confirming a highlighted `available_decisions` entry; there
//!   is no implicit/default decision.
//! - **Esc semantics**: Esc cancels the modal WITHOUT emitting any decision —
//!   the approval stays pending (it will fail closed via the server timeout,
//!   never via a UI-invented deny/approve).

use std::collections::HashMap;

use agent24_protocol::{Approval, Decision, Event, EventBody, Run, RunStatus};

/// Which panel currently has keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Runs,
    Approvals,
}

/// What a keystroke asks the outer loop to do after mutating state.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Nothing beyond the state change (just re-render)
    None,
    /// Submit this decision to POST /api/v1/approvals/{id}
    Decide {
        approval_id: String,
        decision: Decision,
    },
    /// Cancel the given run via POST /api/v1/runs/{id}/cancel
    CancelRun { run_id: String },
    /// The user asked to quit
    Quit,
}

/// The approval decision modal. Opened for one pending approval; the user
/// moves a cursor over `available_decisions` and confirms — or presses Esc.
#[derive(Debug, Clone)]
pub struct ApprovalModal {
    pub approval: Approval,
    pub cursor: usize,
    /// Some(reason) once a `deny` selection has entered reason-entry mode
    pub reason: Option<String>,
}

impl ApprovalModal {
    fn new(approval: Approval) -> Self {
        Self {
            approval,
            cursor: 0,
            reason: None,
        }
    }

    fn move_cursor(&mut self, delta: isize) {
        let n = self.approval.available_decisions.len();
        if n == 0 {
            return;
        }
        let cur = self.cursor as isize;
        self.cursor = (cur + delta).rem_euclid(n as isize) as usize;
    }

    fn selected_kind(&self) -> Option<&str> {
        self.approval
            .available_decisions
            .get(self.cursor)
            .map(String::as_str)
    }
}

pub struct App {
    /// Runs newest-first; the vec order is the display order
    runs: Vec<Run>,
    run_cursor: usize,
    /// run_id → chronological event lines for the detail panel
    run_events: HashMap<String, Vec<String>>,
    /// Pending approvals, oldest-first (FIFO — the user answers them in order)
    approvals: Vec<Approval>,
    approval_cursor: usize,
    focus: Focus,
    modal: Option<ApprovalModal>,
    /// Set when a seq gap was seen — the loop should REST-reconcile
    pub needs_reconcile: bool,
    pub last_seq: Option<u64>,
    pub should_quit: bool,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        Self {
            runs: Vec::new(),
            run_cursor: 0,
            run_events: HashMap::new(),
            approvals: Vec::new(),
            approval_cursor: 0,
            focus: Focus::Runs,
            modal: None,
            needs_reconcile: false,
            last_seq: None,
            should_quit: false,
        }
    }

    // ── accessors (rendering reads these) ────────────────────────────────────

    pub fn runs(&self) -> &[Run] {
        &self.runs
    }
    pub fn run_cursor(&self) -> usize {
        self.run_cursor
    }
    pub fn approvals(&self) -> &[Approval] {
        &self.approvals
    }
    pub fn approval_cursor(&self) -> usize {
        self.approval_cursor
    }
    pub fn focus(&self) -> Focus {
        self.focus
    }
    pub fn modal(&self) -> Option<&ApprovalModal> {
        self.modal.as_ref()
    }

    pub fn selected_run(&self) -> Option<&Run> {
        self.runs.get(self.run_cursor)
    }

    /// Event log lines for the currently selected run.
    pub fn selected_run_events(&self) -> &[String] {
        self.selected_run()
            .and_then(|r| self.run_events.get(&r.id))
            .map_or(&[], Vec::as_slice)
    }

    // ── REST reconciliation ──────────────────────────────────────────────────

    /// Replace the run list wholesale (REST truth), keeping the cursor on the
    /// same run id when possible.
    pub fn set_runs(&mut self, runs: Vec<Run>) {
        let anchor = self.selected_run().map(|r| r.id.clone());
        self.runs = runs;
        self.run_cursor = anchor
            .and_then(|id| self.runs.iter().position(|r| r.id == id))
            .unwrap_or(0)
            .min(self.runs.len().saturating_sub(1));
    }

    /// Replace the pending-approval list (REST truth). Closes the modal if its
    /// approval is no longer pending (resolved elsewhere / timed out).
    pub fn set_approvals(&mut self, approvals: Vec<Approval>) {
        self.approvals = approvals;
        self.approval_cursor = self
            .approval_cursor
            .min(self.approvals.len().saturating_sub(1));
        if let Some(modal) = &self.modal
            && !self.approvals.iter().any(|a| a.id == modal.approval.id)
        {
            self.modal = None;
        }
        self.needs_reconcile = false;
    }

    // ── WS events ────────────────────────────────────────────────────────────

    /// Apply one WS envelope. A seq gap sets `needs_reconcile` (v1 has no
    /// replay — the loop must REST-reconcile).
    pub fn apply_event(&mut self, event: &Event) {
        if let Some(prev) = self.last_seq
            && event.seq != prev + 1
        {
            self.needs_reconcile = true;
        }
        self.last_seq = Some(event.seq);

        match &event.body {
            EventBody::RunStarted(p) => {
                self.log(&p.run_id, "run started".to_owned());
                self.mark_status(&p.run_id, RunStatus::Running);
            }
            EventBody::ModelDelta(p) => self.log(&p.run_id, format!("δ {}", oneline(&p.text))),
            EventBody::ToolStarted(p) => self.log(
                &p.run_id,
                format!("→ tool {} ({})", p.tool, p.input_summary),
            ),
            EventBody::ToolCompleted(p) => self.log(
                &p.run_id,
                format!(
                    "← tool {} [{}]",
                    p.tool_call_id,
                    serde_json::to_string(&p.status)
                        .unwrap_or_default()
                        .trim_matches('"')
                ),
            ),
            EventBody::RunCompleted(p) => {
                self.log(
                    &p.run_id,
                    format!("✓ completed · {} tokens", p.usage.total_tokens),
                );
                self.mark_status(&p.run_id, RunStatus::Completed);
            }
            EventBody::RunFailed(p) => {
                self.log(&p.run_id, format!("✗ failed: {}", p.error.message));
                self.mark_status(&p.run_id, RunStatus::Failed);
            }
            EventBody::RunCancelled(p) => {
                self.log(&p.run_id, "⊘ cancelled".to_owned());
                self.mark_status(&p.run_id, RunStatus::Cancelled);
            }
            EventBody::ApprovalRequired(approval) => {
                self.log(
                    &approval.run_id,
                    format!("⏸ approval needed: {}", approval.summary),
                );
                self.mark_status(&approval.run_id, RunStatus::AwaitingApproval);
                // Dedup by id (a reconcile + a live event can both arrive)
                if !self.approvals.iter().any(|a| a.id == approval.id) {
                    self.approvals.push((**approval).clone());
                }
            }
            EventBody::ApprovalResolved(p) => {
                self.log(
                    &p.run_id,
                    format!("▶ approval {}: {}", p.approval_id, p.decision_type),
                );
                self.approvals.retain(|a| a.id != p.approval_id);
                if let Some(modal) = &self.modal
                    && modal.approval.id == p.approval_id
                {
                    self.modal = None;
                }
                self.clamp_approval_cursor();
            }
            EventBody::ScheduleFired(p) => {
                self.log(&p.run_id, format!("⏰ fired by schedule {}", p.schedule_id))
            }
            EventBody::ScheduleDisabled(_) => {}
        }
    }

    fn log(&mut self, run_id: &str, line: String) {
        self.run_events
            .entry(run_id.to_owned())
            .or_default()
            .push(line);
    }

    fn mark_status(&mut self, run_id: &str, status: RunStatus) {
        if let Some(run) = self.runs.iter_mut().find(|r| r.id == run_id) {
            run.status = status;
        }
    }

    fn clamp_approval_cursor(&mut self) {
        self.approval_cursor = self
            .approval_cursor
            .min(self.approvals.len().saturating_sub(1));
    }

    // ── key handling ─────────────────────────────────────────────────────────

    /// Handle a normalized key. Returns the [`Action`] the outer loop performs.
    pub fn on_key(&mut self, key: Key) -> Action {
        // Modal captures all keys while open
        if self.modal.is_some() {
            return self.modal_key(key);
        }
        match key {
            Key::Quit => {
                self.should_quit = true;
                Action::Quit
            }
            Key::Tab => {
                self.focus = match self.focus {
                    Focus::Runs => Focus::Approvals,
                    Focus::Approvals => Focus::Runs,
                };
                Action::None
            }
            Key::Up => {
                self.move_selection(-1);
                Action::None
            }
            Key::Down => {
                self.move_selection(1);
                Action::None
            }
            Key::Enter => {
                // Enter on the approvals panel opens the decision modal
                if self.focus == Focus::Approvals
                    && let Some(approval) = self.approvals.get(self.approval_cursor)
                {
                    self.modal = Some(ApprovalModal::new(approval.clone()));
                }
                Action::None
            }
            Key::Cancel => {
                // 'c' cancels the selected run (only meaningful on Runs focus)
                if self.focus == Focus::Runs
                    && let Some(run) = self.selected_run()
                    && !run_is_terminal(run.status)
                {
                    return Action::CancelRun {
                        run_id: run.id.clone(),
                    };
                }
                Action::None
            }
            Key::Char(_) | Key::Esc => Action::None,
        }
    }

    fn move_selection(&mut self, delta: isize) {
        match self.focus {
            Focus::Runs => self.run_cursor = step(self.run_cursor, delta, self.runs.len()),
            Focus::Approvals => {
                self.approval_cursor = step(self.approval_cursor, delta, self.approvals.len())
            }
        }
    }

    fn modal_key(&mut self, key: Key) -> Action {
        let Some(modal) = self.modal.as_mut() else {
            return Action::None;
        };
        // Reason-entry sub-mode (deny requires a non-empty reason)
        if let Some(reason) = modal.reason.as_mut() {
            match key {
                Key::Esc => {
                    // Back out of reason entry to the decision list (NOT a
                    // decision — the approval is still pending)
                    modal.reason = None;
                }
                Key::Enter => {
                    if reason.trim().is_empty() {
                        return Action::None; // deny needs a reason; keep waiting
                    }
                    let decision = Decision {
                        kind: "deny".to_owned(),
                        reason: Some(reason.clone()),
                        extra: Default::default(),
                    };
                    let approval_id = modal.approval.id.clone();
                    self.modal = None;
                    return Action::Decide {
                        approval_id,
                        decision,
                    };
                }
                Key::Char(c) => reason.push(c),
                Key::Cancel => reason.push('c'),
                _ => {}
            }
            return Action::None;
        }
        // Decision-selection mode
        match key {
            Key::Esc => {
                // Esc = cancel the modal, emit NO decision (hard contract)
                self.modal = None;
                Action::None
            }
            Key::Up => {
                modal.move_cursor(-1);
                Action::None
            }
            Key::Down => {
                modal.move_cursor(1);
                Action::None
            }
            Key::Enter => {
                let Some(kind) = modal.selected_kind().map(str::to_owned) else {
                    return Action::None;
                };
                if kind == "deny" {
                    // Enter reason-entry mode instead of emitting immediately
                    modal.reason = Some(String::new());
                    return Action::None;
                }
                let approval_id = modal.approval.id.clone();
                self.modal = None;
                Action::Decide {
                    approval_id,
                    decision: Decision {
                        kind,
                        reason: None,
                        extra: Default::default(),
                    },
                }
            }
            _ => Action::None,
        }
    }
}

/// A oneline summary of streamed text for the event log.
fn oneline(s: &str) -> String {
    let flat = s.replace(['\n', '\r'], " ");
    if flat.chars().count() > 80 {
        format!("{}…", flat.chars().take(79).collect::<String>())
    } else {
        flat
    }
}

fn step(cur: usize, delta: isize, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    ((cur as isize + delta).rem_euclid(len as isize)) as usize
}

/// Terminal run statuses (mirrors core, avoided as a dep here to keep the CLI
/// light — kept in sync with SPEC-002 §1.2).
fn run_is_terminal(status: RunStatus) -> bool {
    matches!(
        status,
        RunStatus::Completed | RunStatus::Failed | RunStatus::Cancelled
    )
}

/// Normalized key events the app understands (decoupled from crossterm so the
/// core is testable without a terminal backend).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Enter,
    Esc,
    Tab,
    /// 'q' — quit
    Quit,
    /// 'c' — cancel run (or a literal 'c' during reason entry)
    Cancel,
    Char(char),
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use agent24_protocol::{ApprovalStatus, RunInput, Usage};

    fn approval(id: &str, decisions: &[&str]) -> Approval {
        Approval {
            id: id.to_owned(),
            run_id: "run_1".to_owned(),
            tool_call_id: "tc_1".to_owned(),
            kind: "exec".to_owned(),
            summary: "run rm -rf /tmp/x".to_owned(),
            payload: Default::default(),
            available_decisions: decisions.iter().map(|s| (*s).to_owned()).collect(),
            status: ApprovalStatus::Pending,
            decision: None,
            expires_at: "2026-07-24T10:05:00Z".to_owned(),
            created_at: "2026-07-24T10:00:00Z".to_owned(),
            decided_at: None,
        }
    }

    fn run(id: &str, status: RunStatus) -> Run {
        Run {
            id: id.to_owned(),
            session_id: None,
            status,
            input: RunInput {
                prompt: "hi".to_owned(),
                model_override: None,
            },
            output: None,
            error: None,
            usage: Usage {
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
                cost_usd: 0.0,
            },
            schedule_id: None,
            created_at: "2026-07-24T10:00:00Z".to_owned(),
            started_at: None,
            ended_at: None,
        }
    }

    fn open_modal(app: &mut App) {
        app.set_approvals(vec![approval(
            "apr_1",
            &["approve", "approve_for_session", "deny", "abort"],
        )]);
        app.focus = Focus::Approvals;
        assert_eq!(app.on_key(Key::Enter), Action::None);
        assert!(app.modal().is_some());
    }

    // ── HARD CONTRACT 1: explicit decision ───────────────────────────────────

    #[test]
    fn confirming_approve_emits_exactly_that_decision() {
        let mut app = App::new();
        open_modal(&mut app);
        // cursor starts on "approve"
        let action = app.on_key(Key::Enter);
        match action {
            Action::Decide {
                approval_id,
                decision,
            } => {
                assert_eq!(approval_id, "apr_1");
                assert_eq!(decision.kind, "approve");
                assert_eq!(decision.reason, None);
            }
            other => panic!("expected Decide, got {other:?}"),
        }
        assert!(app.modal().is_none());
    }

    #[test]
    fn only_the_offered_decisions_can_be_selected() {
        // The modal renders exactly available_decisions — moving the cursor
        // never lands on anything the server didn't offer.
        let mut app = App::new();
        app.set_approvals(vec![approval("apr_1", &["approve", "abort"])]);
        app.focus = Focus::Approvals;
        app.on_key(Key::Enter);
        let m = app.modal().unwrap();
        assert_eq!(m.approval.available_decisions, vec!["approve", "abort"]);
        // down past the end wraps within the two offered options only
        app.on_key(Key::Down); // -> abort
        app.on_key(Key::Down); // wraps -> approve
        assert_eq!(app.modal().unwrap().cursor, 0);
    }

    #[test]
    fn deny_requires_a_reason_then_emits_it() {
        let mut app = App::new();
        open_modal(&mut app);
        app.on_key(Key::Down); // approve_for_session
        app.on_key(Key::Down); // deny
        // Enter on deny does NOT emit yet — it opens reason entry
        assert_eq!(app.on_key(Key::Enter), Action::None);
        assert!(app.modal().unwrap().reason.is_some());
        // empty reason submit is a no-op (server would 400 anyway)
        assert_eq!(app.on_key(Key::Enter), Action::None);
        app.on_key(Key::Char('n'));
        app.on_key(Key::Char('o'));
        let action = app.on_key(Key::Enter);
        match action {
            Action::Decide { decision, .. } => {
                assert_eq!(decision.kind, "deny");
                assert_eq!(decision.reason.as_deref(), Some("no"));
            }
            other => panic!("expected deny Decide, got {other:?}"),
        }
    }

    // ── HARD CONTRACT 2: Esc emits no decision ───────────────────────────────

    #[test]
    fn esc_cancels_the_modal_without_any_decision() {
        let mut app = App::new();
        open_modal(&mut app);
        let action = app.on_key(Key::Esc);
        assert_eq!(action, Action::None, "Esc must NOT emit a decision");
        assert!(app.modal().is_none());
        // the approval is still pending (never resolved by the UI)
        assert_eq!(app.approvals().len(), 1);
        assert_eq!(app.approvals()[0].status, ApprovalStatus::Pending);
    }

    #[test]
    fn esc_from_reason_entry_returns_to_the_list_not_a_decision() {
        let mut app = App::new();
        open_modal(&mut app);
        app.on_key(Key::Down);
        app.on_key(Key::Down); // deny
        app.on_key(Key::Enter); // -> reason entry
        assert!(app.modal().unwrap().reason.is_some());
        let action = app.on_key(Key::Esc);
        assert_eq!(action, Action::None);
        // back on the decision list, modal still open, no decision emitted
        assert!(app.modal().unwrap().reason.is_none());
        assert_eq!(app.approvals().len(), 1);
    }

    // ── event application + reconcile ────────────────────────────────────────

    #[test]
    fn approval_required_event_enqueues_and_resolved_dequeues() {
        let mut app = App::new();
        app.set_runs(vec![run("run_1", RunStatus::Running)]);
        let ev = Event {
            v: 1,
            seq: 1,
            ts: "t".to_owned(),
            body: EventBody::ApprovalRequired(Box::new(approval("apr_1", &["approve", "abort"]))),
        };
        app.apply_event(&ev);
        assert_eq!(app.approvals().len(), 1);
        assert_eq!(
            app.selected_run().unwrap().status,
            RunStatus::AwaitingApproval
        );

        let resolved = Event {
            v: 1,
            seq: 2,
            ts: "t".to_owned(),
            body: EventBody::ApprovalResolved(agent24_protocol::ApprovalResolvedPayload {
                approval_id: "apr_1".to_owned(),
                run_id: "run_1".to_owned(),
                decision_type: "approve".to_owned(),
            }),
        };
        app.apply_event(&resolved);
        assert!(app.approvals().is_empty());
    }

    #[test]
    fn a_seq_gap_flags_reconcile() {
        let mut app = App::new();
        let mk = |seq| Event {
            v: 1,
            seq,
            ts: "t".to_owned(),
            body: EventBody::RunCancelled(agent24_protocol::RunCancelledPayload {
                run_id: "run_1".to_owned(),
            }),
        };
        app.apply_event(&mk(1));
        assert!(!app.needs_reconcile);
        app.apply_event(&mk(2)); // contiguous
        assert!(!app.needs_reconcile);
        app.apply_event(&mk(9)); // gap!
        assert!(app.needs_reconcile);
    }

    #[test]
    fn cancel_key_on_a_live_run_requests_cancel_terminal_run_does_not() {
        let mut app = App::new();
        app.set_runs(vec![run("run_live", RunStatus::Running)]);
        app.focus = Focus::Runs;
        match app.on_key(Key::Cancel) {
            Action::CancelRun { run_id } => assert_eq!(run_id, "run_live"),
            other => panic!("expected CancelRun, got {other:?}"),
        }
        app.set_runs(vec![run("run_done", RunStatus::Completed)]);
        assert_eq!(app.on_key(Key::Cancel), Action::None);
    }

    #[test]
    fn set_runs_keeps_the_cursor_on_the_same_run() {
        let mut app = App::new();
        app.set_runs(vec![
            run("a", RunStatus::Running),
            run("b", RunStatus::Running),
        ]);
        app.focus = Focus::Runs;
        app.on_key(Key::Down); // cursor -> b
        assert_eq!(app.selected_run().unwrap().id, "b");
        // reconcile reorders (b now first) — cursor should follow b
        app.set_runs(vec![
            run("b", RunStatus::Completed),
            run("a", RunStatus::Running),
        ]);
        assert_eq!(app.selected_run().unwrap().id, "b");
    }

    #[test]
    fn reconcile_closes_a_modal_whose_approval_vanished() {
        let mut app = App::new();
        open_modal(&mut app);
        // a reconcile that no longer lists apr_1 (resolved elsewhere)
        app.set_approvals(vec![]);
        assert!(app.modal().is_none());
    }

    #[test]
    fn quit_key_sets_should_quit() {
        let mut app = App::new();
        assert_eq!(app.on_key(Key::Quit), Action::Quit);
        assert!(app.should_quit);
    }
}
