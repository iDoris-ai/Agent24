-- G4 dormant persistence foundation: bindings remain nullable and unused.
ALTER TABLE sessions ADD COLUMN workspace_id TEXT REFERENCES workspaces(id);
ALTER TABLE runs ADD COLUMN workspace_id TEXT REFERENCES workspaces(id);
ALTER TABLE approvals ADD COLUMN workspace_id TEXT REFERENCES workspaces(id);
ALTER TABLE tool_calls ADD COLUMN workspace_id TEXT REFERENCES workspaces(id);
ALTER TABLE standing_grants ADD COLUMN workspace_id TEXT REFERENCES workspaces(id);

CREATE INDEX idx_runs_workspace ON runs(workspace_id);
CREATE INDEX idx_approvals_workspace ON approvals(workspace_id);
CREATE INDEX idx_tool_calls_workspace ON tool_calls(workspace_id);
CREATE INDEX idx_standing_grants_workspace ON standing_grants(workspace_id);
