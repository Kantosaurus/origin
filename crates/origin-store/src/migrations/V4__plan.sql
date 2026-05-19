-- Phase 9 — plan op-log + snapshot persistence (P9.3, N7.7).
-- Backs `origin-plan::PlanStore` (op-log append + snapshot fast-forward).
PRAGMA foreign_keys = ON;

CREATE TABLE plan_ops (
    lamport     INTEGER NOT NULL,
    actor       BLOB NOT NULL,
    op_kind     TEXT NOT NULL,
    body        BLOB NOT NULL,
    PRIMARY KEY (lamport, actor)
);

CREATE INDEX idx_plan_ops_lamport ON plan_ops(lamport);

CREATE TABLE plan_snapshots (
    seq                  INTEGER PRIMARY KEY,
    state_handle         BLOB NOT NULL,         -- 32-byte CAS hash
    fully_acked_below    INTEGER NOT NULL,
    created_at_unix_ms   INTEGER NOT NULL
);

CREATE INDEX idx_plan_snapshots_acked ON plan_snapshots(fully_acked_below);
