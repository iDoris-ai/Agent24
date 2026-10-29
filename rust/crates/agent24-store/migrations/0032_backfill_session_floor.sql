-- K1-6b.5f-3 (ADR-K1-02 §2.2), release review, David 2026-10-27 (B): a run
-- with a session that carries no `session_floor:` tag cannot say whether it
-- ran with session memory. Since 6b.4c a run with a session and session
-- memory is tagged before its first model call; a run on a daemon without
-- session memory is not, and the peer view (agent24d peer_runs.rs) lets it
-- through only while the daemon still has none. A development library that
-- ran a build between those two rules holds session runs whose earlier turns
-- did reach the model untagged. Which ones cannot be told now, so every such
-- run present at upgrade gets an `unknown` session floor. Runs created after
-- this migration keep the rule as it is.
--
-- `tag_json` is `SourceRef::session_floor(session_id, Unknown, now)` as
-- serde writes it, at `SOURCE_TAG_SCHEMA_VERSION` 2. No reader consumes
-- `seq` for this tag, and `source_id` keeps the row off any existing key.
INSERT INTO run_source_tags (run_id, seq, source_id, schema_version, tag_json, created_at)
SELECT runs.id, 0, 'session_floor:' || runs.session_id, 2,
       json_object(
           'source_id', 'session_floor:' || runs.session_id,
           'kind', 'session_context',
           'classification', 'unknown',
           'revision_digest', NULL,
           'mode', 'local_only',
           'policy_version', 0,
           'authorization_ref', NULL,
           'created_at', strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
       ),
       strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
FROM runs
WHERE runs.session_id IS NOT NULL
  AND NOT EXISTS (
      SELECT 1 FROM run_source_tags AS tags
      WHERE tags.run_id = runs.id
        AND substr(tags.source_id, 1, 14) = 'session_floor:'
  );
