-- ME4-desktop-model-ui follow-up: AgentEar's per-step turn timings (design
-- ask, AgentEar PR #102 / agent-speaker v0.26.0) correlate several rows —
-- one per `*_ms` field on the SAME turn — back to that one turn. `session_id`
-- + `seq` are the two identifiers AgentEar's own `agentear.event/1` envelope
-- already carries for exactly this purpose; both are opaque ids, never
-- content. Both nullable: every row from `_a24/model/complete`/`/api/v1/chat`
-- leaves them NULL (neither call is turn-scoped).
ALTER TABLE model_call_timings ADD COLUMN session_id TEXT;
ALTER TABLE model_call_timings ADD COLUMN seq INTEGER CHECK (seq IS NULL OR seq >= 0);

-- `GET /api/v1/timings?session_id=` and correlating a turn's own rows.
CREATE INDEX idx_model_call_timings_session ON model_call_timings(session_id, seq);
