-- ME4-desktop-model-ui (latency instrumentation).
--
-- Numbering note (review H1): this was originally shipped as two files,
-- 0009_model_call_timings.sql (the table) and
-- 0010_model_call_timings_session.sql (an ALTER TABLE adding session_id/seq).
-- Both numbers turned out to already be claimed by an in-flight
-- workspace/legacy-recovery branch (integration/open-design-main-sync-wave20,
-- #497, #496) whose sqlx migration checksums this branch cannot see —
-- landing 0009/0010 here risks a checksum mismatch that stops the daemon
-- from starting the moment both branches merge. Merged into ONE file at
-- 0013 (past every number any branch known at review time uses) instead of
-- guessing at a still-free single slot.
--
-- A per-call timing LEDGER — unlike module_model_usage (0008)'s per-day
-- AGGREGATE, this is one row per individual completed model call, kept ONLY
-- as a debugging aid for "which step is slow", never a source of truth for
-- usage/billing (that stays module_model_usage). Written from
-- `_a24/model/complete` (agent24d/src/model_callback.rs), Agent24's own
-- `/api/v1/chat` (agent24d/src/routes.rs), and AgentEar's own per-turn
-- timings breakdown (agent24d/src/agentear_timings.rs) — all three funnel
-- through the SAME background writer, agent24d/src/timing_recorder.rs.
--
-- NEVER stores prompt/response/transcript content — every column here is a
-- number, a short closed-set label, or an id string a provider/module
-- reported; there is no column a user's or a model's actual text could end
-- up in.
--
-- `session_id` + `seq` are AgentEar's own `agentear.event/1` envelope's two
-- correlation identifiers (opaque ids, never content) — they let several
-- rows from the SAME turn (one per `*_ms` field) be grouped back together.
-- Both nullable: every row from `_a24/model/complete`/`/api/v1/chat` leaves
-- them NULL (neither call is turn-scoped).
--
-- Retention (timing_recorder.rs's `prune`): pruned to the tighter of "the
-- last 30 days" and "the newest 100,000 rows", checked once at startup and
-- periodically thereafter — an unbounded per-call ledger would otherwise
-- grow forever, unlike the aggregate table above.
CREATE TABLE model_call_timings (
    id                 INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    ts                 TEXT    NOT NULL, -- ISO 8601 UTC (agent24_core::util::now_iso8601)
    source             TEXT    NOT NULL, -- 'chat' | 'module:<name>'
    model_id           TEXT,             -- nullable: the provider didn't report one
    tier               TEXT    CHECK (tier IS NULL OR tier IN ('local', 'remote')),
    served_by          TEXT,             -- provider name (e.g. 'omlx'); nullable
    ok                 INTEGER NOT NULL CHECK (ok IN (0, 1)),
    error_kind         TEXT,             -- nullable; only meaningful when ok = 0
    -- A module's own finer-grained sub-step (e.g. AgentEar's asr/tts), when
    -- it reports one — NULL for `_a24/model/complete`/`/api/v1/chat` rows.
    step               TEXT,
    session_id         TEXT,
    seq                INTEGER CHECK (seq IS NULL OR seq >= 0),
    first_token_ms     INTEGER CHECK (first_token_ms IS NULL OR first_token_ms >= 0),
    total_ms           INTEGER NOT NULL CHECK (total_ms >= 0),
    prompt_tokens      INTEGER CHECK (prompt_tokens IS NULL OR prompt_tokens >= 0),
    completion_tokens  INTEGER CHECK (completion_tokens IS NULL OR completion_tokens >= 0)
);

-- `GET /api/v1/timings?since=` and the retention prune both filter/order by ts.
CREATE INDEX idx_model_call_timings_ts ON model_call_timings(ts);
-- `GET /api/v1/timings/summary`'s GROUP BY source+model_id.
CREATE INDEX idx_model_call_timings_source_model ON model_call_timings(source, model_id);
-- `GET /api/v1/timings?session_id=` and correlating a turn's own rows.
CREATE INDEX idx_model_call_timings_session ON model_call_timings(session_id, seq);
