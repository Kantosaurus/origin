-- Phase 7 — code knowledge graph schema.
PRAGMA foreign_keys = ON;

CREATE TABLE code_nodes (
    entity_id        BLOB PRIMARY KEY,
    kind             TEXT NOT NULL,
    name             TEXT NOT NULL,
    language         INTEGER NOT NULL,
    file_path        TEXT NOT NULL,
    range_start      INTEGER NOT NULL,
    range_end        INTEGER NOT NULL,
    signature_handle BLOB NOT NULL,
    body_handle      BLOB NOT NULL,
    last_seen        INTEGER NOT NULL
);

CREATE INDEX idx_code_nodes_name ON code_nodes(name);
CREATE INDEX idx_code_nodes_signature ON code_nodes(signature_handle);
CREATE INDEX idx_code_nodes_file ON code_nodes(file_path);

CREATE TABLE code_edges (
    from_id          BLOB NOT NULL,
    to_id            BLOB NOT NULL,
    kind             TEXT NOT NULL,
    confidence       TEXT NOT NULL,
    evidence_handle  BLOB NOT NULL,
    PRIMARY KEY (from_id, to_id, kind)
);

CREATE INDEX idx_code_edges_from ON code_edges(from_id);
CREATE INDEX idx_code_edges_to   ON code_edges(to_id);

CREATE TABLE code_communities (
    community_id     INTEGER PRIMARY KEY,
    members_handle   BLOB NOT NULL,
    god_nodes_handle BLOB NOT NULL,
    modularity       REAL NOT NULL,
    built_at         INTEGER NOT NULL
);

CREATE TABLE cross_links (
    code_id          BLOB NOT NULL,
    mem_id           BLOB NOT NULL,
    relation         TEXT NOT NULL,
    PRIMARY KEY (code_id, mem_id, relation)
);

CREATE INDEX idx_cross_links_code ON cross_links(code_id);
CREATE INDEX idx_cross_links_mem  ON cross_links(mem_id);
