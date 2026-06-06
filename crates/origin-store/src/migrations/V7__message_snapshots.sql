-- Gap 2: pre-compaction message snapshots.
--
-- When the agent compacts a turn, it collapses that turn's full body to a short
-- "[compacted turn N] <summary>" placeholder and persists that over the
-- original body_inline. This table preserves the ORIGINAL rkyv-encoded message
-- so a transcript rewind can reconstruct the pre-compaction text of any kept
-- turn (rather than leaving it lossy).
--
-- Write-once per (session, turn): the daemon inserts with INSERT OR IGNORE so
-- the first/original snapshot always wins and re-compaction never clobbers it.
-- Rows are GC'd when the parent session is deleted (ON DELETE CASCADE) or when a
-- kept turn is restored during rewind. No rows exist until the first
-- compaction, so short/never-compacted sessions carry zero overhead.
CREATE TABLE message_snapshots (
    session_id    TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    turn_index    INTEGER NOT NULL,
    original_body BLOB NOT NULL,
    compacted_at  INTEGER NOT NULL,
    PRIMARY KEY (session_id, turn_index)
);
