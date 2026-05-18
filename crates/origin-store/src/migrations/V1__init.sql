-- Initial schema for origin sessions and messages.
-- NOTE: journal_mode and synchronous are set on the connection before
--       migrations run, because WAL mode cannot be changed inside a
--       transaction (which refinery uses to wrap each migration).
PRAGMA foreign_keys = ON;

CREATE TABLE sessions (
    id         TEXT PRIMARY KEY,
    created_at INTEGER NOT NULL,
    title      TEXT,
    provider   TEXT NOT NULL,
    model      TEXT NOT NULL
);

CREATE TABLE messages (
    session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    turn_index  INTEGER NOT NULL,
    role        INTEGER NOT NULL,
    body_inline BLOB,
    handle_root BLOB,
    summary     TEXT,
    created_at  INTEGER NOT NULL,
    PRIMARY KEY (session_id, turn_index)
);

CREATE INDEX idx_messages_session ON messages(session_id);
