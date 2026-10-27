-- The tool calls each job attempt was accepted for (K1-06a-grant-path §2.5):
-- when an operation's authorization ends, the work of the attempts its calls
-- were accepted for stops, whoever first made the job. Written in the claim's
-- transaction, with the job it names; then never changed.
CREATE TABLE job_acceptances (
    job_id            TEXT NOT NULL REFERENCES jobs(id),
    attempt           INTEGER NOT NULL CHECK (attempt >= 1),
    operation         TEXT NOT NULL CHECK (instr(CAST(operation AS BLOB), x'00') = 0
                                           AND length(CAST(operation AS BLOB)) BETWEEN 1 AND 64),
    -- The grant the call was made under, as the kernel named it (`ga1:…`).
    authorization_ref TEXT CHECK (authorization_ref IS NULL
                                  OR (instr(CAST(authorization_ref AS BLOB), x'00') = 0
                                      AND length(CAST(authorization_ref AS BLOB)) BETWEEN 1 AND 256)),
    run_id            TEXT NOT NULL CHECK (length(run_id) > 0),
    tool_call_id      TEXT NOT NULL CHECK (length(tool_call_id) > 0),
    created_at        TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;
CREATE INDEX job_acceptances_by_job ON job_acceptances(job_id, attempt);
CREATE TRIGGER job_acceptances_are_append_only_update BEFORE UPDATE ON job_acceptances
BEGIN SELECT RAISE(ABORT, 'job_acceptances is append-only'); END;
CREATE TRIGGER job_acceptances_are_append_only_delete BEFORE DELETE ON job_acceptances
BEGIN SELECT RAISE(ABORT, 'job_acceptances is append-only'); END;

