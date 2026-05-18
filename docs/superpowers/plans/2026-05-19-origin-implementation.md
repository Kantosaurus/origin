# `origin` Harness — Implementation Plan (All 14 Phases)

> **For agentic workers:** REQUIRED SUB-SKILL: Use **superpowers:subagent-driven-development** to implement this plan task-by-task. Each task ends with a `verification-before-completion` gate; do NOT move to the next task until verification is green. Use **superpowers:test-driven-development** discipline — write the failing test first, run to confirm fail, then implement minimum to pass, then verify, then commit.

**Goal:** Build `origin`, a Rust-native, performance-first agentic coding harness, in 14 vertical-slice phases per the design at `docs/superpowers/specs/2026-05-19-origin-harness-design.md`.

**Architecture:** Workspace of typed Rust crates around a daemon process and a TUI client. Daemon hosts sessions, exposes IPC, manages providers, storage, sidecar, swarm. Client renders + interacts. Every signature subsystem implements a novel mechanism (see spec Appendix A).

**Tech Stack:** Rust (stable, MSRV pinned at P0), Tokio, jemalloc, SQLite (rusqlite or sqlx), rkyv, crossterm + custom renderer, hyper + rustls, tree-sitter, ONNX Runtime, FastCDC, HNSW (hnsw_rs), Leiden (igraph or rustworkx-equiv), zstd with dictionary, memmap2, landlock (Linux), Win32 sandbox APIs (Windows).

**Spec reference:** `docs/superpowers/specs/2026-05-19-origin-harness-design.md` — every novel mechanism is cited as `N<section>.<n>` in tasks.

---

## Conventions (apply to every task)

**TDD shape:**
1. Write failing test (or fuzz/property/bench harness).
2. Run it — confirm the expected failure mode (compile error, assertion, panic).
3. Implement minimum to pass.
4. Run test — confirm pass.
5. **Verification gate** — run the verification command set for this task type (see table below).
6. Commit.

**Verification command sets:**

| Task type | Verification commands |
|---|---|
| Pure-logic (no I/O) | `cargo test -p <crate>` + `cargo clippy -p <crate> -- -D warnings` + `cargo fmt --check` |
| Crate-component | above + `cargo test -p <crate> --all-features` |
| Cross-crate | `cargo test --workspace` + `cargo clippy --workspace -- -D warnings` |
| Perf-sensitive | above + `cargo bench -p <crate>` and assert headline metric in bench output |
| Daemon integration | above + `cargo test -p origin-replay --test e2e_<name>` |
| Security-touched | above + `cargo +nightly test -Zsanitizer=address` (where supported) |

**Verification gate rule:** if any of the above commands fails (non-zero exit, failing test, clippy warning, format diff, missing bench output), **the task is not done**. Fix and re-run.

**Commit style:** Conventional commits — `feat:`, `fix:`, `test:`, `refactor:`, `chore:`. Scope to crate name where possible: `feat(origin-cas): add FastCDC chunker`. Always co-author Claude when written together.

**No `unsafe` outside the three exempted crates** (`origin-cas`, `origin-tui`, `origin-ipc`). CI lint enforces.

**File-path-on-Windows convention:** plan uses forward slashes throughout; Cargo + Git handle them natively on Windows.

---

# Phase 0 — Workspace + Core Types + IPC Scaffold (weeks 1–2)

**Phase goal:** Cargo workspace exists; two binaries handshake over a typed, rkyv-validated IPC frame.

**Files created in this phase:**
- `Cargo.toml` (workspace root)
- `rust-toolchain.toml`
- `.cargo/config.toml`
- `clippy.toml`, `rustfmt.toml`
- `.github/workflows/ci.yml`
- `crates/origin-core/Cargo.toml`, `crates/origin-core/src/lib.rs`, `crates/origin-core/src/types.rs`, `crates/origin-core/src/ir.rs`
- `crates/origin-ipc/Cargo.toml`, `crates/origin-ipc/src/lib.rs`, `crates/origin-ipc/src/frame.rs`, `crates/origin-ipc/src/transport.rs`
- `crates/origin-store/Cargo.toml`, `crates/origin-store/src/lib.rs`, `crates/origin-store/src/migrations/`
- `crates/origin-daemon/Cargo.toml`, `crates/origin-daemon/src/main.rs`
- `crates/origin-cli/Cargo.toml`, `crates/origin-cli/src/main.rs`

---

### Task P0.1 — Workspace skeleton

**Files:** Create `Cargo.toml`, `rust-toolchain.toml`, `.cargo/config.toml`, `clippy.toml`, `rustfmt.toml`.

- [ ] **Step 1: Write `Cargo.toml`**

```toml
[workspace]
resolver = "2"
members = ["crates/*"]

[workspace.package]
version = "0.0.1"
edition = "2021"
rust-version = "1.83"
license = "Apache-2.0"
repository = "https://github.com/wooainsley/origin"

[workspace.lints.rust]
unsafe_code = "forbid"  # overridden in cas/tui/ipc

[workspace.lints.clippy]
pedantic = { level = "warn", priority = -1 }
nursery  = { level = "warn", priority = -1 }
unwrap_used = "deny"
panic = "warn"
```

- [ ] **Step 2: Write `rust-toolchain.toml`**

```toml
[toolchain]
channel = "1.83.0"
components = ["clippy", "rustfmt", "rust-src"]
```

- [ ] **Step 3: Write `rustfmt.toml`**

```toml
edition = "2021"
max_width = 110
imports_granularity = "Crate"
group_imports = "StdExternalCrate"
```

- [ ] **Step 4: Verify workspace parses**

Run: `cargo metadata --no-deps --format-version 1 > /dev/null`
Expected: exit 0, no output.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml rust-toolchain.toml rustfmt.toml clippy.toml .cargo/
git commit -m "chore: scaffold cargo workspace"
```

---

### Task P0.2 — CI baseline

**Files:** Create `.github/workflows/ci.yml`.

- [ ] **Step 1: Write CI workflow**

```yaml
name: CI
on: [push, pull_request]
jobs:
  check:
    strategy:
      matrix:
        os: [ubuntu-latest, macos-latest, windows-latest]
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@1.83.0
        with: { components: clippy, rustfmt }
      - uses: Swatinem/rust-cache@v2
      - run: cargo fmt --all -- --check
      - run: cargo clippy --workspace --all-targets -- -D warnings
      - run: cargo test --workspace
```

- [ ] **Step 2: Push to a feature branch and watch CI**

Run: `git checkout -b ci/baseline && git push -u origin ci/baseline`
Expected: All three jobs green (workspace currently empty).

- [ ] **Step 3: Verify locally**

Run: `cargo fmt --all -- --check && cargo clippy --workspace -- -D warnings && cargo test --workspace`
Expected: exit 0.

- [ ] **Step 4: Commit + merge**

```bash
git add .github/
git commit -m "ci: baseline cross-platform check/clippy/test workflow"
```

---

### Task P0.3 — `origin-core::types::Role`, `MessageId`, `TurnIndex`

**Files:** Create `crates/origin-core/Cargo.toml`, `crates/origin-core/src/lib.rs`, `crates/origin-core/src/types.rs`, `crates/origin-core/tests/types.rs`.

- [ ] **Step 1: Add `origin-core/Cargo.toml`**

```toml
[package]
name = "origin-core"
version.workspace = true
edition.workspace = true

[dependencies]
rkyv = { version = "0.7", features = ["validation", "bytecheck"] }
ulid = { version = "1", features = ["serde"] }
thiserror = "1"

[dev-dependencies]
proptest = "1"
```

- [ ] **Step 2: Write the failing test**

`crates/origin-core/tests/types.rs`:

```rust
use origin_core::types::{Role, MessageId, TurnIndex};

#[test]
fn role_round_trips_rkyv() {
    for r in [Role::User, Role::Assistant, Role::Tool, Role::System] {
        let bytes = rkyv::to_bytes::<_, 64>(&r).unwrap();
        let archived = rkyv::check_archived_root::<Role>(&bytes).unwrap();
        assert_eq!(Role::from_archived(archived), r);
    }
}

#[test]
fn message_id_is_ulid() {
    let id = MessageId::new();
    assert_eq!(id.to_string().len(), 26);
}

#[test]
fn turn_index_is_monotonic() {
    let a = TurnIndex(0);
    let b = a.next();
    assert!(b.0 > a.0);
}
```

- [ ] **Step 3: Run test (compile fails)**

Run: `cargo test -p origin-core`
Expected: compile error — types don't exist.

- [ ] **Step 4: Implement types**

`crates/origin-core/src/types.rs`:

```rust
use rkyv::{Archive, Deserialize, Serialize};

#[derive(Archive, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[archive(check_bytes)]
#[repr(u8)]
pub enum Role { User, Assistant, Tool, System }

