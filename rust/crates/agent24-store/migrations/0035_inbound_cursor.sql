-- X11-2e: groupchat history cursor. Hyphae DMs (`history inbox`) have no
-- cursor; group history pages with `--after-id` (the logical message id).
-- Hyphae stores group messages by (local_npub, group_id, logical_id). Two
-- local identities in the same group do not share a cursor: a miss is
-- `ErrCursorNotFound` and would stall that identity. One row per
-- source + recipient identity (npub) + thread (group id).

CREATE TABLE inbound_cursor (
    source             TEXT NOT NULL,
    recipient_identity TEXT NOT NULL,
    thread_key         TEXT NOT NULL,
    cursor             TEXT NOT NULL,
    PRIMARY KEY (source, recipient_identity, thread_key),
    -- An empty string is a different key from NULL and would make
    -- `--after-id ''` the cursor; refuse it at the table.
    CHECK (recipient_identity <> '' AND thread_key <> '' AND cursor <> '')
);
