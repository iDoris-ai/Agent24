-- Conversation identity belongs to the authenticated module, across reconnects.
CREATE TABLE assistant_sessions (
    module_name TEXT NOT NULL,
    conversation_id TEXT NOT NULL,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    PRIMARY KEY (module_name, conversation_id)
);
