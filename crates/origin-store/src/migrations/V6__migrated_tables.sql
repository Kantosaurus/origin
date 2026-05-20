-- P14.B.6: dedupe tables for the migration sink (`origin-migrate::sink`).
CREATE TABLE migrated_sessions (
    key  TEXT PRIMARY KEY,
    body TEXT NOT NULL
);

CREATE TABLE migrated_skills (
    key  TEXT PRIMARY KEY,
    body TEXT NOT NULL
);
