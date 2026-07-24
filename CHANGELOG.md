# Changelog

All notable changes to Agent24 are documented here. This project adheres to
[Semantic Versioning](https://semver.org/).

## [0.1.0] — 2026-07-24

The first release of the **Rust-core Agent24**: a 24/7 personal/community
workflow agent (not a coding agent). The Electron desktop shell now ships the
Rust `agent24d` daemon as its default backend, speaking the frozen v1 protocol.

### Added — Rust core (agent24d + agent24 CLI)

- **Domain state machines + store** (C1): exhaustive Run / Approval / ToolCall
  state transitions; SQLite persistence via sqlx with `BEGIN IMMEDIATE`
  transactions and a hash-chained (sha256) tamper-evident audit log.
- **Agent loop v1** (C2): `POST /api/v1/runs` → background execution with
  first-class cancellation woven through every await point; full WS lifecycle
  events; fail-closed orphan-run sweep on startup.
- **Tool system** (C3): `Tool` trait + registry with a fixed dispatch pipeline
  (capability whitelist → approval gate → timeout). Builtins: `http_fetch`
  (SSRF-guarded, resolve-then-pin against DNS rebinding), `fs_read`/`fs_write`
  (cap-std dirfd-anchored, beneath-only traversal), `shell_exec` (argv
  execution, never a shell string).
- **Approval system** (C4): fail-closed approval broker — every non-answer path
  (timeout, run-cancel, dropped channel) resolves negative; the store row is
  the single arbiter; `approve_for_session` grants scoped to (session, tool);
  runs enter `awaiting_approval` while a decision is pending.
- **Wall-clock scheduler** (C5): cron / every / at schedules with pre-advance
  (crash cannot double-fire), skip-missed (no replay bursts), and fail-safe
  disable after 5 consecutive failures. Timezone/DST-correct cron.
- **`agent24 tui`** (C6): a ratatui operator client — runs · event stream ·
  approval queue — with WS streaming, auto-reconnect, and REST reconciliation.

### Added — Desktop

- **Runs / Schedules / Approvals pages** (C7): live REST-polling views;
  Schedules form with an instant next-fire preview; desktop notifications on
  new approvals rendering the server's `available_decisions`.

### Changed

- The desktop shell defaults to the Rust `agent24d` backend
  (`AGENT24_BACKEND=node` opts back into the legacy mock).

### Protocol

- Contract-first v1 API frozen in `protocol/` (openapi.yaml +
  events.schema.json), enforced by dual-backend contract tests and a CI
  zero-drift gate.
