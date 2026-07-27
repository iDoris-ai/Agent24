-- H3/G1 foundation: the durable message thread of a run.
--
-- Until now a run's conversation lived ONLY in the in-memory `messages` vec
-- inside the agent loop; only the compacted user/assistant *exchange* reached
-- session memory (agent24-memory), and tool calls/results never persisted at
-- all. Durable resume (H3) needs the FULL per-run thread on disk so a restarted
-- daemon can reconstruct where a run was suspended — specifically an assistant
-- turn whose trailing tool_call was never answered because it was awaiting
-- approval when the process died. That "trailing unanswered tool_call" is the
-- cheap reconstruction signal OpenWorker uses, and it is unavailable without a
-- persisted thread.
--
-- Scope note: this table records the PER-RUN thread (this run's user prompt
-- onward). A session's prior compacted context still lives in session memory
-- and is reloaded from there on resume; it is deliberately NOT duplicated here.
CREATE TABLE run_messages (
    run_id       TEXT NOT NULL REFERENCES runs(id),
    seq          INTEGER NOT NULL,           -- per-run monotonic order (0-based)
    role         TEXT NOT NULL,              -- user | assistant | tool | system
    content      TEXT,                       -- nullable: an assistant tool-only turn has no text
    tool_calls   TEXT NOT NULL DEFAULT '[]', -- JSON array of ToolCallRequest; '[]' except assistant turns
    tool_call_id TEXT,                       -- set only on role='tool': which call this answers
    created_at   TEXT NOT NULL,
    PRIMARY KEY (run_id, seq)
);
CREATE INDEX idx_run_messages_run ON run_messages(run_id);