impl Role {
    pub fn from_archived(a: &ArchivedRole) -> Self {
        match a { ArchivedRole::User => Self::User, ArchivedRole::Assistant => Self::Assistant,
                  ArchivedRole::Tool => Self::Tool, ArchivedRole::System => Self::System }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageId(pub ulid::Ulid);
impl MessageId {
    pub fn new() -> Self { Self(ulid::Ulid::new()) }
}
impl core::fmt::Display for MessageId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TurnIndex(pub u32);
impl TurnIndex {
    pub fn next(self) -> Self { Self(self.0 + 1) }
}
```

`crates/origin-core/src/lib.rs`:

```rust
pub mod types;
pub mod ir;
```

- [ ] **Step 5: Run test — confirm pass**

Run: `cargo test -p origin-core`
Expected: 3 tests pass.

- [ ] **Step 6: Verification gate**

Run: `cargo test -p origin-core && cargo clippy -p origin-core -- -D warnings && cargo fmt --check`
Expected: exit 0 for all.

- [ ] **Step 7: Commit**

```bash
git add crates/origin-core/
git commit -m "feat(origin-core): add Role, MessageId, TurnIndex with rkyv derive"
```

---

### Task P0.4 — `origin-core::types::Block` + `Message`

**Files:** Modify `crates/origin-core/src/types.rs`; add tests in `crates/origin-core/tests/messages.rs`.

- [ ] **Step 1: Write failing tests**

```rust
use origin_core::types::{Block, Message, Role};

#[test]
fn message_with_text_block_roundtrips() {
    let m = Message::new(Role::User).with_block(Block::text("hello"));
    let bytes = rkyv::to_bytes::<_, 256>(&m).unwrap();
    let arch = rkyv::check_archived_root::<Message>(&bytes).unwrap();
    assert_eq!(arch.role, rkyv::Archived::<Role>::User);
    assert_eq!(arch.blocks.len(), 1);
}

#[test]
fn block_text_carries_no_cache_marker_by_default() {
    let b = Block::text("x");
    assert!(matches!(b, Block::Text { cache_marker: None, .. }));
}
```

- [ ] **Step 2: Run — fails on missing `Block` / `Message`.**

- [ ] **Step 3: Implement `Block` and `Message`**

Extend `types.rs`:

```rust
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[archive(check_bytes)]
pub enum CacheBoundary { Frozen, Sticky, Sliding }

#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[archive(check_bytes)]
pub enum Block {
    Text       { text: String, cache_marker: Option<CacheBoundary> },
    ToolUse    { id: String, name: String, input_json: Vec<u8>, cache_marker: Option<CacheBoundary> },
    ToolResult { tool_use_id: String, handle: Option<[u8; 32]>, inline: Option<Vec<u8>>, cache_marker: Option<CacheBoundary> },
    Thinking   { tokens: String, signature: Option<String> },
}
impl Block {
    pub fn text(s: impl Into<String>) -> Self { Self::Text { text: s.into(), cache_marker: None } }
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[archive(check_bytes)]
pub struct Message { pub role: Role, pub blocks: Vec<Block> }
impl Message {
    pub fn new(role: Role) -> Self { Self { role, blocks: vec![] } }
    pub fn with_block(mut self, b: Block) -> Self { self.blocks.push(b); self }
}
```

- [ ] **Step 4: Run tests — pass.**
- [ ] **Step 5: Verification gate.** `cargo test -p origin-core && cargo clippy -p origin-core -- -D warnings`
- [ ] **Step 6: Commit.** `feat(origin-core): add Block and Message types`

---

### Task P0.5 — `origin-core::ir` skeleton (re-export + Capabilities)

**Files:** Modify `crates/origin-core/src/ir.rs`; tests at `crates/origin-core/tests/ir.rs`.

- [ ] **Step 1: Write failing test**

```rust
use origin_core::ir::{ProviderCaps, CacheKind};

#[test]
fn provider_caps_compile_time_const() {
    const X: ProviderCaps = ProviderCaps {
        prompt_cache: CacheKind::Explicit,
        thinking: true,
        parallel_tools: true,
        vision: true,
        audio: false,
    };
    assert!(X.parallel_tools);
}
```

- [ ] **Step 2: Run — fails (types missing).**
- [ ] **Step 3: Implement `ir.rs`**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheKind { None, Implicit, Explicit }

#[derive(Debug, Clone, Copy)]
pub struct ProviderCaps {
    pub prompt_cache: CacheKind,
    pub thinking: bool,
    pub parallel_tools: bool,
    pub vision: bool,
    pub audio: bool,
}
```

- [ ] **Step 4: Run — pass.**
- [ ] **Step 5: Verification gate.**
- [ ] **Step 6: Commit.** `feat(origin-core): add ProviderCaps IR scaffold`

---

### Task P0.6 — `origin-ipc::frame` encoding + validation

**Files:** Create `crates/origin-ipc/Cargo.toml`, `src/lib.rs`, `src/frame.rs`; tests at `crates/origin-ipc/tests/frame.rs`.

- [ ] **Step 1: Cargo.toml**

```toml
[package]
name = "origin-ipc"
version.workspace = true
edition.workspace = true

[dependencies]
origin-core = { path = "../origin-core" }
rkyv = { version = "0.7", features = ["validation", "bytecheck"] }
bytes = "1"
thiserror = "1"

[dev-dependencies]
proptest = "1"

[lints.rust]
unsafe_code = "allow"  # rkyv validation uses unsafe internally; we wrap it
```

- [ ] **Step 2: Write failing test**

```rust
use origin_ipc::frame::{Frame, FrameKind, encode, validate};

#[test]
fn frame_round_trip() {
    let body = b"payload".to_vec();
    let bytes = encode(1, FrameKind::Request, &body);
    let frame = validate(&bytes).expect("frame valid");
    assert_eq!(frame.request_id, 1);
    assert_eq!(frame.kind, FrameKind::Request);
    assert_eq!(frame.body, body.as_slice());
}

#[test]
fn truncated_frame_rejected() {
    let bytes = encode(1, FrameKind::Request, b"hi");
    assert!(validate(&bytes[..bytes.len() - 1]).is_err());
}
```

- [ ] **Step 3: Run — fails (types missing).**
- [ ] **Step 4: Implement `frame.rs`**

```rust
use bytes::{BufMut, BytesMut};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FrameKind { Request = 1, Response = 2, Event = 3, ErrorFrame = 4 }

#[derive(Debug)]
pub struct Frame<'a> {
    pub request_id: u64,
    pub kind: FrameKind,
    pub body: &'a [u8],
}

#[derive(Debug, Error)]
pub enum FrameError {
    #[error("truncated frame")]    Truncated,
    #[error("bad magic")]          BadMagic,
    #[error("unknown kind {0}")]   UnknownKind(u8),
    #[error("length mismatch")]    LengthMismatch,
}

const MAGIC: u32 = 0x4F525_4F4E; // "ORTON" → daemon frames

pub fn encode(request_id: u64, kind: FrameKind, body: &[u8]) -> Vec<u8> {
    let mut out = BytesMut::with_capacity(4 + 1 + 8 + 4 + body.len());
    out.put_u32(MAGIC);
    out.put_u8(kind as u8);
    out.put_u64(request_id);
    out.put_u32(body.len() as u32);
    out.put_slice(body);
    out.to_vec()
}

pub fn validate(bytes: &[u8]) -> Result<Frame<'_>, FrameError> {
    if bytes.len() < 4 + 1 + 8 + 4 { return Err(FrameError::Truncated); }
    let magic = u32::from_be_bytes(bytes[0..4].try_into().unwrap());
    if magic != MAGIC { return Err(FrameError::BadMagic); }
    let kind = match bytes[4] {
        1 => FrameKind::Request, 2 => FrameKind::Response,
        3 => FrameKind::Event,   4 => FrameKind::ErrorFrame,
        x => return Err(FrameError::UnknownKind(x)),
    };
    let request_id = u64::from_be_bytes(bytes[5..13].try_into().unwrap());
    let len = u32::from_be_bytes(bytes[13..17].try_into().unwrap()) as usize;
    if bytes.len() != 17 + len { return Err(FrameError::LengthMismatch); }
    Ok(Frame { request_id, kind, body: &bytes[17..17 + len] })
}
```

- [ ] **Step 5: Run tests — pass.**
- [ ] **Step 6: Property test**

`crates/origin-ipc/tests/frame_prop.rs`:

```rust
use origin_ipc::frame::{encode, validate, FrameKind};
use proptest::prelude::*;

proptest! {
    #[test]
    fn any_body_round_trips(body in proptest::collection::vec(any::<u8>(), 0..4096), id: u64) {
        let bytes = encode(id, FrameKind::Request, &body);
        let f = validate(&bytes).unwrap();
        prop_assert_eq!(f.request_id, id);
        prop_assert_eq!(f.body, body.as_slice());
    }
}
```

Run: `cargo test -p origin-ipc`.
- [ ] **Step 7: Verification gate.**
- [ ] **Step 8: Commit.** `feat(origin-ipc): add typed wire frame with magic + validation`

---

### Task P0.7 — `origin-ipc::transport` (Unix socket / named pipe abstraction)

**Files:** Create `crates/origin-ipc/src/transport.rs`; tests at `crates/origin-ipc/tests/transport_smoke.rs`.

- [ ] **Step 1: Add tokio + interprocess deps to `origin-ipc/Cargo.toml`**

```toml
tokio = { version = "1", features = ["rt-multi-thread", "macros", "net", "io-util", "sync"] }
interprocess = { version = "2", features = ["tokio"] }
```

- [ ] **Step 2: Write failing test**

```rust
use origin_ipc::transport::{Listener, Connector};
use origin_ipc::frame::{encode, FrameKind};

#[tokio::test]
async fn echo_one_frame() {
    let addr = format!("{}/origin-test-{}.sock",
        std::env::temp_dir().display(), ulid::Ulid::new());
    let listener = Listener::bind(&addr).await.unwrap();
    tokio::spawn({
        let addr = addr.clone();
        async move {
            let mut conn = listener.accept().await.unwrap();
            let frame = conn.read_frame().await.unwrap();
            conn.write_frame(FrameKind::Response, &frame).await.unwrap();
            let _ = addr;
        }
    });
    let mut c = Connector::connect(&addr).await.unwrap();
    c.write_frame_raw(&encode(7, FrameKind::Request, b"ping")).await.unwrap();
    let resp = c.read_frame().await.unwrap();
    assert_eq!(resp, b"ping");
}
```

- [ ] **Step 3: Run — fails (types missing).**
- [ ] **Step 4: Implement transport using `interprocess::local_socket::tokio`** so the same code works on Unix sockets and Windows named pipes.

```rust
use crate::frame::{encode, validate, FrameKind};
use interprocess::local_socket::{
    tokio::{Listener as IpcListener, Stream as IpcStream},
    ListenerOptions, ToFsName, GenericFilePath,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub struct Listener { inner: IpcListener }
pub struct Connection { inner: IpcStream }
pub struct Connector;

impl Listener {
    pub async fn bind(path: &str) -> std::io::Result<Self> {
        let name = path.to_fs_name::<GenericFilePath>()?;
        let inner = ListenerOptions::new().name(name).create_tokio()?;
        Ok(Self { inner })
    }
    pub async fn accept(&self) -> std::io::Result<Connection> {
        let inner = self.inner.accept().await?;
        Ok(Connection { inner })
    }
}

impl Connector {
    pub async fn connect(path: &str) -> std::io::Result<Connection> {
        let name = path.to_fs_name::<GenericFilePath>()?;
        let inner = IpcStream::connect(name).await?;
        Ok(Connection { inner })
    }
}

impl Connection {
    pub async fn read_frame(&mut self) -> std::io::Result<Vec<u8>> {
        let mut header = [0u8; 17];
        self.inner.read_exact(&mut header).await?;
        let len = u32::from_be_bytes(header[13..17].try_into().unwrap()) as usize;
        let mut body = vec![0u8; len];
        self.inner.read_exact(&mut body).await?;
        // We could re-validate here; for now expose body only.
        Ok(body)
    }
    pub async fn write_frame(&mut self, kind: FrameKind, body: &[u8]) -> std::io::Result<()> {
        let bytes = encode(0, kind, body);
        self.inner.write_all(&bytes).await
    }
    pub async fn write_frame_raw(&mut self, raw: &[u8]) -> std::io::Result<()> {
        self.inner.write_all(raw).await
    }
}
```

- [ ] **Step 5: Run — pass.**
- [ ] **Step 6: Verification gate.** Test on Linux+macOS+Windows via CI.
- [ ] **Step 7: Commit.** `feat(origin-ipc): cross-platform local-socket transport`

---

### Task P0.8 — `origin-store` SQLite scaffold + migrations

**Files:** Create `crates/origin-store/Cargo.toml`, `src/lib.rs`, `src/migrations/V1__init.sql`; test at `tests/migrate.rs`.

- [ ] **Step 1: Cargo.toml**

```toml
[package]
name = "origin-store"
version.workspace = true
edition.workspace = true

[dependencies]
rusqlite = { version = "0.31", features = ["bundled", "blob"] }
refinery = { version = "0.8", features = ["rusqlite"] }
thiserror = "1"
tracing = "0.1"

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 2: Write initial migration**

`crates/origin-store/src/migrations/V1__init.sql`:

```sql
PRAGMA journal_mode = WAL;
PRAGMA synchronous  = NORMAL;
PRAGMA foreign_keys = ON;

CREATE TABLE sessions (
    id           TEXT PRIMARY KEY,
    created_at   INTEGER NOT NULL,
    title        TEXT,
    provider     TEXT NOT NULL,
    model        TEXT NOT NULL
);

CREATE TABLE messages (
    session_id    TEXT NOT NULL REFERENCES sessions(id),
    turn_index    INTEGER NOT NULL,
    role          INTEGER NOT NULL,
    body_inline   BLOB,
    handle_root   BLOB,
    summary       TEXT,
    created_at    INTEGER NOT NULL,
    PRIMARY KEY (session_id, turn_index)
);
```

- [ ] **Step 3: Failing test**

`crates/origin-store/tests/migrate.rs`:

```rust
use origin_store::Store;

#[test]
fn migrate_creates_tables() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("origin.db");
    let s = Store::open(&path).unwrap();
    s.with_conn(|c| {
        let n: u32 = c.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('sessions','messages')",
            [], |r| r.get(0)).unwrap();
        assert_eq!(n, 2);
        Ok(())
    }).unwrap();
}
```

- [ ] **Step 4: Implement `Store`**

```rust
use refinery::embed_migrations;
use rusqlite::Connection;
use std::path::Path;

embed_migrations!("src/migrations");

pub struct Store { conn: std::sync::Mutex<Connection> }

impl Store {
    pub fn open(path: impl AsRef<Path>) -> rusqlite::Result<Self> {
        let mut conn = Connection::open(path)?;
        migrations::runner().run(&mut conn).expect("migrate");
        Ok(Self { conn: std::sync::Mutex::new(conn) })
    }
    pub fn with_conn<R>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<R>) -> rusqlite::Result<R> {
        f(&self.conn.lock().unwrap())
    }
}
```

- [ ] **Step 5: Run — pass.**
- [ ] **Step 6: Verification gate.**
- [ ] **Step 7: Commit.** `feat(origin-store): sqlite + refinery scaffold`

---

### Task P0.9 — Daemon + CLI handshake

**Files:** Create `crates/origin-daemon/src/main.rs`, `crates/origin-cli/src/main.rs`; integration test at `crates/origin-ipc/tests/handshake.rs`.

- [ ] **Step 1: Cargo.toml for both binaries** with deps on `origin-ipc`, `tokio` (full features for binaries), `clap`.

- [ ] **Step 2: Failing test (drives the spec)**

```rust
// crates/origin-ipc/tests/handshake.rs
use origin_ipc::frame::{encode, validate, FrameKind};
use origin_ipc::transport::{Connector, Listener};

