// v1 protocol contract skeletons — activated milestone by milestone.
// Source of truth: protocol/openapi.yaml + protocol/events.schema.json.
// A5 turns the M-A group live against the node mock daemon; the M-C group
// goes live with the Rust daemon (tasks C1..C5). Keep names in sync with
// docs/specs/SPEC-002-protocol.md §2/§3.

import { describe, it } from 'vitest'

describe('v1 M-A (activate in A5)', () => {
  it.todo('GET /api/v1/health → 200 {status:"ok", version, backend}')
  it.todo('POST /api/v1/chat → 200 {message, usage} and emits run.started/model.delta/run.completed for a transient run')
  it.todo('POST /api/v1/chat without messages → 400 invalid_request error envelope')
  it.todo('GET /api/v1/models → 200 {models:[{id, provider, tier, loaded}]}')
  it.todo('GET /api/v1/usage → 200 Usage aggregate')
  it.todo('WS /api/v1/events → envelope {v:1, seq, ts, type, payload}, seq monotonic')
  it.todo('WS /api/v1/events → messages validate against protocol/events.schema.json')

  describe('agent24d only (B2+)', () => {
    it.todo('requests without bearer token → 401 unauthorized (mock daemon exempt)')
    it.todo('WS upgrade with browser Origin header is rejected')
  })
})

describe('v1 M-C runs (activate in C2)', () => {
  it.todo('POST /api/v1/runs → 202 Run(status=queued), then run.started event')
  it.todo('GET /api/v1/runs/{id} → 200 Run; unknown id → 404')
  it.todo('GET /api/v1/runs?status=running filters correctly')
  it.todo('POST /api/v1/runs/{id}/cancel → 202; streaming run emits run.cancelled within 1s')
  it.todo('cancel is idempotent: cancelling a terminal run returns 202 with unchanged run')
})

describe('v1 M-C approvals (activate in C4)', () => {
  it.todo('approval-required tool emits approval.required (request class) with available_decisions')
  it.todo('GET /api/v1/approvals?status=pending lists the pending approval')
  it.todo('POST /api/v1/approvals/{id} {type:approve} → 200, run resumes')
  it.todo('POST /api/v1/approvals/{id} {type:deny, reason} → 200, model receives reason, run continues')
  it.todo('POST /api/v1/approvals/{id} {type:abort} → run cancelled')
  it.todo('decision type not in available_decisions → 400 invalid_request')
  it.todo('second decision on same approval → 409 approval_already_resolved')
  it.todo('expired approval resolves to timed_out (fail-closed) and emits approval.resolved')
  it.todo('daemon restart marks lingering pending approvals aborted (fail-closed)')
})

describe('v1 M-C schedules (activate in C5)', () => {
  it.todo('POST /api/v1/schedules (cron spec) → 201 with computed next_run_at')
  it.todo('GET /api/v1/schedules lists it; GET /api/v1/schedules/{id} returns it')
  it.todo('POST /api/v1/schedules with every.secs < 60 → 400')
  it.todo('PATCH /api/v1/schedules/{id} spec change recomputes next_run_at')
  it.todo('DELETE /api/v1/schedules/{id} → 204; then GET → 404')
  it.todo('POST /api/v1/schedules/{id}/run_now → 202 {run_id}, next_run_at unchanged')
  it.todo('schedule firing emits schedule.fired {schedule_id, run_id}')
})

describe('v1 M-C sessions & tools (activate in C1/C3)', () => {
  it.todo('POST /api/v1/sessions → 201 Session; GET list contains it; GET /api/v1/sessions/{id} returns it')
  it.todo('GET /api/v1/tools → 200 {tools:[{name, source, description, requires_approval}]}')
})
