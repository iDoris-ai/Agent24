-- The running attempts an authorization's end asked to stop (K1-06a-grant-path
-- §2.5): the cancel they settle into says why, not that the user cancelled them.
CREATE TABLE job_revocations (
    job_id  TEXT NOT NULL REFERENCES jobs(id),
    attempt INTEGER NOT NULL CHECK (attempt >= 1),
    PRIMARY KEY (job_id, attempt)
) STRICT;
