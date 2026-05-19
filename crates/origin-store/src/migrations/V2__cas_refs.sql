-- CAS refcount table: which content-addressed shards are still reachable
-- from session messages or other live references.
PRAGMA foreign_keys = ON;

CREATE TABLE cas_refs (
    hash        BLOB PRIMARY KEY,    -- 32-byte blake3 hash
    refcount    INTEGER NOT NULL DEFAULT 0,
    tier        INTEGER NOT NULL DEFAULT 0, -- 0=hot, 1=warm, 2=cold
    last_access INTEGER NOT NULL    -- epoch ms
);

CREATE INDEX idx_cas_refs_zero ON cas_refs(refcount) WHERE refcount = 0;
