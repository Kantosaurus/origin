-- N6.3: memory body in CAS, vector inline, tags as bitset over a tag dictionary.
PRAGMA foreign_keys = ON;

CREATE TABLE memories (
    id              TEXT PRIMARY KEY,           -- ULID
    centroid_id     INTEGER NOT NULL,           -- 0..255
    deltas          BLOB    NOT NULL,           -- 384 i8 values, length=384
    body_handle     BLOB    NOT NULL,           -- 32-byte CAS hash
    body_preview    TEXT    NOT NULL,           -- ≤64 bytes utf-8
    tags_bitset     BLOB    NOT NULL DEFAULT (X'00000000000000000000000000000000'),  -- 128-bit
    created_at      INTEGER NOT NULL,           -- epoch ms
    last_seen_at    INTEGER NOT NULL,
    superseded_by   TEXT    REFERENCES memories(id) ON DELETE SET NULL,
    cluster_priority REAL   NOT NULL DEFAULT 1.0
);

CREATE INDEX idx_memories_last_seen ON memories(last_seen_at);
CREATE INDEX idx_memories_superseded ON memories(superseded_by);

CREATE TABLE mem_edges (
    from_id    TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
    to_id      TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
    kind       INTEGER NOT NULL,  -- 0=RelatedTo, 1=Supersedes, 2=Contradicts
    weight     REAL    NOT NULL DEFAULT 1.0,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (from_id, to_id, kind)
);

CREATE INDEX idx_mem_edges_to ON mem_edges(to_id);

CREATE TABLE mem_tags (
    bit_idx INTEGER PRIMARY KEY,  -- 0..127
    name    TEXT NOT NULL UNIQUE
);

CREATE TABLE mem_quantizer (
    id    INTEGER PRIMARY KEY CHECK (id = 1),  -- singleton row
    bytes BLOB    NOT NULL
);
