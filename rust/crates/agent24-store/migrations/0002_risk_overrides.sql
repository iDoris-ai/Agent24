-- H2: user-local risk overrides.
--
-- One row = one glob pattern over TOOL NAMES (e.g. `mcp_fs_*`) mapped to the
-- RiskClass the user wants for the tools it matches. The pattern is the primary
-- key, so setting the same pattern twice replaces rather than accumulates.
--
-- INVIOLABLE: only an explicit user action writes this table. No module,
-- persona, or MCP server install path may touch it — a package that could ship
-- its own exemption would make the conservative default for third-party code
-- worthless. `source` records which surface the user acted from, so an audit
-- can tell them apart; it is NOT an authorization field.
CREATE TABLE risk_overrides (
    pattern    TEXT PRIMARY KEY,
    risk_class TEXT NOT NULL,  -- read | write_local | exec | external
    source     TEXT NOT NULL,  -- open enum: desktop | cli | tui
    created_at TEXT NOT NULL
);
