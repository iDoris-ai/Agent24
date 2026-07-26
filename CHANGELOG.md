# Changelog

All notable changes to Agent24 are documented here. This project adheres to
[Semantic Versioning](https://semver.org/).

## [0.2.0] — 2026-07-26

**M-H — the human boundary.** Everything a person needs to stay in control of an
agent that runs while they're away: what needs asking, how far a "yes" reaches,
and what an error actually tells them. Studied from Andrew Ng's OpenWorker and
put through an adversarial review before landing (see
`docs/reference-notes/openworker.md`). All protocol changes are additive — a
pre-0.2 client keeps validating.

### Added

- **Declared risk classes** (H1): a tool's `risk_class`
  (`read`/`write_local`/`exec`/`external`) is now the one property the approval
  path reads, and `requires_approval` is derived from it — two hand-maintained
  lists can no longer drift. Additive protocol field; gating outcomes are
  byte-for-byte unchanged. Each class earns a different exemption path, which is
  what the next two features are built on.
- **User-local risk overrides** (H2): a glob rule the machine's owner writes to
  relax (or tighten) an individual tool's class — the release valve that makes
  MCP's conservative `external` default actually usable. A user may correct a
  guess (third-party `external`) but never overrule knowledge (a builtin's
  class). The store is user-local and is never written by a module, persona, or
  MCP server. `GET/PUT/DELETE /api/v1/tool-overrides`.
- **Target-scoped standing grants** (H4): "always allow" for an external tool
  now binds to an **exact target** (`send → #ops`), owned by the session or the
  schedule that fired the run, matched exactly and revoked when its schedule is
  deleted. The broad whole-tool `approve_for_session` is no longer offered for
  external tools — the safe option is the only option. `Approval.standing_target`
  labels the choice; `GET/DELETE /api/v1/standing-grants`.
- **Read-only explorer subagent** (H9): the `explore` tool delegates a bounded,
  read-only investigation to a fresh sub-agent with its own context, so the
  dozens of file reads it takes to answer "where is X handled?" never crowd the
  main transcript. Read-only and no-recursion are structural — the sub-run's
  registry simply never contains a write/exec tool or `explore` itself.

### Changed

- **Provider errors say what to do** (H12): `openai returned HTTP 429` becomes a
  named cause plus the provider's own message — bad key vs wrong model vs spent
  quota. A rate-limited or 5xx primary now falls through to a healthy backup
  provider (previously every HTTP error stopped there); auth/model errors still
  stop, since a different provider can't fix a config mistake.

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
