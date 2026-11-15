-- VR-2: embeddings are a projection of current beliefs. This supersedes the
-- 0007 note that reindex never drops rows. Closing a fact
-- (`recorded_to` set to a value) deletes every vector of that assertion, every
-- model and every revision, in the same statement as the close.
--
-- A re-assert writes `recorded_to` back to NULL. That update names the column,
-- but the WHEN clause does not match, so the rows stay for the background
-- reindex to fill if they are missing.
--
-- The DELETE clears vectors of facts that were already closed before this
-- trigger existed. Live facts (`recorded_to` IS NULL) are left in place.
-- Switching the embedder back to an older model re-embeds every current fact
-- once the new key is complete and the older rows have been removed; a few
-- thousand rows is an acceptable cost.

CREATE TRIGGER mem_embeddings_au_recorded_to
AFTER UPDATE OF recorded_to ON mem_assertions
WHEN NEW.recorded_to IS NOT NULL
BEGIN
    DELETE FROM mem_embeddings WHERE assertion_id = NEW.id;
END;

DELETE FROM mem_embeddings
WHERE assertion_id IN (
    SELECT id FROM mem_assertions WHERE recorded_to IS NOT NULL
);
