-- R33: give the migration sink (`origin-migrate::sink`) a real destination for
-- imported memories so `apply_with_store` persists them instead of silently
-- dropping `bundle.memories` while `summarize()` still counts them.
CREATE TABLE migrated_memories (
    key  TEXT PRIMARY KEY,
    body TEXT NOT NULL
);
