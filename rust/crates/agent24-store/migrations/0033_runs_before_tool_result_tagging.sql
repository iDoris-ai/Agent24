-- K1-R3 release review (David 2026-10-29): which runs a build that tagged no
-- tool result may have driven.
--
-- Before 6b.5f-1 no tool result was tagged. A run such a build started and
-- parked on an approval, resumed after the upgrade, would replay those
-- results as if they were ordinary and pass the egress gate, so the kernel
-- does not resume a run listed here: it ends with `source_unproven`. Every
-- run not yet finished when this migration runs is listed; the build that
-- applies it starts no run before it, so a run it creates is never listed.
-- A wall clock cannot tell the two apart: it may be set back across the
-- upgrade. A development library that already ran a tagging build lists its
-- unfinished runs too, and they end the same way. Only the kernel writes
-- this table.
CREATE TABLE runs_before_tool_result_tagging (
    run_id TEXT PRIMARY KEY NOT NULL
);

INSERT INTO runs_before_tool_result_tagging (run_id)
SELECT id FROM runs WHERE status NOT IN ('completed', 'failed', 'cancelled');