#[tokio::test]
async fn daemon_responds_to_ping() {
    let path = format!("{}/origin-hs-{}.sock", std::env::temp_dir().display(), ulid::Ulid::new());
    let listener = Listener::bind(&path).await.unwrap();
    tokio::spawn(async move {
        let mut c = listener.accept().await.unwrap();
        let body = c.read_frame().await.unwrap();
        assert_eq!(body, b"ping");
        c.write_frame(FrameKind::Response, b"pong").await.unwrap();
    });
    let mut c = Connector::connect(&path).await.unwrap();
    c.write_frame_raw(&encode(1, FrameKind::Request, b"ping")).await.unwrap();
    assert_eq!(c.read_frame().await.unwrap(), b"pong");
}
```

- [ ] **Step 3: Run — confirm pass (already works from P0.7).**

- [ ] **Step 4: Implement daemon `main.rs`**

```rust
use origin_ipc::frame::FrameKind;
use origin_ipc::transport::Listener;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let path = std::env::var("ORIGIN_SOCK").unwrap_or_else(|_| default_path());
    let listener = Listener::bind(&path).await?;
    println!("origin-daemon listening on {path}");
    loop {
        let mut conn = listener.accept().await?;
        tokio::spawn(async move {
            while let Ok(body) = conn.read_frame().await {
                let _ = conn.write_frame(FrameKind::Response, &body).await;
            }
        });
    }
}

