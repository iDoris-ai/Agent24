-- K1-6b.5f-2 (ADR-K1-02 §2.1.1 item 6, §2.2): where a session's tool
-- results came from.
--
-- A tool result enters the model context of its run and, through session
-- context, of every later turn of the session. Each origin (`module:{id}`,
-- `tool:{name}`) of a non-ordinary result is recorded here when the result is
-- tagged, in the same transaction, and later turns tag themselves with one
-- source per origin. The origin is kept rather than folded into
-- `session_source_floor`, so that a module can still be sent back what it
-- returned itself in a later turn while every other origin keeps restricting
-- egress. Only the kernel writes this table.
--
-- `session_id` is the memory session id, as in `session_source_floor`, so
-- there is no foreign key.
CREATE TABLE session_tool_result_origins (
    session_id TEXT NOT NULL,
    origin     TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (session_id, origin)
);

-- Codex 6b.5f-2 r1 C1: a session whose runs called tools before this table
-- existed took in results nobody tagged — before 6b.5f-1 no tool result was.
-- Which of them were ordinary cannot be told now, and a tool's name does not
-- prove a module's identity (an MCP tool may contain a dot), so such a
-- session gets one `tool:legacy` origin: later turns carry it as `unknown`,
-- without the same-source exception.
INSERT INTO session_tool_result_origins (session_id, origin, created_at)
SELECT DISTINCT runs.session_id, 'tool:legacy', strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
FROM tool_calls JOIN runs ON runs.id = tool_calls.run_id
WHERE runs.session_id IS NOT NULL;
