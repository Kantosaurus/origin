# origin-store

> SQLite-backed store for origin with embedded refinery migrations

## Purpose

`origin-store` is the durable relational layer for origin. It wraps a single
`SQLite` connection, runs embedded `refinery` schema migrations on open, and
exposes a small typed API over the tables used by the migration sink (migrated
sessions, skills, and memories). It is intentionally minimal: callers reach
through `with_conn` for anything not covered by the convenience methods.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `Store` | struct | Owns a `Mutex<Connection>`; opens + migrates a SQLite DB. |
| `StoreError` | enum | `Sqlite(rusqlite::Error)` / `Migration(refinery::Error)`. |
| `Store::open` | fn | Open/create a DB, set WAL + `synchronous=NORMAL`, run pending migrations. |
| `Store::with_conn` | fn | Run a closure against the connection under the mutex. |
| `Store::contains_migrated_session` / `insert_migrated_session` | fn | Dedup-keyed migrated session rows. |
| `Store::contains_migrated_skill` / `insert_migrated_skill` | fn | Dedup-keyed migrated skill rows. |
| `Store::contains_migrated_memory` / `insert_migrated_memory` / `count_migrated_memories` | fn | Dedup-keyed migrated memory rows + a count helper. |
| `Store::wal_checkpoint_truncate` | fn | `PRAGMA wal_checkpoint(TRUNCATE)` to fold the WAL back. |

## Key types

```rust
#[derive(Debug, Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("migration: {0}")]
    Migration(#[from] refinery::Error),
}

pub struct Store {
    conn: Mutex<Connection>,
}

embed_migrations!("src/migrations"); // refinery, compiled in at build time
```

```rust
impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let mut conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;\
             PRAGMA synchronous  = NORMAL;",
        )?;
        migrations::runner().run(&mut conn)?;
        Ok(Self { conn: Mutex::new(conn) })
    }
}
```

## How it works

Schema lives as numbered SQL files under `src/migrations/` (e.g.
`V8__migrated_memories.sql`); `refinery::embed_migrations!` compiles them into the
binary so there is no runtime migrations directory to ship. On `open`, the store
first sets WAL mode and `synchronous = NORMAL` **outside** any transaction —
refinery wraps each migration in its own transaction, so the pragmas must be
applied before the runner — then runs every pending migration in order.

All access funnels through `with_conn`, which serializes on the connection mutex
(SQLite connections are not `Sync` for concurrent use). The `contains_*` /
`insert_*` helpers implement the idempotent "have I already migrated this content
key?" pattern the migration sink relies on, and `wal_checkpoint_truncate` lets a
quiesced store fold its WAL back into the main file.

```text
open(path) ─► PRAGMA WAL + synchronous=NORMAL ─► refinery runner (V1..Vn)
            └─► Store { Mutex<Connection> } ──with_conn──► caller closures
```

## Dependencies & features

- `rusqlite` (bundled, `blob`) — the embedded SQLite engine.
- `refinery` (`rusqlite`) — embedded, ordered migrations.
- `tracing`, `thiserror`.
- Dev: `tempfile`.

No cargo features are defined.

## Used by

Per `Grep "origin-store" crates/*/Cargo.toml`: `origin-cli`, `origin-codegraph`,
`origin-daemon`, `origin-mem`, `origin-migrate`, `origin-plan`, `origin-swarm`,
`origin-tools`.

## Testing

`crates/origin-store/tests/migrate.rs` exercises open + migration behaviour.
Migration SQL itself lives under `crates/origin-store/src/migrations/`.

## See also

- [../architecture/data-and-storage.md](../architecture/data-and-storage.md) — relational schema and migrations.
- [../subsystems/memory-and-codegraph.md](../subsystems/memory-and-codegraph.md) — consumers of the migrated tables.
- [origin-cas.md](origin-cas.md) — the blob counterpart to this relational store.
- Back to [../crates/README.md](../crates/README.md).

_Last reviewed against workspace version 0.9.8._