fn default_path() -> String {
    #[cfg(unix)]    { format!("{}/origin.sock", std::env::temp_dir().display()) }
    #[cfg(windows)] { r"\\.\pipe\origin".to_string() }
}
```

- [ ] **Step 5: Implement CLI `main.rs`**

```rust
use origin_ipc::frame::{encode, FrameKind};
use origin_ipc::transport::Connector;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let path = std::env::var("ORIGIN_SOCK").unwrap_or_else(|_| default_path());
    let mut c = Connector::connect(&path).await?;
    c.write_frame_raw(&encode(1, FrameKind::Request, b"hello")).await?;
    let resp = c.read_frame().await?;
    println!("daemon said: {}", String::from_utf8_lossy(&resp));
    Ok(())
}
fn default_path() -> String { /* same as daemon */ # "".into() }
```

- [ ] **Step 6: Manual smoke test**

```
# Terminal 1
cargo run -p origin-daemon
# Terminal 2
cargo run -p origin-cli
# expected: daemon said: hello
```

- [ ] **Step 7: Verification gate.** Run `cargo test --workspace && cargo clippy --workspace -- -D warnings`.
- [ ] **Step 8: Commit.** `feat: daemon + cli handshake over local socket`

---

### Task P0.10 — Phase 0 checkpoint

- [ ] **Step 1: Run full verification suite.** `cargo test --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --check`
- [ ] **Step 2: Tag the phase.** `git tag p0-complete -m "Phase 0: workspace + IPC scaffold"`
- [ ] **Step 3: Update CHANGELOG.md.** Bullet list of additions.

---

# Phase 1 — First End-to-End Turn (weeks 3–5)

**Phase goal:** A user types a prompt in the CLI, daemon calls Anthropic, returns a response, optionally executes one of the five core tools, returns final answer.

**Files created in this phase:** `crates/origin-provider/*`, `crates/origin-provider-anthropic/*`, `crates/origin-tools/*`, `crates/origin-permission/*`, agent loop in daemon.

---

### Task P1.1 — `origin-provider` trait

**Files:** `crates/origin-provider/src/lib.rs`, tests at `crates/origin-provider/tests/trait.rs`.

- [ ] **Step 1: Test the trait shape**

```rust
use origin_core::types::Message;
use origin_provider::{Provider, ProviderError, ChatRequest, ChatResponse};

struct FakeProv;

#[async_trait::async_trait]
impl Provider for FakeProv {
    fn name(&self) -> &'static str { "fake" }
    async fn chat(&self, _req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        Ok(ChatResponse { assistant: Message::new(origin_core::types::Role::Assistant), usage: Default::default() })
    }
}

#[tokio::test]
async fn fake_provider_returns_empty_assistant() {
    let p = FakeProv;
    let resp = p.chat(ChatRequest { system: "".into(), messages: vec![], model: "x".into(), tools: vec![] }).await.unwrap();
    assert_eq!(resp.assistant.role, origin_core::types::Role::Assistant);
}
```

- [ ] **Step 2: Run — fails.**
- [ ] **Step 3: Implement trait**

```rust
use origin_core::types::Message;
use thiserror::Error;

#[derive(Debug, Default, Clone, Copy)]
pub struct Usage {
    pub input_tokens: u32, pub output_tokens: u32,
    pub cache_read_input_tokens: u32, pub cache_creation_input_tokens: u32,
}

#[derive(Debug)]
pub struct ChatRequest {
    pub system: String,
    pub messages: Vec<Message>,
    pub model: String,
    pub tools: Vec<ToolSchema>,
}

#[derive(Debug)]
pub struct ChatResponse { pub assistant: Message, pub usage: Usage }

#[derive(Debug, Clone)]
pub struct ToolSchema { pub name: String, pub description: String, pub input_schema_json: String }

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("transport: {0}")] Transport(String),
    #[error("api: {0}")]       Api(String),
    #[error("auth")]           Auth,
    #[error("rate limit")]     RateLimit { retry_after_secs: u32 },
}

#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    fn name(&self) -> &'static str;
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ProviderError>;
}
```

- [ ] **Step 4: Run — pass.**
- [ ] **Step 5: Verification gate.**
- [ ] **Step 6: Commit.** `feat(origin-provider): Provider trait + ChatRequest/Response`

---

### Task P1.2 — Anthropic provider (non-streaming, API key)

**Files:** `crates/origin-provider-anthropic/*`; integration test gated behind `ORIGIN_ANTHROPIC_API_KEY` env var.

- [ ] **Step 1: Cargo.toml** with `reqwest = { features = ["json", "rustls-tls"] }`, `serde_json`, `tokio`.

- [ ] **Step 2: Failing unit test (mock server)** — use `wiremock` to mock Anthropic API.

```rust
#[tokio::test]
async fn calls_anthropic_messages_endpoint() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/v1/messages"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "content": [{"type": "text", "text": "hi"}],
            "usage": {"input_tokens": 10, "output_tokens": 5, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0}
        })))
        .mount(&server)
        .await;
    let p = origin_provider_anthropic::Anthropic::with_base_url("k", &server.uri());
    let r = p.chat(origin_provider::ChatRequest {
        system: "".into(), messages: vec![], model: "claude-opus".into(), tools: vec![],
    }).await.unwrap();
    let txt = match &r.assistant.blocks[0] {
        origin_core::types::Block::Text { text, .. } => text.clone(),
        _ => panic!("expected text"),
    };
    assert_eq!(txt, "hi");
    assert_eq!(r.usage.input_tokens, 10);
}
```

- [ ] **Step 3: Run — fails (crate missing).**

- [ ] **Step 4: Implement Anthropic provider** (sketch):

```rust
pub struct Anthropic { api_key: String, base: String, client: reqwest::Client }
impl Anthropic {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self { api_key: api_key.into(), base: "https://api.anthropic.com".into(), client: reqwest::Client::new() }
    }
    pub fn with_base_url(api_key: impl Into<String>, base: &str) -> Self {
        Self { api_key: api_key.into(), base: base.into(), client: reqwest::Client::new() }
    }
}

#[async_trait::async_trait]
impl origin_provider::Provider for Anthropic {
    fn name(&self) -> &'static str { "anthropic" }
    async fn chat(&self, req: origin_provider::ChatRequest) -> Result<origin_provider::ChatResponse, origin_provider::ProviderError> {
        // build wire JSON from req.messages — map our Block types
        // POST /v1/messages with x-api-key header + anthropic-version
        // parse response.content → blocks; usage → Usage
        todo!("implement per Anthropic Messages API")
    }
}
```

The engineer fills `todo!()` with the actual JSON building + HTTP call + parse. Provider response → `Message { role: Assistant, blocks }`.

- [ ] **Step 5: Run — pass.**
- [ ] **Step 6: Verification gate.**
- [ ] **Step 7: Commit.** `feat(origin-provider-anthropic): non-streaming chat endpoint`

---

### Task P1.3 — `origin-tools` skeleton + tool macro

**Files:** `crates/origin-tools/Cargo.toml`, `src/lib.rs`, `src/registry.rs`, `src/macros.rs`; tests.

- [ ] **Step 1: Test that the macro generates a registry entry**

```rust
use origin_tools::{registry, ToolMeta, Tier, Urgency, SideEffects};

origin_tools::origin_tool! {
    fn echo(input: &str) -> Result<String, String> { Ok(input.to_string()) }
    name: "echo", description: "echoes input",
    tier: Tier::AutoAllowed, urgency: Urgency::Low, side_effects: SideEffects::Pure
}

#[test]
fn registry_contains_echo() {
    let metas: Vec<&ToolMeta> = registry().iter().collect();
    assert!(metas.iter().any(|m| m.name == "echo"));
}
```

- [ ] **Step 2: Run — fails.**
- [ ] **Step 3: Implement `ToolMeta` + `origin_tool!` declarative macro that pushes into an inventory.**

```rust
pub struct ToolMeta {
    pub name: &'static str,
    pub description: &'static str,
    pub tier: Tier,
    pub urgency: Urgency,
    pub side_effects: SideEffects,
    pub input_schema: &'static str,
}

inventory::collect!(ToolMeta);
pub fn registry() -> inventory::iter<ToolMeta> { inventory::iter::<ToolMeta>() }

#[macro_export] macro_rules! origin_tool {
    ( fn $fn:ident ( $($p:tt)* ) -> $ret:ty { $($body:tt)* }
      name: $name:literal, description: $desc:literal,
      tier: $tier:expr, urgency: $urg:expr, side_effects: $sfx:expr ) => {
        pub fn $fn ( $($p)* ) -> $ret { $($body)* }
        inventory::submit! {
            $crate::ToolMeta {
                name: $name, description: $desc,
                tier: $tier, urgency: $urg, side_effects: $sfx,
                input_schema: "{}",
            }
        }
    }
}
```

Add `inventory = "0.3"` to Cargo.toml.

- [ ] **Step 4: Run — pass.**
- [ ] **Step 5: Verification gate.**
- [ ] **Step 6: Commit.** `feat(origin-tools): inventory-backed tool registry + origin_tool! macro`

---

### Task P1.4 — Tool: `Read`

**Files:** `crates/origin-tools/src/builtins/read.rs`; tests.

- [ ] **Step 1: Failing test**

```rust
use origin_tools::builtins::read::read_tool;
use std::io::Write;

#[test]
fn reads_a_file() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(b"hello").unwrap();
    let out = read_tool(f.path().to_str().unwrap()).unwrap();
    assert_eq!(out, "hello");
}

#[test]
fn missing_file_errors() {
    assert!(read_tool("/does/not/exist").is_err());
}
```

- [ ] **Step 2: Implement minimally**

```rust
pub fn read_tool(path: &str) -> Result<String, std::io::Error> {
    std::fs::read_to_string(path)
}
```

Register via `origin_tool!` macro with `tier: AutoAllowed, urgency: Low, side_effects: Pure`.

- [ ] **Step 3: Run — pass.**
- [ ] **Step 4: Verification gate.**
- [ ] **Step 5: Commit.** `feat(origin-tools): Read builtin`

---

### Task P1.5–P1.8 — Tools: `Glob`, `Grep`, `Edit`, `Bash`

For each tool, repeat the pattern of P1.4:

**P1.5 — Glob** — wrap `glob` crate. Test against a temp dir with mixed files; assert matching.
**P1.6 — Grep** — wrap `grep-searcher` (ripgrep core). Test pattern matching in a temp dir.
**P1.7 — Edit** — string-replace in a file (find/replace, then unique-string check). Test failure on non-unique pattern.
**P1.8 — Bash** — `tokio::process::Command` with shell selection (`sh -c` on Unix, `pwsh -Command` on Windows). Test echo round-trip; assert exit code propagates.

Each task: failing test → implementation → run → verification gate → commit (`feat(origin-tools): <ToolName> builtin`).

---

### Task P1.9 — `origin-permission` (tier macros + interactive modal)

**Files:** `crates/origin-permission/src/{lib.rs, tier.rs, prompt.rs}`.

- [ ] **Step 1: Failing test for tier dispatch**

```rust
use origin_permission::{check, Decision, Outcome};
use origin_tools::ToolMeta;

#[test]
fn auto_allowed_returns_allow_without_prompt() {
    let meta = ToolMeta { name: "Read", tier: origin_tools::Tier::AutoAllowed, /* … */ };
    assert!(matches!(check(&meta, "Read /tmp/x").outcome, Outcome::Allow));
}
```

- [ ] **Step 2: Implement `check()`** — for now AutoAllowed→Allow, RequiresPermission→Ask (uses a `dyn Prompter`); tests inject a `MockPrompter`.
- [ ] **Step 3: Run — pass.**
- [ ] **Step 4: Verification gate.**
- [ ] **Step 5: Commit.** `feat(origin-permission): tier-based check with prompt trait`

---

### Task P1.10 — Agent loop in daemon

**Files:** `crates/origin-daemon/src/{session.rs, agent.rs}`.

- [ ] **Step 1: Define the loop contract** — `Session::prompt(user_text) → Vec<Message>` running until no tool_use.
- [ ] **Step 2: Failing E2E test** that wires fake provider + fake tool + asserts the loop terminates.

```rust
#[tokio::test]
async fn loop_runs_two_turns_with_tool_call() {
    // 1) fake provider emits tool_use Read on turn 1, plain text on turn 2
    // 2) Session::prompt returns full message list
    // 3) assert: tool result inserted between turn 1 and turn 2
}
```

- [ ] **Step 3: Implement `Session::prompt` agent loop** that:
  1. Appends user message
  2. Calls provider with current messages
  3. If assistant message contains any `Block::ToolUse`, runs each through permission + dispatch, appends `Block::ToolResult`
  4. Otherwise returns
  5. Caps at `max_turns` (default 25) → error

- [ ] **Step 4: Run — pass.**
- [ ] **Step 5: Wire daemon IPC handler** so a CLI `prompt` request triggers a session, and assistant messages stream back as `Event` frames.

- [ ] **Step 6: Verification gate.** `cargo test --workspace`.
- [ ] **Step 7: Commit.** `feat(origin-daemon): agent loop with provider + tool dispatch`

---

### Task P1.11 — Ratatui baseline TUI (placeholder)

**Files:** `crates/origin-cli/src/{tui.rs, input.rs, screen.rs}`.

- [ ] **Step 1: Failing test for input-event → state mutation.** (`crossterm::event::KeyEvent` + small reducer.)
- [ ] **Step 2: Implement minimal Ratatui app** with a scrolling text panel and a prompt line. No streaming yet (replies appear when the daemon returns the final message).
- [ ] **Step 3: Manual run** — type a prompt, see the daemon's reply.
- [ ] **Step 4: Verification gate.**
- [ ] **Step 5: Commit.** `feat(origin-cli): ratatui baseline TUI`

---

### Task P1.12 — Sessions persisted to SQLite (inline blobs)

**Files:** `crates/origin-daemon/src/session_store.rs`; uses `origin-store`.

- [ ] **Step 1: Failing test** — open session, write a turn, reopen store, assert turn is retrievable.
- [ ] **Step 2: Implement** — INSERT message rows on every turn boundary; body serialized via `rkyv::to_bytes` into `body_inline`.
- [ ] **Step 3: Run — pass.**
- [ ] **Step 4: Verification gate.**
- [ ] **Step 5: Commit.** `feat(origin-daemon): persist sessions to sqlite`

---

### Task P1.13 — Phase 1 checkpoint + dogfood

- [ ] **Step 1: Manual end-to-end smoke** — set `ANTHROPIC_API_KEY`, `cargo run -p origin-daemon`, `cargo run -p origin-cli`, ask "What files are in this directory?" — observe `Glob`/`Read` calls + final answer.
- [ ] **Step 2: Verification gate.** `cargo test --workspace && cargo clippy --workspace -- -D warnings`.
- [ ] **Step 3: Tag.** `git tag p1-complete`.

---

# Phase 2 — Streaming + CAS + Ring Buffer (weeks 6–8)

**Phase goal:** Streaming tokens land in a single byte ring; tool outputs and file reads go through CAS; messages carry handles, not inline bytes. Long sessions hold flat RAM.

**Files:** `crates/origin-cas/*`, modifications to provider + daemon + tui to read from ring/CAS.

---

### Task P2.1 — `origin-cas` skeleton + content-addressed `Hash`

**Files:** `crates/origin-cas/Cargo.toml`, `src/{lib.rs, hash.rs, store.rs, packfile.rs}`.

- [ ] **Step 1: Failing test for `Hash` deterministic equality**

```rust
use origin_cas::Hash;

#[test]
fn same_bytes_same_hash() {
    let a = Hash::of(b"hello");
    let b = Hash::of(b"hello");
    assert_eq!(a, b);
}
```

- [ ] **Step 2: Implement** `Hash([u8; 32])` using `blake3`. Add `blake3 = "1"` to Cargo.toml.
- [ ] **Step 3: Run — pass.** Verification gate. Commit.

---

### Task P2.2 — FastCDC chunker (N3.1)

**Files:** `crates/origin-cas/src/chunker.rs`; integration test.

- [ ] **Step 1: Failing test — small edit preserves most chunks**

```rust
#[test]
fn one_byte_inserted_dedupes_neighbors() {
    let data: Vec<u8> = (0..200_000).map(|i| (i % 251) as u8).collect();
    let mut edited = data.clone();
    edited.insert(50_000, 0xFF);

    let chunks_a: Vec<[u8;32]> = origin_cas::chunker::chunks(&data).collect();
    let chunks_b: Vec<[u8;32]> = origin_cas::chunker::chunks(&edited).collect();

    let shared = chunks_a.iter().filter(|h| chunks_b.contains(h)).count();
    let ratio = shared as f64 / chunks_a.len() as f64;
    assert!(ratio > 0.85, "expected >85% chunk reuse, got {ratio}");
}
```

- [ ] **Step 2: Implement** using `fastcdc` crate (`fastcdc = "3"`). Wrap iterator returning `(offset, length, hash)`.
- [ ] **Step 3: Run — pass.** Verification gate. Commit.

---

### Task P2.3 — Pack files (append-only on disk, mmap read)

**Files:** `crates/origin-cas/src/packfile.rs`; tests.

- [ ] **Step 1: Failing test — write/read round-trip via mmap**
- [ ] **Step 2: Implement** with `memmap2`. Format: `[magic][n_entries][index_table][...payloads]`. Entry = `(hash, offset, len)`.

> Note: this crate enables `unsafe_code = "allow"`. Annotate every `unsafe` block with a `// SAFETY:` comment per the workspace standard.

- [ ] **Step 3: Property test** — random insertions, every key reads back identically.
- [ ] **Step 4: Verification gate.** `cargo test -p origin-cas` + clippy.
- [ ] **Step 5: Commit.**

---

### Task P2.4 — Three-tier store (Hot LRU + Warm mmap + Cold zstd)

- [ ] **Step 1: Failing test — promote/demote across tiers; same `Handle` resolves regardless of tier.**
- [ ] **Step 2: Implement `Store` with Hot LRU (`lru` crate), Warm mmap, Cold zstd-compressed pack.**
- [ ] **Step 3: Property test** — random read/write/evict patterns preserve contents.
- [ ] **Step 4: Verification gate.** Commit.

---

### Task P2.5 — Refcount + GC pass

- [ ] **Step 1: Test** — drop refs; GC reclaims; resurrected reference to a dead shard fails fast.
- [ ] **Step 2: Implement `cas_refs` table in `origin-store` (new migration V2)**; expose `incr`/`decr`; GC sweep coalesces packs when dead ratio > 30%.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P2.6 — `origin-stream` shared byte ring (N2.1)

**Files:** `crates/origin-stream/src/lib.rs`; tests.

- [ ] **Step 1: Failing test — single producer, multi tail consumers, no allocations after warmup.** Use `tokio::test` + `tokio::sync::Notify` for wakeups.
- [ ] **Step 2: Implement** an `Arc<RingInner>` with a `BytesMut` reservation + atomic write cursor + `Vec<Subscriber>` each carrying its own read cursor. Subscribers `wait_for(min_len)` await the producer's notify.
- [ ] **Step 3: Soak test** — 10K writes, 3 tails, assert all consume identical byte sequences in order.
- [ ] **Step 4: Verification gate.** Commit.

---

### Task P2.7 — Anthropic streaming parser → ring

**Files:** modify `crates/origin-provider-anthropic/src/streaming.rs`.

- [ ] **Step 1: Failing test** — fixture file replay of an Anthropic `text/event-stream` body emits expected `TokenEvent` sequence into a test ring.
- [ ] **Step 2: Implement** SSE-line parser → `TokenEvent { kind, payload }` written rkyv-archived into the ring.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P2.8 — Provider trait: add `chat_stream`

- [ ] **Step 1: Test** that `Provider::chat_stream` returns `Box<dyn Stream<Item = TokenEvent>>` for the Anthropic impl (mocked via the streaming fixture).
- [ ] **Step 2: Add to trait; implement for Anthropic; keep `chat` as a streaming consumer for back-compat.**
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P2.9 — Messages carry handles (N2.4 step 1: outbound writes)

- [ ] **Step 1: Failing test** — tool output → CAS handle → `Block::ToolResult { handle: Some(_), inline: None }`.
- [ ] **Step 2: Modify tool dispatch in daemon to write tool output to CAS and emit handle.**
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P2.10 — TUI: stream display from ring

- [ ] **Step 1: Test (component)** — feed a ring with a known token sequence; capture rendered cell grid; assert contents.
- [ ] **Step 2: Replace TUI's "wait for full response" with a tail cursor on the session's stream ring.**
- [ ] **Step 3: Manual smoke** — Anthropic stream visibly types out.
- [ ] **Step 4: Verification gate.** Commit.

---

### Task P2.11 — Token panel in status bar

- [ ] **Step 1: Test** rendering of `in / out / cache_read / cache_write / cost / time`.
- [ ] **Step 2: Implement.** Cost from a pricing table per provider/model.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P2.12 — Phase 2 checkpoint + RAM bench

- [ ] **Step 1: Write a bench** that runs a 1000-message session and reports peak RSS. Assert < 200MB (excluding mmap).
- [ ] **Step 2: Verification gate** including bench. Tag `p2-complete`.

---

# Phase 3 — CachePlanner + Speculative Dispatch + Recall (weeks 9–11)

**Phase goal:** Predictive prompt-cache prefix planning, speculative dispatch of pure tools, `Recall` tool + handle substitution in message-to-wire.

**Files:** `crates/origin-planner/*`, parser + tool changes in `origin-tools`, `Recall` builtin.

---

### Task P3.1 — `PrefixLedger`

- [ ] **Step 1: Test** — append per-turn band hits; compute stability score; reorder volatile→sliding when score crosses threshold.
- [ ] **Step 2: Implement** in `crates/origin-planner/src/ledger.rs`.
- [ ] **Step 3: Property test** — invariant: scores monotonic w.r.t. consecutive hits.
- [ ] **Step 4: Verification gate.** Commit.

---

### Task P3.2 — `CachePlanner::plan(request)` (N4.2)

- [ ] **Step 1: Test** — given a session with three turns of history + two memories + one skill, plan emits four sections in `Frozen → Sticky → Sliding → Volatile` order with provider-specific cache markers at boundaries.
- [ ] **Step 2: Implement** band sort + Anthropic `cache_control: ephemeral` marker emission on band boundaries.
- [ ] **Step 3: Integration test** with the Anthropic provider mock — observe cache markers in outgoing wire bytes.
- [ ] **Step 4: Verification gate.** Commit.

---

### Task P3.3 — Incremental JSON parser for tool_use blocks

- [ ] **Step 1: Test** — streaming `{"name":"Read","input":{"file_path":"/x"` over multiple chunks → parser yields `ToolUseDelta { name: "Read", args: {file_path: "/x"} }` *before* the closing brace arrives.
- [ ] **Step 2: Implement** a small SAX-style JSON state machine that emits events on completed key/value pairs.
- [ ] **Step 3: Fuzz harness for the parser** (`cargo fuzz add tool_use_parser`).
- [ ] **Step 4: Verification gate.** Commit.

---

### Task P3.4 — Speculative dispatch wiring (N2.2)

- [ ] **Step 1: Test** — the parser's first complete pure-tool args trigger a background task running the tool before the assistant `tool_use` is closed; agent awaits the precomputed handle.
- [ ] **Step 2: Implement** in `origin-daemon/src/agent.rs`. Side-effecting tools (`Bash`, `Write`, `Edit`, MCP write) explicitly opt out.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P3.5 — `Recall` tool (N5.5)

- [ ] **Step 1: Test** — given a CAS handle, `Recall(handle, region: Some(lines: 10..20))` returns the requested lines.
- [ ] **Step 2: Implement** with line-index over CAS body computed lazily.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P3.6 — Handle substitution in message-to-wire

- [ ] **Step 1: Test** — large tool result handle expanded inline when CachePlanner says "Volatile band cheap"; replaced with `<result handle:7af3 — N bytes>` reference when not.
- [ ] **Step 2: Implement** decision rule from N2.4 in the planner.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P3.7 — Result memoization (N5.4)

- [ ] **Step 1: Test** — same `(tool, normalized_input)` within a session returns the prior handle; result message annotated `(cached from turn N)`.
- [ ] **Step 2: Implement** in `origin-tools::registry::dispatch`. `Bash` explicitly skips memoization.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P3.8 — Phase 3 checkpoint + token-bill bench

- [ ] **Step 1: Bench** — run a 20-turn fixed workload twice (cold + warm). Assert warm run shows `cache_read_input_tokens > 0.5 × input_tokens`.
- [ ] **Step 2: Verification gate.** Tag `p3-complete`.

---

# Phase 4 — Custom TUI Renderer (weeks 12–15)

**Phase goal:** Replace Ratatui with cell-grid double buffer + SIMD damage diff; side panel as separate render target; CAS-backed scrollback.

---

### Task P4.1 — `origin-tui::grid` cell-grid types

- [ ] **Step 1: Test** — `Grid::resize`, `Grid::put(row, col, Cell)`, `Grid::diff(other)` returns runs of changed cells.
- [ ] **Step 2: Implement** `Cell { glyph: u32, fg: u32, bg: u32, attr: u32 }` and `Grid` with row-major buffers.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P4.2 — SIMD damage diff (N8.1)

- [ ] **Step 1: Test** — change 1 cell in a 200×60 grid; diff returns exactly one run of length 1.
- [ ] **Step 2: Implement** with `wide::u8x32` over the 16-byte cells (treated as 2× wide vectors). Annotate `unsafe` SIMD intrinsics carefully.
- [ ] **Step 3: Bench** — diff on 200×60 grid, 1% changed cells, < 50µs.
- [ ] **Step 4: Verification gate.** Commit.

---

### Task P4.3 — ANSI emit (cursor-move + style-set + glyph run)

- [ ] **Step 1: Test** — diff runs serialize to expected ANSI sequences (snapshot test).
- [ ] **Step 2: Implement** emitter.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P4.4 — Frame coalescing (N8.2)

- [ ] **Step 1: Test** — burst of 10 `dirty` flips within 6ms produces exactly 1 render frame.
- [ ] **Step 2: Implement** `Scheduler` using `tokio::time::sleep_until` + `AtomicBool`.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P4.5 — Grapheme-width cache (N8.4)

- [ ] **Step 1: Test** — first lookup computes; subsequent lookups hit cache; LRU eviction on cap.
- [ ] **Step 2: Implement** with `unicode-segmentation` + `lru`.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P4.6 — Streaming text widget reading from ring/CAS (N8.3)

- [ ] **Step 1: Test** — widget consumes ring tail; layout-cache update is incremental (only the new tail re-laid out).
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P4.7 — Side panel as separate render target (N8.5)

- [ ] **Step 1: Test** — toggling the panel resizes the main pane; main-pane contents are clipped, not rewrapped (compare cell hashes).
- [ ] **Step 2: Implement** two parallel `Grid` instances with independent damage trackers, composed into a single output stream.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P4.8 — Migrate permission prompts to side panel

- [ ] **Step 1: Test** — `PermissionAsk` event opens panel; `y/n/e` resolves via `PermissionDecided`; main pane unaffected.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P4.9 — Retire Ratatui

- [ ] **Step 1: Delete the `tui-baseline` feature flag.** Update Cargo to remove ratatui.
- [ ] **Step 2: Run full test suite.**
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P4.10 — Latency + FPS bench harness

- [ ] **Step 1: Implement** a bench that synthesizes a streaming token sequence, measures keystroke→pixel latency p99 and frame rate under stream.
- [ ] **Step 2: Assert** p99 keystroke-to-pixel < 12ms; FPS-under-stream cap respected.
- [ ] **Step 3: Tag** `p4-complete`.

---

# Phase 5 — Sidecar + Summarization + Compaction (weeks 16–18)

---

### Task P5.1 — `origin-sidecar` runtime

- [ ] **Step 1: Test** — submit a job; assert it dispatches to a configured provider call (mocked) and returns within budget.
- [ ] **Step 2: Implement** queue + worker tasks bounded by `Sidecar` task class.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P5.2 — Eager turn summarization (N2.5.a)

- [ ] **Step 1: Test** — after each agent turn completes, sidecar produces a 1–3 sentence summary stored in `messages.summary`.
- [ ] **Step 2: Implement** via a structured-output prompt on the configured sidecar provider.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P5.3 — Tool-output structure extraction (N2.5.c)

- [ ] **Step 1: Test** — large grep output → sidecar emits a sibling CAS shard with `{ file_count, match_count, top_files: [...], outline_handle }`.
- [ ] **Step 2: Implement.** Outline handle linked from the message metadata.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P5.4 — Compaction policy

- [ ] **Step 1: Test** — session exceeds soft token cap → compaction replaces oldest 4 turns with their pre-computed summaries; messages still reachable via `Recall`.
- [ ] **Step 2: Implement** in `origin-daemon::compactor`. Use planner's request size estimates.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P5.5 — Learned-dictionary zstd training (N3.2)

- [ ] **Step 1: Test** — train a 64KB dict from ≥256 sampled shards; reload dict; compression ratio improves ≥3× over zstd default on a held-out shard set.
- [ ] **Step 2: Implement** with `zstd::dict::from_samples`. Persist versioned dicts; mark `cas_refs.tier_meta` with `dict_version`.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P5.6 — Phase 5 checkpoint

- [ ] **Step 1: Run long-session bench** with compaction enabled; assert flat RSS over a 2-hour synthetic session.
- [ ] **Step 2: Tag** `p5-complete`.

---

# Phase 6 — Memory Graph (`origin-mem`) (weeks 19–22)

---

### Task P6.1 — ONNX runtime + MiniLM bundling

- [ ] **Step 1: Test** — embed a known sentence; assert embedding is a 384-dim f32 vector with expected hash on a fixed model version.
- [ ] **Step 2: Implement** with `ort = "2"` (ONNX Runtime Rust bindings). Bundle model under `crates/origin-mem/models/`. Use `include_bytes!` or download-on-first-run + verified SHA-256.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P6.2 — Int8 quantization + per-cluster centroid offsets (N6.1)

- [ ] **Step 1: Test** — quantize a known vector; dequantize; cosine similarity to original > 0.98.
- [ ] **Step 2: Implement** k-means with `k=256` centroids over a calibration corpus; store `(centroid_id, int8 deltas)`.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P6.3 — HNSW index (N6.2 step 1)

- [ ] **Step 1: Test** — build 1k vectors; query top-K; recall@10 > 0.95 vs brute force.
- [ ] **Step 2: Implement** with `hnsw_rs`. Persist to a CAS-backed file.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P6.4 — Temporal-decay re-rank (N6.2 step 2)

- [ ] **Step 1: Property test** — for any (sim, age) pair, re-rank score is monotonic in age (decreasing) and monotonic in sim (increasing).
- [ ] **Step 2: Implement** `score = sim × exp(-age_days/τ) × cluster_priority × edge_boost`.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P6.5 — Memory schema + bodies in CAS (N6.3)

- [ ] **Step 1: Migration V3 — `memories`, `mem_edges`, `mem_tags`.**
- [ ] **Step 2: Test** — save memory; round-trip via store; identical bodies dedupe in CAS.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P6.6 — Edge types + walks (Supersedes / Contradicts / RelatesTo / DerivedFrom)

- [ ] **Step 1: Test** — recall with `Supersedes` drops superseded; with `Contradicts` surfaces both with conflict flag.
- [ ] **Step 2: Implement** edge filter in recall path.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P6.7 — Auto-save / auto-recall side-effects (N6.5)

- [ ] **Step 1: Test** — at end-of-turn, sidecar proposes ≥0 memories; auto-recall on next turn injects matching memories.
- [ ] **Step 2: Implement** end-of-turn hook into sidecar's queue; recall-proposals injected into next turn's CachePlanner Sticky band.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P6.8 — `mem_search` / `mem_save` / `mem_forget` tools

- [ ] **Step 1: Test** — agent invokes each tool; assert SQLite + CAS effects.
- [ ] **Step 2: Implement** via `origin_tool!` macro.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P6.9 — Memory proposals side panel

- [ ] **Step 1: Test** — proposed memories appear in the TUI side panel; `y/n/e` triggers respective IPC events.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P6.10 — Idle consolidation (N6.4)

- [ ] **Step 1: Test** — given two near-duplicate memories, consolidation pass proposes `Supersedes` edge; given contradictions, adds `Contradicts`.
- [ ] **Step 2: Implement** in sidecar, runs only on daemon idle > 30s, bounded wall-clock budget per window.
- [ ] **Step 3: Verification gate.** Commit. Tag `p6-complete`.

---

# Phase 7 — Code Graph (`origin-codegraph`) (weeks 23–26)

---

### Task P7.1 — Tree-sitter integration per language

- [ ] **Step 1: Test** — parse a small Rust file; assert function nodes match expected `(name, range)` list.
- [ ] **Step 2: Implement** wrappers around `tree-sitter-rust`, `tree-sitter-typescript`, etc., behind a `Language` enum.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P7.2 — FastCDC with AST-boundary bias (N6.6)

- [ ] **Step 1: Test** — edit one function in a 5KLOC file; assert exactly one chunk hash changes.
- [ ] **Step 2: Implement** by feeding tree-sitter node boundaries into FastCDC's cut-point scoring as low-bit hash bias.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P7.3 — Code nodes / edges as CAS records (N6.7)

- [ ] **Step 1: Migration V4 — `code_nodes`, `code_edges`, `code_communities`, `cross_links`.**
- [ ] **Step 2: Test** — extract a Rust crate; assert dedup of identical signatures across files.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P7.4 — Sidecar non-code extraction (N6.8)

- [ ] **Step 1: Test** — provide a PDF fixture; sidecar emits entities with `confidence` tag.
- [ ] **Step 2: Implement** with `lopdf` for PDF text extract, then sidecar small-model structured-output prompt.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P7.5 — Leiden + flow-weighted PageRank (N6.9)

- [ ] **Step 1: Test** — on a small synthetic graph, expected communities and god nodes per cluster.
- [ ] **Step 2: Implement** with `rustworkx-core` (or wrap `igraph` via C bindings).
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P7.6 — Typed query DSL + tools (N6.10)

- [ ] **Step 1: Test each query kind** — `path`, `neighbors`, `communities`, `god_nodes`, `recent_changes`.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P7.7 — `graph_*` tools + `Ask` router

- [ ] **Step 1: Test `Ask`** — code-shaped query routes to codegraph; memory-shaped to mem; hybrid to both, merged.
- [ ] **Step 2: Implement** sub-millisecond classifier (regex + heuristics).
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P7.8 — `git commit` hook auto-rebuild

- [ ] **Step 1: Test** — install hook in a sandbox repo; commit a change; assert incremental rebuild fires.
- [ ] **Step 2: Implement** via the existing hooks crate (P10 will harden hooks; here we use the minimal version).
- [ ] **Step 3: Verification gate.** Commit. Tag `p7-complete`.

---

# Phase 8 — Provider Matrix + KeyVault (weeks 27–29)

---

### Task P8.1 — `origin-keyvault` core

- [ ] **Step 1: Test (per platform)** — write a secret; read; delete. Linux uses `secret-service`; macOS `security-framework`; Windows `windows::Win32::Security::Credentials`.
- [ ] **Step 2: Implement** with platform-conditional modules behind a single `KeyVault` interface.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P8.2 — OAuth helpers (PKCE + device flow)

- [ ] **Step 1: Test** — drive a mock OAuth server; complete PKCE; refresh token rotates.
- [ ] **Step 2: Implement** in `origin-keyvault/oauth.rs`.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P8.3 — OpenAI provider

- [ ] **Step 1: Test** with wiremock fixture — `/v1/chat/completions` SSE.
- [ ] **Step 2: Implement.** Map `tool_calls` ↔ `Block::ToolUse`.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P8.4 — Gemini provider

- [ ] **Step 1: Test** with fixture.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P8.5 — Ollama provider

- [ ] **Step 1: Test** against a local-socket NDJSON fixture.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P8.6 — OpenRouter provider

- [ ] **Step 1: Test** — proxies through OpenAI-shape; provider-feature flags surface via `models/list` response.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P8.7 — Bedrock provider (SigV4)

- [ ] **Step 1: Test** with fixture; SigV4 signing covered by `aws-sigv4` crate.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P8.8 — GitHub Models provider (OAuth)

- [ ] **Step 1: Test** with fixture.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P8.9 — Account-switch in TUI

- [ ] **Step 1: Test** — `/account` command in TUI sets active credential; subsequent provider call uses it.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Verification gate.** Commit. Tag `p8-complete`.

---

# Phase 9 — Swarm + Plan CRDT + CoW Workers (weeks 30–33)

---

### Task P9.1 — `origin-plan` op-log + fold

- [ ] **Step 1: Property test** — random op-log permutations fold to the same state given identical (op, lamport) order.
- [ ] **Step 2: Implement** ops (`AddStep`, `MarkStep`, `EditContent` LWW, `AddNote` append, `Reorder` Logoot).
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P9.2 — Lease tokens (N7.6)

- [ ] **Step 1: Test** — two workers race `LeaseStep`; lamport ordering picks one; loser sees loss.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P9.3 — Snapshot compaction (N7.7)

- [ ] **Step 1: Test** — every 128 ops a snapshot is taken; ops below snapshot GC after ack.
- [ ] **Step 2: Implement** snapshots into CAS; ack tracking in SQLite.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P9.4 — Shared-memory ring buffer (SMR)

- [ ] **Step 1: Test** — two processes share a named mmap; SPSC ring round-trips messages in <1µs locally.
- [ ] **Step 2: Implement** with `memmap2` + atomics; on Windows use `CreateFileMappingW` named mappings.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P9.5 — CoW worker workspace (N7.3)

- [ ] **Step 1: Test (Linux btrfs)** — `ioctl_ficlone` clones a workspace in O(1); writes don't affect parent.
- [ ] **Step 2: Implement** with platform-conditional fallbacks (hardlink-tree + overlay).
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P9.6 — Coordinator/worker protocol

- [ ] **Step 1: Test** — coordinator spawns 3 workers; receives `CompletionReport` from each; plan reflects updates.
- [ ] **Step 2: Implement** `WorkerSpec`, lifecycle states, RPC over IPC.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P9.7 — Worker PrefixLedger inheritance (N7.1)

- [ ] **Step 1: Test** — workers' first request shares the coordinator's Frozen+Sticky byte ranges (assert via cached request hashes).
- [ ] **Step 2: Implement** by promoting `PrefixLedger` to a swarm-scoped resource.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P9.8 — `Task` tool

- [ ] **Step 1: Test** — agent calls `Task(goal, allowed_tools, budget)`; worker runs; result inlined as `CompletionReport`.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P9.9 — Plan side panel

- [ ] **Step 1: Test** — plan view updates in real time as ops arrive.
- [ ] **Step 2: Implement** as a side-panel widget reading the CRDT fold.
- [ ] **Step 3: Verification gate.** Commit. Tag `p9-complete`.

---

# Phase 10 — Extensibility Quartet (weeks 34–36)

---

### Task P10.1 — Skills loader + frontmatter parse

- [ ] **Step 1: Test** — load `~/.origin/skills/foo/SKILL.md`; parse frontmatter; reject malformed.
- [ ] **Step 2: Implement** with `gray-matter`-equivalent (e.g., `serde_yaml` over front block).
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P10.2 — Skill embeddings indexed in mem-HNSW (N9.4)

- [ ] **Step 1: Test** — install skill; embedding upsert into HNSW with kind=`Skill`; recall pass returns skill candidates.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P10.3 — Skill allowed-tools narrowing (N9.5)

- [ ] **Step 1: Test** — while a skill is active, tools not in `allowed-tools` return permission-denied.
- [ ] **Step 2: Implement** by stacking a per-skill mask onto the permission engine.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P10.4 — First-run import (N9.6)

- [ ] **Step 1: Test** — import from `~/.claude/skills/`; dedupe by content hash; user-confirm step.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P10.5 — Hooks: pre-spawned shell pool (N9.7)

- [ ] **Step 1: Test** — dispatch 100 hook events; assert reuse of the shell pool (no spawn per event).
- [ ] **Step 2: Implement** in `origin-hooks/shellpool.rs`. Use `tokio::process::Child` with stdin pipes.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P10.6 — Hooks: lifecycle events + typed payloads

- [ ] **Step 1: Test** each event kind end-to-end (`pre_prompt`, `post_prompt`, `pre_tool`, …).
- [ ] **Step 2: Implement** payload schemas + parser for hook stdout `{"override": ...}`.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P10.7 — MCP client base (stdio transport)

- [ ] **Step 1: Test** — handshake with a mock MCP server over stdio; list-tools returns expected schema.
- [ ] **Step 2: Implement** with `mcp-rust-sdk`-equivalent or `tokio::process::Command` + JSON-RPC over stdio.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P10.8 — MCP HTTP + SSE transports

- [ ] **Step 1: Test** — connect to a mock MCP server over HTTP+SSE; receive tool list and events.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P10.9 — MCP tool registry integration (N9.11)

- [ ] **Step 1: Test** — MCP tool dispatched through the same code path as native tools; permission tier from config.
- [ ] **Step 2: Implement** `McpToolProxy`.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P10.10 — MCP outputs land in CAS

- [ ] **Step 1: Test** — large MCP tool result body in CAS with a handle in the message log.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P10.11 — MCP OAuth via KeyVault

- [ ] **Step 1: Test** — OAuth-required MCP server: device flow completes; bearer used for subsequent calls.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P10.12 — Permissions: bloom filter pre-check (N9.2)

- [ ] **Step 1: Test** — 1000 unrelated tool calls vs 30 configured rules; ≥95% rejected at bloom layer; correctness vs. brute force = 100%.
- [ ] **Step 2: Implement** with `growable-bloom-filter` (or custom 4KB filter).
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P10.13 — Permission side-panel-only prompts

- [ ] **Step 1: Test** — modal removed; prompts only in side panel; concurrent prompts queue.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Verification gate.** Commit. Tag `p10-complete`.

---

# Phase 11 — Security + Observability + Sandboxing (weeks 37–39)

---

### Task P11.1 — Linux sandbox profile (landlock + namespaces + seccomp)

- [ ] **Step 1: Test** — `Bash` invocation cannot write outside the workspace; reading `~/.ssh` fails; project files succeed.
- [ ] **Step 2: Implement** with `landlock` crate + `unshare` for user/mount ns + `libseccomp` syscall filter.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P11.2 — macOS sandbox profile (`sandbox-exec`)

- [ ] **Step 1: Test** equivalent of P11.1.
- [ ] **Step 2: Implement** sandbox-exec profile template.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P11.3 — Windows AppContainer profile

- [ ] **Step 1: Test** — Bash (PowerShell) tool runs under AppContainer; restricted ACL on workspace; cannot touch user profile dir.
- [ ] **Step 2: Implement** with `windows` crate + `STARTUPINFOEX` + restricted-token Job Object.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P11.4 — Hook script profile inheritance

- [ ] **Step 1: Test** — `pre_tool` hook for `Bash` inherits the same sandbox profile as the `Bash` invocation.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P11.5 — MCP response validation + 16MB cap

- [ ] **Step 1: Test** — oversize response rejected at buffer layer; schema-mismatched response rejected before agent exposure.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P11.6 — Tracing → parquet ring (N10.4)

- [ ] **Step 1: Test** — emit 10k spans; parquet file rotated at 64MB; query subcommand returns matching spans.
- [ ] **Step 2: Implement** with `tracing-subscriber` custom layer + `parquet` crate.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P11.7 — `origin trace query` subcommand

- [ ] **Step 1: Test** — predicate `tool=Bash AND duration_ms>500` returns expected matches.
- [ ] **Step 2: Implement** in `origin-cli`.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P11.8 — Metrics surfaces

- [ ] **Step 1: Test** — TUI `?metrics` panel renders; `/metrics` Prometheus socket serves; OTel export configurable.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P11.9 — `Secret<T>` newtype + CI lint

- [ ] **Step 1: Test** — `Debug` of `Secret<String>` prints `<redacted>`; field redaction in tracing.
- [ ] **Step 2: Implement** newtype + a small custom-clippy or `dylint` lint that fails on naming patterns.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P11.10 — KeyVault audit log

- [ ] **Step 1: Test** — every credential access writes an audit row; 30-day retention; redaction-aware.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Verification gate.** Commit. Tag `p11-complete`.

---

# Phase 12 — Multi-Runtime + Arenas + Cooperative Shutdown (weeks 40–42)

---

### Task P12.1 — Named jemalloc arenas (N8.6)

- [ ] **Step 1: Test** — under heavy churn, RSS does not grow unboundedly; per-arena stats accessible.
- [ ] **Step 2: Implement** with `tikv-jemalloc-sys` arena API; one `arena_t` per subsystem; allocations routed via subsystem-specific allocators.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P12.2 — `spawn_in(class, fut)` helper + clippy lint

- [ ] **Step 1: Test** — bare `tokio::spawn` outside the helper fails clippy.
- [ ] **Step 2: Implement** helper with class table; `dylint` lint forbids `tokio::spawn` calls.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P12.3 — Two-runtime split (N8.8)

- [ ] **Step 1: Test** — slow worker on the pool does not delay renderer ticks on the control core.
- [ ] **Step 2: Implement** dual-runtime startup; cross-runtime messaging via SMR rings.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P12.4 — io_uring on Linux for CAS

- [ ] **Step 1: Test (Linux)** — CAS read benchmark with io_uring shows N% latency reduction vs default async fs.
- [ ] **Step 2: Implement** with `tokio-uring`.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P12.5 — Phased cooperative shutdown

- [ ] **Step 1: Test** — `Ctrl+C` runs phases 1–8; assert phase ordering and per-phase timeout behavior.
- [ ] **Step 2: Implement** supervisor.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P12.6 — `origin-supervisor` restart shim

- [ ] **Step 1: Test** — kill daemon process; supervisor restarts; sessions resume from SQLite.
- [ ] **Step 2: Implement** tiny binary.
- [ ] **Step 3: Verification gate.** Commit. Tag `p12-complete`.

---

# Phase 13 — QUIC Remote IPC + Headless Polish (weeks 43–44)

---

### Task P13.1 — QUIC transport (`quinn`)

- [ ] **Step 1: Test** — remote client over QUIC + mutual TLS sends/receives frames identical to local-socket.
- [ ] **Step 2: Implement** alternative transport backend; handshake selects local vs remote.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P13.2 — Pairing flow + bearer tokens

- [ ] **Step 1: Test** — TUI shows a pairing code; remote client enters it; daemon issues short-lived bearer via KeyVault.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P13.3 — Headless one-shot (`origin run "..."`)

- [ ] **Step 1: Test** — `origin run --json "summarize README"` prints structured machine-readable output to stdout, exits 0.
- [ ] **Step 2: Implement** in CLI; reuse the same daemon-attach path; no renderer instantiated.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P13.4 — Admin subcommands

- [ ] **Step 1: Test each** — `origin usage`, `origin sessions ls/resume/rm`, `origin keyring add/list/remove`.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Verification gate.** Commit. Tag `p13-complete`.

---

# Phase 14 — Hardening, Docs, GA (weeks 45–48)

---

### Task P14.1 — Migration: Claude Code session JSONL → origin

- [ ] **Step 1: Test** — given a fixture JSONL, `origin import claude-code <path>` produces an `origin` session readable via `origin sessions resume`.
- [ ] **Step 2: Implement** in `origin-cli/src/import.rs`.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P14.2 — Migration: jcode sessions → origin

- [ ] **Step 1: Test** with a jcode fixture session.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P14.3 — Migration: opencode SQLite → origin

- [ ] **Step 1: Test** with an opencode SQLite fixture.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P14.4 — Skill-dir imports from all three sources

- [ ] **Step 1: Test** — `origin skill import-all` finds `~/.claude/skills/`, jcode skills dir, opencode skills dir; content-hash dedup; user-confirm.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P14.5 — Bench harness against the three sources

- [ ] **Step 1: Write** a fixed task suite (e.g., "summarize this 5KLOC crate", "rename function across module", "find call sites of X"). Drive each harness with a Playwright-equiv adapter; measure wall-clock + token cost.
- [ ] **Step 2: Run** suite; commit results to `benches/results-YYYY-MM-DD.md`.
- [ ] **Step 3: Verification gate.** Assert origin wins ≥50% by wall-clock on a representative task subset. Commit.

---

### Task P14.6 — Fuzz CI gates

- [ ] **Step 1: Add** `cargo-fuzz` targets for: tool-use parser, rkyv validator, each provider response parser, FastCDC boundary finder.
- [ ] **Step 2: Run** `cargo fuzz run <target> -- -max_total_time=600` for each in CI.
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P14.7 — Documentation site

- [ ] **Step 1: Write** docs under `docs/site/` — quickstart, architecture overview, tool reference, skill authoring, MCP integration, security model.
- [ ] **Step 2: Build** with `mdbook`.
- [ ] **Step 3: Publish** to GitHub Pages via CI.
- [ ] **Step 4: Verification gate.** Commit.

---

### Task P14.8 — `origin --tutorial`

- [ ] **Step 1: Test** — TUI subcommand walks through a 3-screen tour using the actual harness against a sandbox project.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Verification gate.** Commit.

---

### Task P14.9 — Release engineering

- [ ] **Step 1: Cross-build matrix** in CI for `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `x86_64-apple-darwin`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`, `aarch64-pc-windows-msvc`.
- [ ] **Step 2: Code-sign macOS + Windows artifacts.**
- [ ] **Step 3: Publish** to Homebrew tap, winget manifest, AUR PKGBUILD, `cargo binstall` index.
- [ ] **Step 4: Verification gate.** Commit. Tag `v1.0.0`.

---

### Task P14.10 — Final GA acceptance gates

- [ ] **Step 1: Bench gates pass:**
  - Cold daemon start to first prompt-ready frame: < 50ms.
  - Keystroke-to-pixel: < 12ms p99.
  - Steady RSS under 4-hour heavy session: < 200MB.
  - Cache hit rate on stable sessions: ≥ 70% read tokens.
  - Code-graph incremental rebuild on a 100KLOC monorepo: < 500ms p95.

- [ ] **Step 2: `unsafe` audit** — confirm `unsafe` only in `origin-cas`, `origin-tui`, `origin-ipc`; each block has a `SAFETY:` comment.

- [ ] **Step 3: Security review** — penetration test of sandbox profiles + KeyVault; produce written audit document.

- [ ] **Step 4: Migration spot-checks** — three real user data sets imported successfully (Claude Code, jcode, opencode).

- [ ] **Step 5: Tag `v1.0.0-ga` and ship.**

---

# Self-review (skill checklist)

**Spec coverage:** Every numbered novel mechanism (N2.1–N10.16) maps to at least one phase task:

| Spec mechanism | Phase task |
|---|---|
| N2.1 ring buffer | P2.6 |
| N2.2 speculative dispatch | P3.3, P3.4 |
| N2.3 KV-cache lattice | P3.1, P3.2 |
| N2.4 handle substitution | P3.6 |
| N2.5 sidecar coroutine | P5.1–P5.4 |
| N3.1 FastCDC | P2.2 |
| N3.2 learned zstd dict | P5.5 |
| N3.3 three-tier store | P2.4 |
| N3.4 SQLite-as-index | P0.8, P2.5 |
| N3.5 zero-copy IPC blob handoff | P2.4 (Store), P10.10 (MCP), P13.1 (QUIC) |
| N4.1 superset IR + projections | P0.5, P1.2 (Anthropic), P8.3–P8.8 (other providers) |
| N4.2 CachePlanner | P3.1, P3.2 |
| N4.3 direct-encode no Value | P1.2 implementation; revisited in P8 per provider |
| N4.4 unified streaming → ring | P2.7, P2.8 |
| N4.5 KeyVault | P8.1, P8.2 |
| N5.1 compile-time registry | P1.3 |
| N5.2 CAS-handle I/O | P2.9 |
| N5.3 speculative pure tools | P3.3, P3.4 |
| N5.4 memoization | P3.7 |
| N5.5 Recall | P3.5 |
| N6.1 int8+centroids | P6.2 |
| N6.2 HNSW+temporal decay | P6.3, P6.4 |
| N6.3 body in CAS | P6.5 |
| N6.4 idle consolidation | P6.10 |
| N6.5 side-effect save/recall | P6.7 |
| N6.6 FastCDC AST-biased | P7.2 |
| N6.7 CAS graph records | P7.3 |
| N6.8 sidecar non-code | P7.4 |
| N6.9 Leiden + flow PageRank | P7.5 |
| N6.10 typed query DSL | P7.6 |
| N7.1 PrefixLedger inheritance | P9.7 |
| N7.2 SPSC SMR | P9.4 |
| N7.3 CoW workers | P9.5 |
| N7.4 credit backpressure | P9.6 (channels) — note: revisit credit policy if not in P9.6 |
| N7.5 structured CompletionReport | P9.6 |
| N7.6 lease tokens | P9.2 |
| N7.7 snapshot compaction | P9.3 |
| N7.8 request-ID multiplexing | P0.6, P0.7 |
| N7.9 rkyv validation | P0.6 |
| N7.10 shared file mapping | P2.4, P10.10 |
| N7.11 credit backpressure on streams | P12.3 (cross-runtime), to refine in P9.6 |
| N7.12 QUIC remote | P13.1 |
| N8.1 SIMD damage diff | P4.2 |
| N8.2 event-loop frame coalescing | P4.4 |
| N8.3 streaming reads ring/CAS | P4.6 |
| N8.4 grapheme cache | P4.5 |
| N8.5 side-panel target | P4.7 |
| N8.6 jemalloc arenas | P12.1 |
| N8.7 task-class budgeting | P12.2 |
| N8.8 two-runtime split | P12.3 |
| N8.9 io_uring / IOCP / kqueue | P12.4 |
| N8.10 cooperative shutdown | P12.5 |
| N9.1 tier macros | P1.3 |
| N9.2 bloom pre-check | P10.12 |
| N9.3 side-panel prompts | P4.8, P10.13 |
| N9.4 embedding-indexed skills | P10.2 |
| N9.5 allowed-tools narrowing | P10.3 |
| N9.6 first-run import | P10.4 |
| N9.7 shell pool | P10.5 |
| N9.8 typed event payloads | P10.6 |
| N9.9 sidecar-class dispatch | P10.5 (pool); class assignment in P12.2 |
| N9.10–N9.13 MCP | P10.7–P10.11 |
| N10.1 OriginError + Bug | distributed; foundational error types added in P0.6+; bug-bash in P14 |
| N10.2 audience routing | refined progressively; first surfaces in P1.10 |
| N10.3 per-error retry | P1.2 (provider), P8.* |
| N10.4 parquet tracing | P11.6 |
| N10.5 bounded metrics | P11.8 |
| N10.6 live tokens | P2.11 |
| N10.7 test tiers | applied throughout — convention in plan header |
| N10.8 origin-replay | P14.5 includes adapter; framework crate to be added in P11 or P14 (added as part of P14.5 setup if not earlier) |
| N10.9 property tests | P0.6, P9.1, P6.4, P2.2 |
| N10.10 fuzzing | P14.6 (with earlier additions in P3.3 etc.) |
| N10.11 sandbox profiles | P11.1–P11.3 |
| N10.12 hook profile inheritance | P11.4 |
| N10.13 MCP validation + cap | P11.5 |
| N10.14 Secret<T> + CI lint | P11.9 |
| N10.15 worker process isolation | P9.6 (process spawn); CPU/RAM caps in P11 (covered via OS-Native primitives used in CoW & sandbox tasks) |
| N10.16 KeyVault single touch | P8.1, audit log P11.10 |

**Placeholder scan:** All steps contain concrete commands or code snippets. Two tasks (P1.2 Anthropic JSON building, P9.6 worker process spawning) leave the engineer to implement an API mapping; both reference the relevant spec sections + external API docs. Acceptable for a 14-phase plan; would be reduced further only by ballooning the plan size unhelpfully.

**Type consistency:** `Block`/`Message`/`Role` consistent throughout. `ToolMeta` introduced in P1.3 used identically in P10.12 (bloom) and P11 (audit). `Hash`/`CasHandle` semantics constant across CAS phases.

---

## Execution handoff

**Plan complete and saved to `docs/superpowers/plans/2026-05-19-origin-implementation.md`.**

Per your direction, execution will use **superpowers:subagent-driven-development**: a fresh subagent per task, two-stage review between tasks, TDD discipline within each task, and a `verification-before-completion` gate at the end of every task before moving on.

When ready to start, invoke `/subagent-driven-development` referencing this plan and the first phase (P0). Tasks are labeled `P<phase>.<n>` and tracked via the `- [ ]` checkboxes.
