# `origin` Phase 2 — Streaming + CAS + Ring Buffer — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL — use **superpowers:subagent-driven-development** to execute task-by-task. Within each task follow **superpowers:test-driven-development** (failing test first, run to fail, implement, run to pass) and apply **superpowers:verification-before-completion** — do NOT advance to the next task until the verification gate is fully green. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Provider tokens stream through a single byte ring with multi-tail consumers; tool outputs and large file reads land in a content-addressed store (CAS) with FastCDC chunking and a three-tier (Hot LRU + Warm mmap + Cold zstd) backend; messages carry CAS handles instead of inline bytes; the Ratatui TUI renders tokens as they arrive; the status bar shows live token/cost accounting.

**Architecture:** Build `origin-cas` from the ground up — `Hash`, FastCDC chunker, append-only mmap pack files, three-tier `Store`, refcounted GC. Build `origin-stream` — `Arc<RingInner>` with one writer cursor and many `Subscriber` read cursors, woken by `tokio::sync::Notify`. Extend the Anthropic provider with an SSE parser that writes `TokenEvent`s (rkyv-archived `#[repr(C)]`) into the ring. Add `Provider::chat_stream`. Rewire daemon tool dispatch to put outputs in CAS and propagate handles on `Block::ToolResult`. Open an event channel from daemon to CLI so the TUI can tail streams. Add a token/cost panel. Close with a soak benchmark proving flat RAM over a 1000-message session.

**Tech Stack:** Rust 1.83 (MSRV pin), Tokio (multi-thread daemon, current-thread CLI), `blake3` (hashing), `fastcdc` 3.x (chunking), `memmap2` (pack file mmap), `lru` (hot tier), `zstd` 0.13 (cold tier), `bytes` (ring storage), `rkyv` 0.7 (TokenEvent archival), `eventsource-stream` (Anthropic SSE), `reqwest` streaming, `wiremock` (provider tests), `proptest` (CAS property tests), `tempfile` (test scaffolding).

**Builds on:** `docs/superpowers/specs/2026-05-19-origin-harness-design.md` (spec) and the Phase 1 deliverables (commit `c091ba8`, tag `p1-complete`).

**Phase 2 spec mechanism citations:**
- **N2.1** — single-buffer ring with multi-tail consumers (Tasks P2.6, P2.10)
- **N2.4 step 1** — outbound CAS-handle writes for tool results (Task P2.9)
- **N3.1** — FastCDC variable-length chunking (Task P2.2)
- **N3.3** — three-tier addressable storage (Task P2.4)
- **N4.4** — unified streaming → ring (Tasks P2.7, P2.8)
- **N10.4** — token/usage accounting plumbed for later tracing (Task P2.11)

What is **explicitly out of scope** for Phase 2 (deferred):
- N3.2 learned-dictionary zstd training — Phase 5
- N3.5 zero-copy IPC blob handoff via `SharedHandle` — Phase 11 (uses a simpler "ship body bytes in event frame" approach in P2)
- N2.2 speculative dispatch / N2.3 / N2.5 sidecar / N4.2 CachePlanner / N5.5 Recall — Phase 3+
- io_uring on Linux — Phase 12

---

## Conventions reminder (apply to every task)

**TDD shape, every task:**
1. Write the failing test.
2. Run it — confirm the expected failure mode.
3. Implement the minimum to pass.
4. Run the test — confirm pass.
5. Verification gate (see table).
6. Commit.

**Verification gate per task type:**

| Task type | Verification commands (all must exit 0) |
|---|---|
| Pure-logic / single-crate | `cargo test -p <crate>` + `cargo clippy -p <crate> --all-targets -- -D warnings` + `cargo fmt --check` |
| Cross-crate / daemon / CLI | `cargo test --workspace` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo fmt --check` |
| Final phase gate (P2.12) | All of the above + the new `phase2_ram_soak` bench (assertion-bounded) + tag `p2-complete` |

**Patterns inherited from earlier phases:**
- `[lints] workspace = true` in every crate `Cargo.toml`; workspace inheritance for `version`/`edition`/`rust-version`/`license`/`repository`.
- All shared/persisted/IPC-crossing types derive `Archive + Serialize + Deserialize` from rkyv 0.7 with `#[archive(check_bytes)]`.
- `[lints.rust] unsafe_code = "forbid"` is the default; **the new `origin-cas` and `origin-stream` crates override it to `allow`** because mmap pack files and atomic ring cursors require `unsafe`. Every `unsafe` block must carry a `// SAFETY: …` comment per the workspace standard. Add a crate-level `#![deny(clippy::undocumented_unsafe_blocks)]`.
- `#[must_use]` on every public constructor; `const fn` wherever Rust allows.
- Tests use `.expect("meaningful message")` — never `#[allow(clippy::unwrap_used)]`.
- Custom error enums via `thiserror`; document `# Errors` and `# Panics` on `pub fn`s.
- For each `#[allow(clippy::…)]` add an inline comment justifying it; never blanket-suppress.
- **MSRV pin reflex:** if `cargo check` complains about `edition2024` or "requires Rust 1.85+", pin the offender with `cargo update -p <crate> --precise <ver>`. Record pins in `Cargo.lock`. See `[[project-msrv-dep-pinning]]` memory.
- **Novel-implementation reflex:** every signature subsystem must use a mechanism that beats openclaude / jcode / opencode on tokens or perf (see `[[feedback-novel-implementations]]` memory). Phase 2's novelties: single-buffer multi-tail ring (vs. per-consumer channels), CAS handles in messages (vs. inline bytes), FastCDC content-defined chunking (vs. fixed-size), three-tier addressable namespace (vs. flat blob store).

**Commit style:** Conventional commits, scoped to crate where possible. Each task lands in **one commit**.

---

## File map for Phase 2

| New crate / file | Responsibility |
|---|---|
| `crates/origin-cas/Cargo.toml` | manifest |
| `crates/origin-cas/src/lib.rs` | public surface — `Hash`, `Handle`, `Store`, `ChunkRef`, errors |
| `crates/origin-cas/src/hash.rs` | `Hash([u8; 32])` over blake3 + `Display` (lowercase hex) |
| `crates/origin-cas/src/chunker.rs` | FastCDC iterator yielding `(offset, length, Hash)` |
| `crates/origin-cas/src/packfile.rs` | append-only mmap'd pack file format + reader |
| `crates/origin-cas/src/store.rs` | three-tier `Store` (Hot LRU + Warm mmap + Cold zstd) |
| `crates/origin-cas/src/refs.rs` | refcount + GC pass over CAS shards |
| `crates/origin-cas/tests/hash.rs` | hash determinism + display |
| `crates/origin-cas/tests/chunker.rs` | FastCDC small-edit-dedup test |
| `crates/origin-cas/tests/packfile.rs` | round-trip + property test |
| `crates/origin-cas/tests/store.rs` | tier promote/demote + property test |
| `crates/origin-cas/tests/gc.rs` | refcount + GC reclaim test |
| `crates/origin-stream/Cargo.toml` | manifest |
| `crates/origin-stream/src/lib.rs` | `Ring`, `Subscriber`, `Producer`, `TokenEvent` |
| `crates/origin-stream/src/event.rs` | rkyv-archived `TokenEvent { kind, payload }` |
| `crates/origin-stream/tests/multi_tail.rs` | one writer, three tails — sequence consistency |
| `crates/origin-stream/tests/soak.rs` | 10k writes / 3 tails / no allocations after warmup |
| `crates/origin-provider-anthropic/src/streaming.rs` | SSE parser → `TokenEvent` → ring |
| `crates/origin-provider-anthropic/tests/streaming.rs` | fixture replay (no network) |
| `crates/origin-provider-anthropic/tests/fixtures/sse_hello.txt` | small SSE capture for replay |
| `crates/origin-provider/src/lib.rs` *(modify)* | add `chat_stream` to trait |
| `crates/origin-store/src/migrations/V2__cas_refs.sql` | new table `cas_refs(hash, refcount, tier, last_access)` |
| `crates/origin-daemon/src/protocol.rs` *(modify)* | extend with streaming protocol — `PromptRequest` unchanged, replies carry `event_kind` for ring tokens |
| `crates/origin-daemon/src/agent.rs` *(modify)* | tool output → CAS, emit `Block::ToolResult { handle: Some(_), inline: None }` |
| `crates/origin-daemon/src/stream_relay.rs` *(new)* | bridges a `Subscriber` on the daemon-side ring to a sequence of IPC `Event` frames |
| `crates/origin-daemon/src/main.rs` *(modify)* | wire `Anthropic::with_streaming`, spawn relay per request |
| `crates/origin-daemon/tests/stream_e2e.rs` *(new)* | wiremock-backed: prompt → stream events → completion |
| `crates/origin-cli/src/tui.rs` *(modify)* | live token append from event stream; preserve scrollback |
| `crates/origin-cli/src/status.rs` *(new)* | token/cost panel render |
| `crates/origin-cli/src/main.rs` *(modify)* | consume `Event` frames and pump them into App |

**File-size discipline:** every new `.rs` file targets <300 LOC. If a task naturally pushes a file past 300 LOC, split early (e.g. `store.rs` → `store/hot.rs` + `store/warm.rs` + `store/cold.rs`).

---

## Task P2.1 — `origin-cas` skeleton + content-addressed `Hash`

**Files:**
- Create: `crates/origin-cas/Cargo.toml`
- Create: `crates/origin-cas/src/lib.rs`
- Create: `crates/origin-cas/src/hash.rs`
- Create: `crates/origin-cas/tests/hash.rs`
- Modify: workspace `Cargo.toml` (members list is `crates/*`; no change needed — confirm via `ls`)

- [ ] **Step 1: Manifest** at `crates/origin-cas/Cargo.toml`

```toml
[package]
name = "origin-cas"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[lints]
workspace = true

# CAS uses mmap and atomic cursors; forbid is overridden to allow.
# Every unsafe block carries a `// SAFETY:` comment.
[lints.rust]
unsafe_code = "allow"

[lints.clippy]
undocumented_unsafe_blocks = "deny"

[dependencies]
blake3 = "1"
thiserror = "1"

[dev-dependencies]
hex = "0.4"
```

> Note on lints: workspace lints set `unsafe_code = "forbid"` and a pedantic clippy block; the `[lints.rust]` and `[lints.clippy]` blocks above override the safety lint and add one new clippy deny. Because `[lints] workspace = true` is *also* present implicitly via the table above being empty for unlisted entries — wait, it isn't. Replace `[lints] workspace = true` is **not** added: when you specify per-crate `[lints.rust]` / `[lints.clippy]`, Cargo treats that as full opt-out of workspace lints. To inherit workspace pedantic warnings *and* add overrides, repeat them. For Phase 2 we keep it simple — drop workspace lint inheritance for `origin-cas` and `origin-stream` since they need the unsafe override.

- [ ] **Step 2: Failing test** at `crates/origin-cas/tests/hash.rs`

```rust
use origin_cas::Hash;

#[test]
fn same_bytes_same_hash() {
    let a = Hash::of(b"hello");
    let b = Hash::of(b"hello");
    assert_eq!(a, b);
}

#[test]
fn different_bytes_different_hash() {
    let a = Hash::of(b"hello");
    let b = Hash::of(b"world");
    assert_ne!(a, b);
}

#[test]
fn display_is_lowercase_hex_64_chars() {
    let h = Hash::of(b"x");
    let s = format!("{h}");
    assert_eq!(s.len(), 64);
    assert!(s.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
}

#[test]
fn from_bytes_round_trip() {
    let h = Hash::of(b"y");
    let bytes = *h.as_bytes();
    let h2 = Hash::from_bytes(bytes);
    assert_eq!(h, h2);
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p origin-cas`
Expected: compile error — `origin_cas` and `Hash` don't exist.

- [ ] **Step 4: Implement** `crates/origin-cas/src/hash.rs`

```rust
//! Content-addressed hash type backed by blake3.

use core::fmt;

/// A 32-byte blake3 hash. The canonical CAS address.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Hash([u8; 32]);

impl Hash {
    /// Hash an arbitrary byte slice.
    #[must_use]
    pub fn of(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }

    /// Wrap an existing 32-byte hash.
    #[must_use]
    pub const fn from_bytes(b: [u8; 32]) -> Self {
        Self(b)
    }

    /// Borrow the raw 32 bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}
```

- [ ] **Step 5: Implement** `crates/origin-cas/src/lib.rs`

```rust
//! `origin-cas` — content-addressed store.
//!
//! Phase 2 deliverables: Hash, FastCDC chunker, mmap pack files, three-tier
//! Store, refcount + GC.

#![deny(clippy::undocumented_unsafe_blocks)]

mod hash;

pub use hash::Hash;
```

- [ ] **Step 6: Run tests to verify pass**

Run: `cargo test -p origin-cas`
Expected: 4 tests pass.

- [ ] **Step 7: Verification gate**

All must exit 0:
- `cargo test -p origin-cas`
- `cargo clippy -p origin-cas --all-targets -- -D warnings`
- `cargo fmt --check`

- [ ] **Step 8: Commit**

```bash
git add crates/origin-cas/
git commit -m "feat(origin-cas): crate skeleton + Hash over blake3"
```

---

## Task P2.2 — FastCDC chunker (N3.1)

**Files:**
- Create: `crates/origin-cas/src/chunker.rs`
- Create: `crates/origin-cas/tests/chunker.rs`
- Modify: `crates/origin-cas/Cargo.toml`, `crates/origin-cas/src/lib.rs`

- [ ] **Step 1: Add `fastcdc` dep**

Edit `crates/origin-cas/Cargo.toml`, add to `[dependencies]`:

```toml
fastcdc = "3"
```

If `cargo check -p origin-cas` complains about MSRV (`edition2024` / `requires 1.85+`), pin with `cargo update -p fastcdc --precise <last-1.83-compatible-version>` and record the pin. Per `[[project-msrv-dep-pinning]]`.

- [ ] **Step 2: Failing test** at `crates/origin-cas/tests/chunker.rs`

```rust
use origin_cas::{chunks, Hash};

/// 200 KB of pseudo-random but deterministic bytes, plus one inserted byte at
/// the midpoint. Expect >85% of chunk hashes shared across the two streams.
#[test]
fn one_byte_inserted_dedupes_neighbors() {
    let data: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    let mut edited = data.clone();
    edited.insert(50_000, 0xFF);

    let a: Vec<Hash> = chunks(&data).map(|c| c.hash).collect();
    let b: Vec<Hash> = chunks(&edited).map(|c| c.hash).collect();

    assert!(a.len() > 4, "expected ≥5 chunks on a 200KB stream, got {}", a.len());

    let shared = a.iter().filter(|h| b.contains(h)).count();
    let ratio = shared as f64 / a.len() as f64;
    assert!(ratio > 0.85, "expected >85% chunk reuse, got {ratio:.3}");
}

#[test]
fn small_input_yields_one_chunk() {
    let data = b"short";
    let cs: Vec<_> = chunks(data).collect();
    assert_eq!(cs.len(), 1);
    assert_eq!(cs[0].offset, 0);
    assert_eq!(cs[0].length, data.len());
    assert_eq!(cs[0].hash, Hash::of(data));
}

#[test]
fn chunks_cover_input_exactly() {
    let data: Vec<u8> = (0..120_000u32).map(|i| (i % 199) as u8).collect();
    let cs: Vec<_> = chunks(&data).collect();
    let total: usize = cs.iter().map(|c| c.length).sum();
    assert_eq!(total, data.len());
    // Offsets strictly increasing and contiguous.
    let mut cursor = 0;
    for c in &cs {
        assert_eq!(c.offset, cursor);
        cursor += c.length;
    }
}
```

- [ ] **Step 3: Run — expected fail**

Run: `cargo test -p origin-cas --test chunker`
Expected: compile error — `chunks` not exported.

- [ ] **Step 4: Implement** `crates/origin-cas/src/chunker.rs`

```rust
//! FastCDC content-defined chunker. ~16 KiB average chunk size.
//!
//! Why FastCDC: a small edit (one byte inserted) shifts only the chunk that
//! contains it; downstream chunks keep their content-defined boundaries and
//! hash to the same address. This is the basis of CAS dedup across turns.

use crate::Hash;

/// Single chunk emitted by the FastCDC iterator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkRef {
    pub offset: usize,
    pub length: usize,
    pub hash: Hash,
}

/// Average / min / max chunk sizes (bytes). Match Anthropic-typical tool-output
/// sizes; tweak in Phase 5 once we have shard-size telemetry.
const MIN_SIZE: u32 = 4 * 1024;
const AVG_SIZE: u32 = 16 * 1024;
const MAX_SIZE: u32 = 64 * 1024;

/// Iterate content-defined chunks over `data`.
#[must_use]
pub fn chunks(data: &[u8]) -> ChunkIter<'_> {
    ChunkIter {
        data,
        inner: fastcdc::v2020::FastCDC::new(data, MIN_SIZE, AVG_SIZE, MAX_SIZE),
    }
}

pub struct ChunkIter<'a> {
    data: &'a [u8],
    inner: fastcdc::v2020::FastCDC<'a>,
}

impl<'a> Iterator for ChunkIter<'a> {
    type Item = ChunkRef;

    fn next(&mut self) -> Option<Self::Item> {
        let c = self.inner.next()?;
        let slice = &self.data[c.offset..c.offset + c.length];
        Some(ChunkRef {
            offset: c.offset,
            length: c.length,
            hash: Hash::of(slice),
        })
    }
}
```

- [ ] **Step 5: Re-export** in `crates/origin-cas/src/lib.rs`

```rust
//! `origin-cas` — content-addressed store.

#![deny(clippy::undocumented_unsafe_blocks)]

mod chunker;
mod hash;

pub use chunker::{chunks, ChunkIter, ChunkRef};
pub use hash::Hash;
```

- [ ] **Step 6: Run — expected pass**

Run: `cargo test -p origin-cas`
Expected: all chunker tests + hash tests pass.

- [ ] **Step 7: Verification gate**

- `cargo test -p origin-cas`
- `cargo clippy -p origin-cas --all-targets -- -D warnings`
- `cargo fmt --check`

If clippy flags `cast_possible_truncation` on `c.offset`/`c.length` (fastcdc returns `usize`), no cast is needed — leave as-is. If a future fastcdc version returns `u64`, add a `try_into` with a SAFETY comment.

- [ ] **Step 8: Commit**

```bash
git add crates/origin-cas/
git commit -m "feat(origin-cas): FastCDC chunker (N3.1)"
```

---

## Task P2.3 — Pack files (append-only on disk, mmap read)

**Files:**
- Create: `crates/origin-cas/src/packfile.rs`
- Create: `crates/origin-cas/tests/packfile.rs`
- Modify: `crates/origin-cas/Cargo.toml`, `crates/origin-cas/src/lib.rs`

- [ ] **Step 1: Add deps**

Edit `crates/origin-cas/Cargo.toml`, add to `[dependencies]`:

```toml
memmap2 = "0.9"
byteorder = "1"
```

Add to `[dev-dependencies]`:

```toml
tempfile = "3"
proptest = { version = "=1.4.0", default-features = false, features = ["std"] }
```

- [ ] **Step 2: Failing test** at `crates/origin-cas/tests/packfile.rs`

```rust
use origin_cas::{Hash, PackBuilder, PackReader};
use tempfile::tempdir;

#[test]
fn round_trip_single_payload() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("p0001.pack");

    let mut b = PackBuilder::create(&path).expect("create");
    let h = Hash::of(b"hello");
    b.append(h, b"hello").expect("append");
    let _ = b.finalize().expect("finalize");

    let r = PackReader::open(&path).expect("open");
    let bytes = r.read(h).expect("present");
    assert_eq!(bytes.as_ref(), b"hello");
}

#[test]
fn round_trip_multiple_payloads_in_order() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("p0002.pack");

    let payloads: Vec<&[u8]> = vec![b"alpha", b"beta", b"gamma-payload", b""];
    let hashes: Vec<Hash> = payloads.iter().map(|p| Hash::of(p)).collect();

    let mut b = PackBuilder::create(&path).expect("create");
    for (h, p) in hashes.iter().zip(&payloads) {
        b.append(*h, p).expect("append");
    }
    let _ = b.finalize().expect("finalize");

    let r = PackReader::open(&path).expect("open");
    for (h, p) in hashes.iter().zip(&payloads) {
        assert_eq!(r.read(*h).expect("present").as_ref(), *p);
    }
}

#[test]
fn missing_hash_returns_none() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("p0003.pack");
    let mut b = PackBuilder::create(&path).expect("create");
    b.append(Hash::of(b"a"), b"a").expect("append");
    let _ = b.finalize().expect("finalize");

    let r = PackReader::open(&path).expect("open");
    assert!(r.read(Hash::of(b"absent")).is_none());
}

proptest::proptest! {
    #[test]
    fn random_insertions_read_back_identically(payloads in proptest::collection::vec(proptest::collection::vec(proptest::num::u8::ANY, 0..4096), 1..32)) {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("p_prop.pack");
        let hashes: Vec<Hash> = payloads.iter().map(|p| Hash::of(p)).collect();

        let mut b = PackBuilder::create(&path).expect("create");
        for (h, p) in hashes.iter().zip(&payloads) {
            b.append(*h, p).expect("append");
        }
        let _ = b.finalize().expect("finalize");

        let r = PackReader::open(&path).expect("open");
        for (h, p) in hashes.iter().zip(&payloads) {
            proptest::prop_assert_eq!(r.read(*h).expect("present").as_ref(), &p[..]);
        }
    }
}
```

- [ ] **Step 3: Run — expected fail**

Run: `cargo test -p origin-cas --test packfile`
Expected: compile error — `PackBuilder` / `PackReader` not exported.

- [ ] **Step 4: Implement** `crates/origin-cas/src/packfile.rs`

```rust
//! Append-only pack file.
//!
//! Format (all big-endian):
//!   magic:     4 bytes ("OCPK")
//!   version:   u16
//!   reserved:  u16
//!   payloads:  repeated [hash:32][len:u32][bytes:len]
//!   index:     repeated [hash:32][offset:u64][len:u32], count = entries
//!   footer:    [entries:u64][index_offset:u64][magic:4 "OCFT"]
//!
//! Writes are append-only; readers mmap the whole file and look entries up via
//! a HashMap built from the index.

use crate::Hash;
use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use memmap2::Mmap;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use thiserror::Error;

const MAGIC_HEADER: [u8; 4] = *b"OCPK";
const MAGIC_FOOTER: [u8; 4] = *b"OCFT";
const VERSION: u16 = 1;

#[derive(Debug, Error)]
pub enum PackError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("bad magic")]
    BadMagic,
    #[error("unsupported version {0}")]
    UnsupportedVersion(u16),
    #[error("truncated")]
    Truncated,
}

/// Writer for a brand-new pack file. Writes payloads as they arrive, buffers
/// the index in memory, flushes on `finalize`.
pub struct PackBuilder {
    file: BufWriter<File>,
    path: PathBuf,
    payload_cursor: u64,
    index: Vec<(Hash, u64, u32)>,
}

impl PackBuilder {
    /// Create a new pack file at `path`. Fails if it already exists.
    ///
    /// # Errors
    /// Propagates I/O errors from file creation.
    pub fn create(path: impl AsRef<Path>) -> Result<Self, PackError> {
        let path = path.as_ref().to_path_buf();
        let mut file = BufWriter::new(
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)?,
        );
        file.write_all(&MAGIC_HEADER)?;
        file.write_u16::<BigEndian>(VERSION)?;
        file.write_u16::<BigEndian>(0)?; // reserved
        Ok(Self {
            file,
            path,
            payload_cursor: 4 + 2 + 2, // header length
            index: Vec::new(),
        })
    }

    /// Append a payload addressed by `hash`. Duplicate hashes are stored once
    /// at this layer — callers (Store) handle dedup before reaching here.
    ///
    /// # Errors
    /// Propagates I/O errors. Also fails if `bytes.len()` exceeds `u32::MAX`.
    pub fn append(&mut self, hash: Hash, bytes: &[u8]) -> Result<(), PackError> {
        let len = u32::try_from(bytes.len()).map_err(|_| PackError::Truncated)?;
        self.file.write_all(hash.as_bytes())?;
        self.file.write_u32::<BigEndian>(len)?;
        self.file.write_all(bytes)?;
        let entry_offset = self.payload_cursor;
        self.payload_cursor += 32 + 4 + u64::from(len);
        self.index.push((hash, entry_offset, len));
        Ok(())
    }

    /// Flush the index + footer and close the file.
    ///
    /// # Errors
    /// Propagates I/O errors.
    pub fn finalize(mut self) -> Result<PathBuf, PackError> {
        let index_offset = self.payload_cursor;
        for (h, off, len) in &self.index {
            self.file.write_all(h.as_bytes())?;
            self.file.write_u64::<BigEndian>(*off)?;
            self.file.write_u32::<BigEndian>(*len)?;
        }
        let entries = u64::try_from(self.index.len()).unwrap_or(0);
        self.file.write_u64::<BigEndian>(entries)?;
        self.file.write_u64::<BigEndian>(index_offset)?;
        self.file.write_all(&MAGIC_FOOTER)?;
        self.file.flush()?;
        Ok(self.path)
    }
}

/// mmap-backed reader. Holds an `Mmap` keeping the file mapped while alive.
pub struct PackReader {
    map: Mmap,
    index: HashMap<Hash, (u64, u32)>,
}

impl PackReader {
    /// Open a previously-finalized pack file.
    ///
    /// # Errors
    /// Returns `PackError::BadMagic` / `UnsupportedVersion` / `Truncated` for
    /// malformed inputs; otherwise propagates I/O errors.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, PackError> {
        let mut file = File::open(path.as_ref())?;
        let len = file.metadata()?.len();
        if len < 4 + 2 + 2 + 8 + 8 + 4 {
            return Err(PackError::Truncated);
        }

        // Header.
        let mut header_magic = [0u8; 4];
        file.read_exact(&mut header_magic)?;
        if header_magic != MAGIC_HEADER {
            return Err(PackError::BadMagic);
        }
        let version = file.read_u16::<BigEndian>()?;
        if version != VERSION {
            return Err(PackError::UnsupportedVersion(version));
        }

        // Footer.
        file.seek(SeekFrom::End(-(8 + 8 + 4)))?;
        let entries = file.read_u64::<BigEndian>()?;
        let index_offset = file.read_u64::<BigEndian>()?;
        let mut footer_magic = [0u8; 4];
        file.read_exact(&mut footer_magic)?;
        if footer_magic != MAGIC_FOOTER {
            return Err(PackError::BadMagic);
        }

        // mmap whole file for payload reads.
        // SAFETY: file is opened read-only above; mmap inherits that. No other
        // process should be mutating an in-flight pack file; concurrent
        // builders create disjoint files by name.
        let map = unsafe { Mmap::map(&file)? };

        // Build index hashmap.
        let mut index = HashMap::with_capacity(usize::try_from(entries).unwrap_or(0));
        let mut cursor = usize::try_from(index_offset).map_err(|_| PackError::Truncated)?;
        for _ in 0..entries {
            if cursor + 32 + 8 + 4 > map.len() {
                return Err(PackError::Truncated);
            }
            let mut h = [0u8; 32];
            h.copy_from_slice(&map[cursor..cursor + 32]);
            cursor += 32;
            let off = (&map[cursor..cursor + 8]).read_u64::<BigEndian>()?;
            cursor += 8;
            let len = (&map[cursor..cursor + 4]).read_u32::<BigEndian>()?;
            cursor += 4;
            index.insert(Hash::from_bytes(h), (off, len));
        }

        Ok(Self { map, index })
    }

    /// Look up a hash and return a slice into the mmap'd region. `None` if
    /// the hash isn't present.
    #[must_use]
    pub fn read(&self, hash: Hash) -> Option<PackSlice<'_>> {
        let (off, len) = self.index.get(&hash).copied()?;
        let start = usize::try_from(off).ok()? + 32 + 4; // skip embedded hash+len
        let end = start + usize::try_from(len).ok()?;
        if end > self.map.len() {
            return None;
        }
        Some(PackSlice(&self.map[start..end]))
    }
}

/// Borrow into the mmap'd pack region. Zero-copy.
pub struct PackSlice<'a>(&'a [u8]);

impl AsRef<[u8]> for PackSlice<'_> {
    fn as_ref(&self) -> &[u8] {
        self.0
    }
}
```

- [ ] **Step 5: Re-export** in `crates/origin-cas/src/lib.rs`

```rust
//! `origin-cas` — content-addressed store.

#![deny(clippy::undocumented_unsafe_blocks)]

mod chunker;
mod hash;
mod packfile;

pub use chunker::{chunks, ChunkIter, ChunkRef};
pub use hash::Hash;
pub use packfile::{PackBuilder, PackError, PackReader, PackSlice};
```

- [ ] **Step 6: Run — expected pass**

Run: `cargo test -p origin-cas`
Expected: all hash, chunker, and packfile tests pass (including proptest).

- [ ] **Step 7: Verification gate**

- `cargo test -p origin-cas`
- `cargo clippy -p origin-cas --all-targets -- -D warnings`
- `cargo fmt --check`

Common clippy hits and resolutions:
- `cast_possible_truncation` on `u64 → usize`: justify with an inline comment or use `try_from`.
- `unused_self` on `PackReader::read`: ignore — method semantics depend on the index.

- [ ] **Step 8: Commit**

```bash
git add crates/origin-cas/
git commit -m "feat(origin-cas): append-only mmap pack files"
```

---

## Task P2.4 — Three-tier store (Hot LRU + Warm mmap + Cold zstd)

**Files:**
- Create: `crates/origin-cas/src/store.rs`
- Create: `crates/origin-cas/tests/store.rs`
- Modify: `crates/origin-cas/Cargo.toml`, `crates/origin-cas/src/lib.rs`

- [ ] **Step 1: Add deps**

Edit `crates/origin-cas/Cargo.toml`, add:

```toml
lru = "0.12"
zstd = "0.13"
parking_lot = "0.12"
```

If MSRV pin needed for `lru` or `zstd`, follow the pin reflex.

- [ ] **Step 2: Failing test** at `crates/origin-cas/tests/store.rs`

```rust
use origin_cas::{Hash, Store, StoreConfig};
use tempfile::tempdir;

fn cfg(dir: &std::path::Path) -> StoreConfig {
    StoreConfig {
        root: dir.to_path_buf(),
        hot_capacity: 4,
        warm_pack_target_bytes: 1024 * 1024,
        cold_zstd_level: 3,
    }
}

#[test]
fn put_then_get_via_hot() {
    let dir = tempdir().expect("tempdir");
    let store = Store::open(cfg(dir.path())).expect("open");

    let h = store.put(b"hello").expect("put");
    let v = store.get(h).expect("get").expect("present");
    assert_eq!(v.as_slice(), b"hello");
    assert_eq!(h, Hash::of(b"hello"));
}

#[test]
fn dedup_returns_same_hash() {
    let dir = tempdir().expect("tempdir");
    let store = Store::open(cfg(dir.path())).expect("open");

    let a = store.put(b"dup").expect("put a");
    let b = store.put(b"dup").expect("put b");
    assert_eq!(a, b);
}

#[test]
fn eviction_from_hot_falls_back_to_warm() {
    let dir = tempdir().expect("tempdir");
    let store = Store::open(cfg(dir.path())).expect("open");

    let h0 = store.put(b"first").expect("put");
    // Fill hot beyond capacity (4) to evict the first entry.
    for i in 0..6 {
        let _ = store.put(format!("filler-{i}").as_bytes()).expect("put");
    }
    // h0 should now live in warm; get still works.
    let v = store.get(h0).expect("get").expect("present");
    assert_eq!(v.as_slice(), b"first");
}

#[test]
fn explicit_demote_to_cold_still_readable() {
    let dir = tempdir().expect("tempdir");
    let store = Store::open(cfg(dir.path())).expect("open");

    let h = store.put(b"please-compress-me").expect("put");
    store.demote_to_cold(h).expect("demote");
    let v = store.get(h).expect("get").expect("present");
    assert_eq!(v.as_slice(), b"please-compress-me");
}

proptest::proptest! {
    #[test]
    fn random_read_write_evict_preserves_content(items in proptest::collection::vec(proptest::collection::vec(proptest::num::u8::ANY, 0..512), 1..50)) {
        let dir = tempdir().expect("tempdir");
        let store = Store::open(cfg(dir.path())).expect("open");

        let mut handles = Vec::new();
        for item in &items {
            let h = store.put(item).expect("put");
            handles.push(h);
        }
        for (h, item) in handles.iter().zip(&items) {
            let v = store.get(*h).expect("get").expect("present");
            proptest::prop_assert_eq!(v.as_slice(), &item[..]);
        }
    }
}
```

- [ ] **Step 3: Run — expected fail**

Run: `cargo test -p origin-cas --test store`
Expected: compile error — `Store`, `StoreConfig` not exported.

- [ ] **Step 4: Implement** `crates/origin-cas/src/store.rs`

```rust
//! Three-tier content-addressed store.
//!
//! - **Hot:** in-memory LRU of `Vec<u8>`. Bounded by `hot_capacity` entries.
//! - **Warm:** append-only mmap'd pack files on disk (one open pack per tier).
//! - **Cold:** zstd-compressed pack files; same on-disk format as Warm, but
//!   each payload is independently compressed before append.
//!
//! All three tiers resolve under the same `Hash` namespace. `get(h)` walks
//! Hot → Warm → Cold; the first hit wins. New writes land in Hot; eviction
//! promotes to Warm via `demote_to_warm`; manual `demote_to_cold` recompresses.

use crate::{Hash, PackBuilder, PackReader};
use lru::LruCache;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::fs::{self};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Tunables for `Store::open`.
#[derive(Debug, Clone)]
pub struct StoreConfig {
    /// Root directory holding `warm/` and `cold/` subdirs.
    pub root: PathBuf,
    /// Max entries kept in Hot. LRU evicts down to this.
    pub hot_capacity: usize,
    /// Soft cap before the current Warm pack is sealed and a new one started.
    pub warm_pack_target_bytes: u64,
    /// zstd compression level for Cold (typical: 3).
    pub cold_zstd_level: i32,
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("pack: {0}")]
    Pack(#[from] crate::packfile::PackError),
    #[error("zstd: {0}")]
    Zstd(String),
    #[error("hot capacity must be ≥ 1")]
    BadHotCapacity,
}

enum Tier {
    Warm,
    Cold,
}

struct Inner {
    cfg: StoreConfig,
    hot: LruCache<Hash, Vec<u8>>,
    warm_bytes: u64,
    warm_pending: Vec<(Hash, Vec<u8>)>,
    warm_packs: Vec<PackReader>,
    cold_packs: Vec<PackReader>,
    // `Hash → which Warm pack index` to skip scanning every pack.
    warm_index: HashMap<Hash, usize>,
    cold_index: HashMap<Hash, usize>,
}

/// Tiered CAS store. Thread-safe via internal mutex.
pub struct Store {
    inner: Mutex<Inner>,
}

impl Store {
    /// Open / create a store rooted at `cfg.root`.
    ///
    /// # Errors
    /// Propagates I/O errors; `BadHotCapacity` if `hot_capacity == 0`.
    pub fn open(cfg: StoreConfig) -> Result<Self, StoreError> {
        if cfg.hot_capacity == 0 {
            return Err(StoreError::BadHotCapacity);
        }
        fs::create_dir_all(cfg.root.join("warm"))?;
        fs::create_dir_all(cfg.root.join("cold"))?;

        let mut warm_packs = Vec::new();
        let mut warm_index = HashMap::new();
        for entry in fs::read_dir(cfg.root.join("warm"))? {
            let entry = entry?;
            let r = PackReader::open(entry.path())?;
            for h in r.hashes() {
                warm_index.insert(h, warm_packs.len());
            }
            warm_packs.push(r);
        }

        let mut cold_packs = Vec::new();
        let mut cold_index = HashMap::new();
        for entry in fs::read_dir(cfg.root.join("cold"))? {
            let entry = entry?;
            let r = PackReader::open(entry.path())?;
            for h in r.hashes() {
                cold_index.insert(h, cold_packs.len());
            }
            cold_packs.push(r);
        }

        let cap = NonZeroUsize::new(cfg.hot_capacity).ok_or(StoreError::BadHotCapacity)?;
        Ok(Self {
            inner: Mutex::new(Inner {
                cfg,
                hot: LruCache::new(cap),
                warm_bytes: 0,
                warm_pending: Vec::new(),
                warm_packs,
                cold_packs,
                warm_index,
                cold_index,
            }),
        })
    }

    /// Write bytes; returns the content address. Dedupes via hash lookup.
    ///
    /// # Errors
    /// Propagates I/O errors from any tier the eviction touches.
    pub fn put(&self, bytes: &[u8]) -> Result<Hash, StoreError> {
        let h = Hash::of(bytes);
        let mut inner = self.inner.lock();

        // Dedup: if any tier has it, we're done.
        if inner.hot.contains(&h) || inner.warm_index.contains_key(&h) || inner.cold_index.contains_key(&h) {
            return Ok(h);
        }

        // Insert into Hot; if Hot was full, the evictee is the *least-recently-used*
        // entry, which we shove into a Warm pending buffer.
        if let Some((evicted_hash, evicted_bytes)) = inner.hot.push(h, bytes.to_vec()) {
            // Only demote if the LRU returned an actual eviction (not the same key replaced).
            if evicted_hash != h {
                inner.warm_bytes += evicted_bytes.len() as u64;
                inner.warm_pending.push((evicted_hash, evicted_bytes));
                if inner.warm_bytes >= inner.cfg.warm_pack_target_bytes {
                    flush_warm(&mut inner)?;
                }
            }
        }
        Ok(h)
    }

    /// Read bytes by handle. Walks Hot → Warm → Cold.
    ///
    /// # Errors
    /// Propagates I/O errors; `Ok(None)` if the hash is unknown.
    pub fn get(&self, h: Hash) -> Result<Option<Vec<u8>>, StoreError> {
        let mut inner = self.inner.lock();
        if let Some(v) = inner.hot.get(&h) {
            return Ok(Some(v.clone()));
        }
        // Warm pending (not yet sealed into a pack).
        for (ph, pv) in &inner.warm_pending {
            if *ph == h {
                return Ok(Some(pv.clone()));
            }
        }
        if let Some(&idx) = inner.warm_index.get(&h) {
            if let Some(slice) = inner.warm_packs[idx].read(h) {
                return Ok(Some(slice.as_ref().to_vec()));
            }
        }
        if let Some(&idx) = inner.cold_index.get(&h) {
            if let Some(slice) = inner.cold_packs[idx].read(h) {
                let dec = zstd::decode_all(slice.as_ref()).map_err(|e| StoreError::Zstd(e.to_string()))?;
                return Ok(Some(dec));
            }
        }
        Ok(None)
    }

    /// Force `h` to migrate Hot/Warm → Cold (zstd-compressed pack).
    ///
    /// # Errors
    /// Propagates I/O errors; returns Ok(()) if `h` is already cold or unknown.
    pub fn demote_to_cold(&self, h: Hash) -> Result<(), StoreError> {
        let mut inner = self.inner.lock();
        let bytes = if let Some(v) = inner.hot.pop(&h) {
            v
        } else if let Some(&idx) = inner.warm_index.get(&h) {
            match inner.warm_packs[idx].read(h) {
                Some(s) => s.as_ref().to_vec(),
                None => return Ok(()),
            }
        } else {
            return Ok(());
        };

        let compressed = zstd::encode_all(&bytes[..], inner.cfg.cold_zstd_level)
            .map_err(|e| StoreError::Zstd(e.to_string()))?;
        let next_idx = inner.cold_packs.len();
        let path = inner.cfg.root.join("cold").join(format!("c{next_idx:08}.pack"));
        let mut b = PackBuilder::create(&path)?;
        b.append(h, &compressed)?;
        let _ = b.finalize()?;
        let r = PackReader::open(&path)?;
        inner.cold_index.insert(h, next_idx);
        inner.cold_packs.push(r);
        Ok(())
    }
}

fn flush_warm(inner: &mut Inner) -> Result<(), StoreError> {
    if inner.warm_pending.is_empty() {
        return Ok(());
    }
    let next_idx = inner.warm_packs.len();
    let path = inner.cfg.root.join("warm").join(format!("w{next_idx:08}.pack"));
    let mut b = PackBuilder::create(&path)?;
    let pending = std::mem::take(&mut inner.warm_pending);
    for (h, bytes) in pending {
        b.append(h, &bytes)?;
        inner.warm_index.insert(h, next_idx);
    }
    let _ = b.finalize()?;
    let r = PackReader::open(&path)?;
    inner.warm_packs.push(r);
    inner.warm_bytes = 0;
    Ok(())
}

// We also need a way to enumerate hashes inside a PackReader during open.
// Implemented as a small accessor in packfile.rs (see Step 5).
impl Tier {
    // Phantom — kept for future routing hooks (refcount). Currently unused.
    #[allow(dead_code)]
    fn label(&self) -> &'static str {
        match self {
            Self::Warm => "warm",
            Self::Cold => "cold",
        }
    }
}
```

- [ ] **Step 5: Add `hashes()` accessor** to `crates/origin-cas/src/packfile.rs`

Right after the `impl PackReader` block, add:

```rust
impl PackReader {
    /// Iterate every hash recorded in this pack's index.
    pub fn hashes(&self) -> impl Iterator<Item = Hash> + '_ {
        self.index.keys().copied()
    }
}
```

- [ ] **Step 6: Re-export** in `crates/origin-cas/src/lib.rs`

```rust
//! `origin-cas` — content-addressed store.

#![deny(clippy::undocumented_unsafe_blocks)]

mod chunker;
mod hash;
mod packfile;
mod store;

pub use chunker::{chunks, ChunkIter, ChunkRef};
pub use hash::Hash;
pub use packfile::{PackBuilder, PackError, PackReader, PackSlice};
pub use store::{Store, StoreConfig, StoreError};
```

- [ ] **Step 7: Run — expected pass**

Run: `cargo test -p origin-cas`
Expected: hash + chunker + packfile + store tests all pass.

- [ ] **Step 8: Verification gate**

- `cargo test -p origin-cas`
- `cargo clippy -p origin-cas --all-targets -- -D warnings`
- `cargo fmt --check`

Expect clippy to flag `large_types_passed_by_value` on `Hash` (32 bytes) — allow with an inline comment if needed; the project policy is to pass `Hash` by value since it's `Copy`.

- [ ] **Step 9: Commit**

```bash
git add crates/origin-cas/
git commit -m "feat(origin-cas): three-tier store (Hot LRU + Warm mmap + Cold zstd) (N3.3)"
```

---

## Task P2.5 — Refcount + GC pass

**Files:**
- Create: `crates/origin-cas/src/refs.rs`
- Create: `crates/origin-cas/tests/gc.rs`
- Modify: `crates/origin-cas/src/lib.rs`
- Modify: `crates/origin-store/src/migrations/V2__cas_refs.sql` (new)

- [ ] **Step 1: New SQLite migration** at `crates/origin-store/src/migrations/V2__cas_refs.sql`

```sql
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
```

- [ ] **Step 2: Failing test** at `crates/origin-cas/tests/gc.rs`

```rust
use origin_cas::{Hash, RefTable};
use rusqlite::Connection;

fn make_table() -> (Connection, RefTable) {
    let conn = Connection::open_in_memory().expect("memdb");
    conn.execute_batch(
        "CREATE TABLE cas_refs (hash BLOB PRIMARY KEY, refcount INTEGER NOT NULL DEFAULT 0, tier INTEGER NOT NULL DEFAULT 0, last_access INTEGER NOT NULL);",
    )
    .expect("schema");
    let table = RefTable::new();
    (conn, table)
}

#[test]
fn incr_then_decr_reaches_zero() {
    let (conn, table) = make_table();
    let h = Hash::of(b"x");
    table.incr(&conn, h).expect("incr");
    assert_eq!(table.get(&conn, h).expect("get"), Some(1));
    table.decr(&conn, h).expect("decr");
    assert_eq!(table.get(&conn, h).expect("get"), Some(0));
}

#[test]
fn dead_hashes_lists_only_zero_count() {
    let (conn, table) = make_table();
    let a = Hash::of(b"a");
    let b = Hash::of(b"b");
    let c = Hash::of(b"c");
    table.incr(&conn, a).expect("incr a");
    table.incr(&conn, b).expect("incr b1");
    table.incr(&conn, b).expect("incr b2");
    table.incr(&conn, c).expect("incr c");
    table.decr(&conn, c).expect("decr c");
    let dead: Vec<Hash> = table.dead_hashes(&conn).expect("dead").collect();
    assert_eq!(dead, vec![c]);
}

#[test]
fn decr_below_zero_is_clamped_and_errors() {
    let (conn, table) = make_table();
    let h = Hash::of(b"never-incremented");
    let err = table.decr(&conn, h);
    assert!(err.is_err(), "decr below zero must error");
}
```

- [ ] **Step 3: Run — expected fail**

Run: `cargo test -p origin-cas --test gc`
Expected: compile error — `RefTable` not exported. Add `rusqlite` to dev-deps.

Edit `crates/origin-cas/Cargo.toml` `[dev-dependencies]`:

```toml
rusqlite = { version = "0.31", features = ["bundled"] }
```

And add to `[dependencies]`:

```toml
rusqlite = { version = "0.31", features = ["bundled"] }
```

- [ ] **Step 4: Implement** `crates/origin-cas/src/refs.rs`

```rust
//! Refcount table for CAS shards.
//!
//! The actual SQLite schema lives in `origin-store` migrations (V2). This
//! module is a thin typed wrapper: callers pass a `&Connection`, we use
//! parameterised SQL. GC is `dead_hashes` → caller deletes pack entries +
//! removes rows.

use crate::Hash;
use rusqlite::{params, Connection, OptionalExtension};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RefError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("decr below zero for {0}")]
    BelowZero(Hash),
}

#[derive(Debug, Default, Clone, Copy)]
pub struct RefTable;

impl RefTable {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Increment refcount; inserts a row at 1 if absent.
    ///
    /// # Errors
    /// Propagates sqlite errors.
    pub fn incr(&self, conn: &Connection, h: Hash) -> Result<(), RefError> {
        let now = now_ms();
        conn.execute(
            "INSERT INTO cas_refs (hash, refcount, tier, last_access) \
             VALUES (?1, 1, 0, ?2) \
             ON CONFLICT(hash) DO UPDATE SET refcount = refcount + 1, last_access = ?2",
            params![h.as_bytes().as_slice(), now],
        )?;
        Ok(())
    }

    /// Decrement refcount. Errors if the row is absent or already at zero.
    ///
    /// # Errors
    /// Returns `BelowZero` if no positive count exists; otherwise sqlite errors.
    pub fn decr(&self, conn: &Connection, h: Hash) -> Result<(), RefError> {
        let cur = self.get(conn, h)?;
        match cur {
            None | Some(0) => Err(RefError::BelowZero(h)),
            Some(_) => {
                conn.execute(
                    "UPDATE cas_refs SET refcount = refcount - 1, last_access = ?2 WHERE hash = ?1",
                    params![h.as_bytes().as_slice(), now_ms()],
                )?;
                Ok(())
            }
        }
    }

    /// Read the current count for `h`, or `None` if no row exists.
    ///
    /// # Errors
    /// Propagates sqlite errors.
    pub fn get(&self, conn: &Connection, h: Hash) -> Result<Option<i64>, RefError> {
        let c = conn
            .query_row(
                "SELECT refcount FROM cas_refs WHERE hash = ?1",
                params![h.as_bytes().as_slice()],
                |r| r.get::<_, i64>(0),
            )
            .optional()?;
        Ok(c)
    }

    /// Enumerate all hashes with refcount = 0 (GC candidates).
    ///
    /// # Errors
    /// Propagates sqlite errors via the iterator collection.
    pub fn dead_hashes(&self, conn: &Connection) -> Result<impl Iterator<Item = Hash>, RefError> {
        let mut stmt = conn.prepare("SELECT hash FROM cas_refs WHERE refcount = 0")?;
        let rows: Vec<Hash> = stmt
            .query_map([], |r| {
                let bytes: Vec<u8> = r.get(0)?;
                let mut arr = [0u8; 32];
                if bytes.len() == 32 {
                    arr.copy_from_slice(&bytes);
                }
                Ok(Hash::from_bytes(arr))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows.into_iter())
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}
```

- [ ] **Step 5: Re-export** in `crates/origin-cas/src/lib.rs`

```rust
//! `origin-cas` — content-addressed store.

#![deny(clippy::undocumented_unsafe_blocks)]

mod chunker;
mod hash;
mod packfile;
mod refs;
mod store;

pub use chunker::{chunks, ChunkIter, ChunkRef};
pub use hash::Hash;
pub use packfile::{PackBuilder, PackError, PackReader, PackSlice};
pub use refs::{RefError, RefTable};
pub use store::{Store, StoreConfig, StoreError};
```

- [ ] **Step 6: Run — expected pass**

Run: `cargo test -p origin-cas`
Expected: all tests pass (including new gc tests).

- [ ] **Step 7: Confirm migration runs** in `origin-store`

Run: `cargo test -p origin-store`
Expected: pass. (refinery will pick up `V2__cas_refs.sql` from the embedded migrations dir at compile time.)

- [ ] **Step 8: Verification gate**

- `cargo test --workspace`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo fmt --check`

- [ ] **Step 9: Commit**

```bash
git add crates/origin-cas/ crates/origin-store/src/migrations/V2__cas_refs.sql
git commit -m "feat(origin-cas): refcount table + GC enumeration"
```

---

## Task P2.6 — `origin-stream` shared byte ring (N2.1)

**Files:**
- Create: `crates/origin-stream/Cargo.toml`
- Create: `crates/origin-stream/src/lib.rs`
- Create: `crates/origin-stream/src/event.rs`
- Create: `crates/origin-stream/tests/multi_tail.rs`
- Create: `crates/origin-stream/tests/soak.rs`

- [ ] **Step 1: Manifest**

```toml
[package]
name = "origin-stream"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[lints.rust]
unsafe_code = "allow"   # raw cursor atomics

[lints.clippy]
undocumented_unsafe_blocks = "deny"

[dependencies]
bytes = "1"
rkyv = { version = "0.7", features = ["validation", "bytecheck"] }
tokio = { version = "1", features = ["sync"] }
thiserror = "1"
parking_lot = "0.12"

[dev-dependencies]
tokio = { version = "1", features = ["rt-multi-thread", "macros", "test-util", "time", "sync"] }
```

- [ ] **Step 2: Failing test** at `crates/origin-stream/tests/multi_tail.rs`

```rust
use origin_stream::{Ring, TokenEvent, TokenKind};

#[tokio::test]
async fn single_producer_three_consumers_see_same_sequence() {
    let ring = Ring::with_capacity(64 * 1024);
    let mut sub_a = ring.subscribe();
    let mut sub_b = ring.subscribe();
    let mut sub_c = ring.subscribe();

    let events: Vec<TokenEvent> = (0..50)
        .map(|i| TokenEvent::new(TokenKind::TextDelta, format!("tok-{i}").into_bytes()))
        .collect();

    let producer = {
        let ring = ring.clone();
        let events = events.clone();
        tokio::spawn(async move {
            for ev in events {
                ring.publish(&ev).expect("publish");
            }
            ring.close();
        })
    };

    let collect = |mut sub: origin_stream::Subscriber| {
        async move {
            let mut out = Vec::new();
            while let Some(ev) = sub.next().await.expect("recv") {
                out.push(ev);
            }
            out
        }
    };

    let (a, b, c) = tokio::join!(collect(sub_a), collect(sub_b), collect(sub_c));
    producer.await.expect("producer task");

    assert_eq!(a.len(), 50);
    assert_eq!(a, b);
    assert_eq!(b, c);
}
```

- [ ] **Step 3: Failing soak test** at `crates/origin-stream/tests/soak.rs`

```rust
use origin_stream::{Ring, TokenEvent, TokenKind};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ten_thousand_writes_three_tails_consistent() {
    let ring = Ring::with_capacity(4 * 1024 * 1024);
    let tails = (0..3).map(|_| ring.subscribe()).collect::<Vec<_>>();

    let producer = {
        let ring = ring.clone();
        tokio::spawn(async move {
            for i in 0..10_000u32 {
                let ev = TokenEvent::new(TokenKind::TextDelta, i.to_be_bytes().to_vec());
                ring.publish(&ev).expect("publish");
            }
            ring.close();
        })
    };

    let mut handles = Vec::new();
    for mut sub in tails {
        handles.push(tokio::spawn(async move {
            let mut count = 0u32;
            while let Some(ev) = sub.next().await.expect("recv") {
                let bytes = ev.payload();
                let arr: [u8; 4] = bytes.try_into().expect("4 bytes");
                assert_eq!(u32::from_be_bytes(arr), count);
                count += 1;
            }
            count
        }));
    }
    producer.await.expect("producer");
    for h in handles {
        assert_eq!(h.await.expect("tail"), 10_000);
    }
}
```

- [ ] **Step 4: Run — expected fail**

Run: `cargo test -p origin-stream`
Expected: compile error — `Ring`, `Subscriber`, `TokenEvent`, `TokenKind` not defined.

- [ ] **Step 5: Implement event** `crates/origin-stream/src/event.rs`

```rust
//! `TokenEvent` — rkyv-archived discriminated record.
//!
//! Per spec N4.4 the ring stores rkyv-archived `TokenEvent` records so the
//! provider stream parser, the renderer, and the tool-use parser can all read
//! the same bytes with no intermediate `String`.

use rkyv::{Archive, Deserialize, Serialize};

#[derive(Archive, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[archive(check_bytes)]
#[repr(u8)]
pub enum TokenKind {
    /// Streaming text delta from the assistant.
    TextDelta = 0,
    /// `tool_use` JSON delta (full input arrives in fragments).
    ToolUseDelta = 1,
    /// `thinking` token delta (extended thinking).
    ThinkingDelta = 2,
    /// Provider boundary: turn complete, no more deltas this round.
    TurnEnd = 3,
    /// Provider sent usage stats after `message_stop`.
    Usage = 4,
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[archive(check_bytes)]
pub struct TokenEvent {
    kind: TokenKind,
    payload: Vec<u8>,
}

impl TokenEvent {
    #[must_use]
    pub const fn new(kind: TokenKind, payload: Vec<u8>) -> Self {
        Self { kind, payload }
    }

    #[must_use]
    pub const fn kind(&self) -> TokenKind {
        self.kind
    }

    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }
}
```

- [ ] **Step 6: Implement ring** `crates/origin-stream/src/lib.rs`

```rust
//! `origin-stream` — single-producer multi-consumer byte ring.
//!
//! Mechanism N2.1: one append-only `Bytes` buffer + an atomic write cursor;
//! each subscriber holds its own read cursor. Wakeups via `tokio::sync::Notify`.
//! After warmup the ring never reallocates (it's a fixed-capacity buffer).
//!
//! Records are rkyv-archived `TokenEvent`s, length-prefixed (`u32` BE).

#![deny(clippy::undocumented_unsafe_blocks)]

mod event;

pub use event::{TokenEvent, TokenKind};

use parking_lot::Mutex;
use rkyv::{archived_root, Deserialize, Infallible};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::Notify;

#[derive(Debug, Error)]
pub enum RingError {
    #[error("ring closed")]
    Closed,
    #[error("record too large for ring: {0} bytes")]
    TooLarge(usize),
    #[error("rkyv encode: {0}")]
    Encode(String),
    #[error("rkyv decode: {0}")]
    Decode(String),
}

struct Inner {
    buf: Mutex<Vec<u8>>,
    write_cursor: AtomicUsize,
    notify: Notify,
    closed: AtomicBool,
    capacity: usize,
}

/// Cloneable handle to the underlying ring.
#[derive(Clone)]
pub struct Ring {
    inner: Arc<Inner>,
}

impl Ring {
    /// Create a ring with a fixed byte capacity. Records exceeding capacity
    /// fail with `TooLarge`.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Inner {
                buf: Mutex::new(Vec::with_capacity(capacity)),
                write_cursor: AtomicUsize::new(0),
                notify: Notify::new(),
                closed: AtomicBool::new(false),
                capacity,
            }),
        }
    }

    /// Append a `TokenEvent`. Wakes all subscribers.
    ///
    /// # Errors
    /// `Closed` if the producer has called `close()`; `TooLarge` if the
    /// archived record + length prefix don't fit the remaining capacity.
    /// (Phase 2: no wrap-around. The ring is sized for one turn.)
    pub fn publish(&self, ev: &TokenEvent) -> Result<(), RingError> {
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(RingError::Closed);
        }
        let bytes = rkyv::to_bytes::<_, 256>(ev).map_err(|e| RingError::Encode(e.to_string()))?;
        let len = u32::try_from(bytes.len()).map_err(|_| RingError::TooLarge(bytes.len()))?;

        let mut buf = self.inner.buf.lock();
        let new_total = buf.len() + 4 + bytes.len();
        if new_total > self.inner.capacity {
            return Err(RingError::TooLarge(bytes.len()));
        }
        buf.extend_from_slice(&len.to_be_bytes());
        buf.extend_from_slice(&bytes);
        self.inner.write_cursor.store(buf.len(), Ordering::Release);
        drop(buf);
        self.inner.notify.notify_waiters();
        Ok(())
    }

    /// Mark the ring as closed; subscribers see `Ok(None)` after the last record.
    pub fn close(&self) {
        self.inner.closed.store(true, Ordering::Release);
        self.inner.notify.notify_waiters();
    }

    /// Create a subscriber starting at the current write cursor.
    #[must_use]
    pub fn subscribe(&self) -> Subscriber {
        let start = self.inner.write_cursor.load(Ordering::Acquire);
        Subscriber {
            ring: self.clone(),
            read_cursor: start,
        }
    }
}

/// One tail. Each subscriber tracks its own read position.
pub struct Subscriber {
    ring: Ring,
    read_cursor: usize,
}

impl Subscriber {
    /// Await the next `TokenEvent`. Returns `Ok(None)` when the ring closes
    /// and the subscriber has drained all records.
    ///
    /// # Errors
    /// Propagates rkyv decode errors.
    pub async fn next(&mut self) -> Result<Option<TokenEvent>, RingError> {
        loop {
            let write = self.ring.inner.write_cursor.load(Ordering::Acquire);
            if self.read_cursor < write {
                let buf = self.ring.inner.buf.lock();
                let len_bytes: [u8; 4] = buf[self.read_cursor..self.read_cursor + 4]
                    .try_into()
                    .map_err(|_| RingError::Decode("len prefix".into()))?;
                let len = u32::from_be_bytes(len_bytes) as usize;
                let start = self.read_cursor + 4;
                let end = start + len;
                let slice = &buf[start..end];
                let archived = archived_root::<TokenEvent>(slice);
                let ev: TokenEvent = archived
                    .deserialize(&mut Infallible)
                    .map_err(|e| RingError::Decode(format!("{e:?}")))?;
                self.read_cursor = end;
                return Ok(Some(ev));
            }
            if self.ring.inner.closed.load(Ordering::Acquire) {
                return Ok(None);
            }
            let notified = self.ring.inner.notify.notified();
            // Re-check under the notified future to close the wake-race window.
            if self.ring.inner.write_cursor.load(Ordering::Acquire) > self.read_cursor
                || self.ring.inner.closed.load(Ordering::Acquire)
            {
                continue;
            }
            notified.await;
        }
    }
}
```

- [ ] **Step 7: Run — expected pass**

Run: `cargo test -p origin-stream`
Expected: `multi_tail` + `soak` both pass.

- [ ] **Step 8: Verification gate**

- `cargo test -p origin-stream`
- `cargo clippy -p origin-stream --all-targets -- -D warnings`
- `cargo fmt --check`

If clippy raises `large_enum_variant` on `TokenKind`, ignore — the discriminant is a u8 (`#[repr(u8)]`).

- [ ] **Step 9: Commit**

```bash
git add crates/origin-stream/
git commit -m "feat(origin-stream): single-buffer multi-tail token ring (N2.1)"
```

---

## Task P2.7 — Anthropic streaming parser → ring

**Files:**
- Create: `crates/origin-provider-anthropic/src/streaming.rs`
- Create: `crates/origin-provider-anthropic/tests/streaming.rs`
- Create: `crates/origin-provider-anthropic/tests/fixtures/sse_hello.txt`
- Modify: `crates/origin-provider-anthropic/Cargo.toml`
- Modify: `crates/origin-provider-anthropic/src/lib.rs`

- [ ] **Step 1: Add deps**

Edit `crates/origin-provider-anthropic/Cargo.toml`. Add to `[dependencies]`:

```toml
origin-stream = { path = "../origin-stream" }
eventsource-stream = "0.2"
futures-util = "0.3"
```

Already present: `reqwest` (we'll add the `stream` feature). Update the `reqwest` line:

```toml
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls", "stream"] }
```

- [ ] **Step 2: Create the SSE fixture**

`crates/origin-provider-anthropic/tests/fixtures/sse_hello.txt`:

```
event: message_start
data: {"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","content":[],"model":"claude-test","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":7,"output_tokens":0,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hel"}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"lo!"}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"input_tokens":7,"output_tokens":2,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}

event: message_stop
data: {"type":"message_stop"}

```

Note: each SSE record is separated by a blank line; the file ends with a trailing newline.

- [ ] **Step 3: Failing test** at `crates/origin-provider-anthropic/tests/streaming.rs`

```rust
use origin_provider_anthropic::streaming::parse_into_ring;
use origin_stream::{Ring, TokenKind};
use std::path::PathBuf;

#[tokio::test]
async fn fixture_emits_two_text_deltas_then_turn_end() {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("tests/fixtures/sse_hello.txt");
    let body = std::fs::read(&path).expect("read fixture");

    let ring = Ring::with_capacity(64 * 1024);
    let mut sub = ring.subscribe();

    let r = ring.clone();
    let parser = tokio::spawn(async move {
        parse_into_ring(body.as_slice(), &r).await.expect("parse");
        r.close();
    });

    let mut text_payloads: Vec<String> = Vec::new();
    let mut saw_turn_end = false;
    let mut saw_usage = false;
    while let Some(ev) = sub.next().await.expect("recv") {
        match ev.kind() {
            TokenKind::TextDelta => text_payloads.push(String::from_utf8_lossy(ev.payload()).into_owned()),
            TokenKind::TurnEnd => saw_turn_end = true,
            TokenKind::Usage => saw_usage = true,
            _ => {}
        }
    }
    parser.await.expect("parser");

    assert_eq!(text_payloads, vec!["Hel".to_string(), "lo!".to_string()]);
    assert!(saw_turn_end, "expected TurnEnd");
    assert!(saw_usage, "expected Usage");
}
```

- [ ] **Step 4: Run — expected fail**

Run: `cargo test -p origin-provider-anthropic --test streaming`
Expected: compile error — `streaming::parse_into_ring` not defined.

- [ ] **Step 5: Implement** `crates/origin-provider-anthropic/src/streaming.rs`

```rust
//! SSE → `TokenEvent` adapter.
//!
//! Reads an Anthropic `text/event-stream` body byte stream and writes
//! `TokenEvent`s into an `origin_stream::Ring`. Handles `content_block_delta`,
//! `message_delta` (stop_reason → TurnEnd), and `message_delta` usage.

use futures_util::StreamExt;
use origin_stream::{Ring, RingError, TokenEvent, TokenKind};
use serde::Deserialize;
use thiserror::Error;
use tokio_util::io::ReaderStream;

#[derive(Debug, Error)]
pub enum StreamingError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("ring: {0}")]
    Ring(#[from] RingError),
    #[error("sse: {0}")]
    Sse(String),
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireEvent {
    MessageStart {},
    ContentBlockStart {},
    ContentBlockDelta {
        index: u32,
        delta: WireDelta,
    },
    ContentBlockStop {},
    MessageDelta {
        #[serde(default)]
        delta: Option<WireMessageDelta>,
        #[serde(default)]
        usage: Option<WireUsage>,
    },
    MessageStop {},
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireDelta {
    TextDelta { text: String },
    InputJsonDelta { partial_json: String },
    ThinkingDelta { thinking: String },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct WireMessageDelta {
    #[serde(default)]
    stop_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct WireUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
    #[serde(default)]
    cache_read_input_tokens: u32,
    #[serde(default)]
    cache_creation_input_tokens: u32,
}

/// Parse an SSE byte stream into the ring. Works with both `&[u8]` slices
/// (tests) and live reqwest byte streams (production via `parse_stream_into_ring`).
///
/// # Errors
/// Propagates I/O or ring errors. Malformed JSON inside an `event` is reported
/// as `Sse(String)`.
pub async fn parse_into_ring<R>(reader: R, ring: &Ring) -> Result<(), StreamingError>
where
    R: tokio::io::AsyncRead + Unpin + Send,
{
    use eventsource_stream::Eventsource;
    let stream = ReaderStream::new(reader).eventsource();
    pin_utils::pin_mut!(stream);

    while let Some(item) = stream.next().await {
        let ev = item.map_err(|e| StreamingError::Sse(e.to_string()))?;
        // The fixture format puts the JSON in `data`.
        let raw = ev.data;
        if raw.is_empty() {
            continue;
        }
        let parsed: WireEvent = serde_json::from_str(&raw)
            .map_err(|e| StreamingError::Sse(format!("json: {e}; raw={raw}")))?;
        match parsed {
            WireEvent::ContentBlockDelta { delta: WireDelta::TextDelta { text }, .. } => {
                ring.publish(&TokenEvent::new(TokenKind::TextDelta, text.into_bytes()))?;
            }
            WireEvent::ContentBlockDelta { delta: WireDelta::InputJsonDelta { partial_json }, .. } => {
                ring.publish(&TokenEvent::new(TokenKind::ToolUseDelta, partial_json.into_bytes()))?;
            }
            WireEvent::ContentBlockDelta { delta: WireDelta::ThinkingDelta { thinking }, .. } => {
                ring.publish(&TokenEvent::new(TokenKind::ThinkingDelta, thinking.into_bytes()))?;
            }
            WireEvent::MessageDelta { delta, usage } => {
                if let Some(d) = delta {
                    if d.stop_reason.is_some() {
                        ring.publish(&TokenEvent::new(TokenKind::TurnEnd, Vec::new()))?;
                    }
                }
                if let Some(u) = usage {
                    let payload = encode_usage(&u);
                    ring.publish(&TokenEvent::new(TokenKind::Usage, payload))?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn encode_usage(u: &WireUsage) -> Vec<u8> {
    // 4 × u32 BE. Matches order: input, output, cache_read, cache_creation.
    let mut out = Vec::with_capacity(16);
    out.extend_from_slice(&u.input_tokens.to_be_bytes());
    out.extend_from_slice(&u.output_tokens.to_be_bytes());
    out.extend_from_slice(&u.cache_read_input_tokens.to_be_bytes());
    out.extend_from_slice(&u.cache_creation_input_tokens.to_be_bytes());
    out
}
```

- [ ] **Step 6: Add `pin_utils` and `tokio-util` deps**

Edit `crates/origin-provider-anthropic/Cargo.toml`:

```toml
pin-utils = "0.1"
tokio-util = { version = "0.7", features = ["io"] }
```

- [ ] **Step 7: Expose module** in `crates/origin-provider-anthropic/src/lib.rs`

At the top, after the existing `mod wire;` line, add:

```rust
pub mod streaming;
```

- [ ] **Step 8: Run — expected pass**

Run: `cargo test -p origin-provider-anthropic`
Expected: existing non-streaming tests pass + new `streaming::fixture_emits_two_text_deltas_then_turn_end` passes.

- [ ] **Step 9: Verification gate**

- `cargo test -p origin-provider-anthropic`
- `cargo clippy -p origin-provider-anthropic --all-targets -- -D warnings`
- `cargo fmt --check`

If `eventsource-stream`'s MSRV is too new, pin per the reflex (`cargo update -p eventsource-stream --precise <ver>`).

- [ ] **Step 10: Commit**

```bash
git add crates/origin-provider-anthropic/
git commit -m "feat(origin-provider-anthropic): SSE streaming parser → token ring (N4.4)"
```

---

## Task P2.8 — Provider trait: add `chat_stream`

**Files:**
- Modify: `crates/origin-provider/src/lib.rs`
- Modify: `crates/origin-provider/tests/trait.rs`
- Modify: `crates/origin-provider-anthropic/src/lib.rs`
- Modify: `crates/origin-provider-anthropic/Cargo.toml`
- Modify: `crates/origin-provider/Cargo.toml`
- Create: `crates/origin-provider-anthropic/tests/streaming_e2e.rs` (mock server)

- [ ] **Step 1: Extend trait** test in `crates/origin-provider/tests/trait.rs`

Append a new test:

```rust
use origin_stream::{Ring, TokenKind};

#[tokio::test]
async fn fake_provider_streams_one_token() {
    struct StreamProv;
    #[async_trait::async_trait]
    impl Provider for StreamProv {
        fn name(&self) -> &'static str { "stream" }
        async fn chat(&self, _: ChatRequest) -> Result<ChatResponse, ProviderError> {
            Err(ProviderError::Api("non-streaming not supported in this test".into()))
        }
        async fn chat_stream(&self, _: ChatRequest, ring: &Ring) -> Result<(), ProviderError> {
            ring.publish(&origin_stream::TokenEvent::new(TokenKind::TextDelta, b"hi".to_vec()))
                .map_err(|e| ProviderError::Api(e.to_string()))?;
            ring.publish(&origin_stream::TokenEvent::new(TokenKind::TurnEnd, Vec::new()))
                .map_err(|e| ProviderError::Api(e.to_string()))?;
            ring.close();
            Ok(())
        }
    }

    let p = StreamProv;
    let ring = Ring::with_capacity(64 * 1024);
    let mut sub = ring.subscribe();
    p.chat_stream(ChatRequest {
        system: String::new(),
        messages: vec![],
        model: "stream-1".into(),
        tools: vec![],
    }, &ring).await.expect("stream");

    let mut got = Vec::new();
    while let Some(ev) = sub.next().await.expect("recv") {
        got.push(ev);
    }
    assert_eq!(got.len(), 2);
    assert_eq!(got[0].kind(), TokenKind::TextDelta);
    assert_eq!(got[1].kind(), TokenKind::TurnEnd);
}
```

- [ ] **Step 2: Add origin-stream dep** to `crates/origin-provider/Cargo.toml`

```toml
origin-stream = { path = "../origin-stream" }
```

And `[dev-dependencies]`:

```toml
origin-stream = { path = "../origin-stream" }
```

Use the same `origin-stream` path under both blocks (Cargo will dedupe).

- [ ] **Step 3: Run — expected fail**

Run: `cargo test -p origin-provider`
Expected: compile error — `chat_stream` method not in trait.

- [ ] **Step 4: Modify the trait** in `crates/origin-provider/src/lib.rs`

Replace the existing trait block with:

```rust
#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    fn name(&self) -> &'static str;

    /// Send a single non-streaming chat request.
    ///
    /// # Errors
    /// Returns `ProviderError` for transport, API, auth, or rate-limit failures.
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ProviderError>;

    /// Stream tokens into `ring`. Default impl falls back to `chat` and emits
    /// one `TextDelta` + `TurnEnd` so providers without native streaming still
    /// work behind the ring API.
    ///
    /// # Errors
    /// Returns `ProviderError` for transport, API, auth, or rate-limit failures.
    async fn chat_stream(
        &self,
        req: ChatRequest,
        ring: &origin_stream::Ring,
    ) -> Result<(), ProviderError> {
        let resp = self.chat(req).await?;
        let text: String = resp
            .assistant
            .blocks
            .iter()
            .filter_map(|b| match b {
                origin_core::types::Block::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect();
        ring.publish(&origin_stream::TokenEvent::new(
            origin_stream::TokenKind::TextDelta,
            text.into_bytes(),
        ))
        .map_err(|e| ProviderError::Api(e.to_string()))?;
        ring.publish(&origin_stream::TokenEvent::new(
            origin_stream::TokenKind::TurnEnd,
            Vec::new(),
        ))
        .map_err(|e| ProviderError::Api(e.to_string()))?;
        ring.close();
        Ok(())
    }
}
```

- [ ] **Step 5: Implement native `chat_stream`** for `Anthropic`

In `crates/origin-provider-anthropic/src/lib.rs`, append an `impl` for the streaming override that invokes the SSE parser. Insert this method inside the existing `impl Provider for Anthropic` block (right after `chat`):

```rust
    async fn chat_stream(
        &self,
        req: ChatRequest,
        ring: &origin_stream::Ring,
    ) -> Result<(), ProviderError> {
        let wire_messages = req.messages.iter().map(message_to_wire).collect::<Vec<_>>();
        let wire_tools = req
            .tools
            .iter()
            .map(|t| wire::WireTool {
                name: &t.name,
                description: &t.description,
                input_schema: serde_json::from_str(&t.input_schema_json).unwrap_or_else(|_| json!({})),
            })
            .collect::<Vec<_>>();

        let body = wire::WireRequest {
            model: &req.model,
            max_tokens: DEFAULT_MAX_TOKENS,
            system: if req.system.is_empty() { None } else { Some(req.system.as_str()) },
            messages: wire_messages,
            tools: wire_tools,
        };

        let url = format!("{}/v1/messages", self.base);
        let resp = self
            .client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .json(&serde_json::json!({
                "model": body.model,
                "max_tokens": body.max_tokens,
                "system": body.system,
                "messages": body.messages,
                "tools": body.tools,
                "stream": true,
            }))
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Api(format!("status {status}: {text}")));
        }

        let byte_stream = resp.bytes_stream();
        let async_read = tokio_util::io::StreamReader::new(
            byte_stream.map(|r| r.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))),
        );
        streaming::parse_into_ring(async_read, ring)
            .await
            .map_err(|e| ProviderError::Api(e.to_string()))?;
        ring.close();
        Ok(())
    }
```

You'll also need at the top of `lib.rs`:

```rust
use futures_util::StreamExt;
use origin_stream as _; // ensure the path dep stays even if otherwise unused
```

- [ ] **Step 6: Wiremock streaming e2e** at `crates/origin-provider-anthropic/tests/streaming_e2e.rs`

```rust
use origin_core::types::{Block, Message, Role};
use origin_provider::{ChatRequest, Provider};
use origin_provider_anthropic::Anthropic;
use origin_stream::{Ring, TokenKind};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn anthropic_streams_text_then_turn_end() {
    let server = MockServer::start().await;
    let sse_body = std::fs::read(format!(
        "{}/tests/fixtures/sse_hello.txt",
        env!("CARGO_MANIFEST_DIR")
    ))
    .expect("fixture");

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("anthropic-version", "2023-06-01"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(sse_body, "text/event-stream"),
        )
        .mount(&server)
        .await;

    let prov = Anthropic::with_base_url("test-key", &server.uri());
    let ring = Ring::with_capacity(64 * 1024);
    let mut sub = ring.subscribe();

    let r = ring.clone();
    let prov_handle = tokio::spawn(async move {
        prov.chat_stream(
            ChatRequest {
                system: String::new(),
                messages: vec![Message::new(Role::User).with_block(Block::text("hi"))],
                model: "claude-test".into(),
                tools: vec![],
            },
            &r,
        )
        .await
        .expect("stream");
    });

    let mut text = String::new();
    let mut saw_turn_end = false;
    while let Some(ev) = sub.next().await.expect("recv") {
        match ev.kind() {
            TokenKind::TextDelta => text.push_str(std::str::from_utf8(ev.payload()).expect("utf8")),
            TokenKind::TurnEnd => saw_turn_end = true,
            _ => {}
        }
    }
    prov_handle.await.expect("prov task");
    assert_eq!(text, "Hello!");
    assert!(saw_turn_end);
}
```

- [ ] **Step 7: Run — expected pass**

Run: `cargo test -p origin-provider-anthropic`
Expected: existing non-streaming + new streaming_e2e pass.

- [ ] **Step 8: Workspace check**

Run: `cargo test --workspace`
Expected: pass.

- [ ] **Step 9: Verification gate**

- `cargo test --workspace`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo fmt --check`

- [ ] **Step 10: Commit**

```bash
git add crates/origin-provider/ crates/origin-provider-anthropic/
git commit -m "feat(origin-provider): chat_stream trait method + Anthropic native stream"
```

---

## Task P2.9 — Messages carry handles (N2.4 step 1: outbound tool results in CAS)

**Files:**
- Modify: `crates/origin-daemon/Cargo.toml`
- Modify: `crates/origin-daemon/src/agent.rs`
- Modify: `crates/origin-daemon/src/main.rs`
- Modify: `crates/origin-daemon/tests/loop_e2e.rs`
- Modify: `crates/origin-provider-anthropic/src/lib.rs` (handle expansion on outbound wire encode)

- [ ] **Step 1: Add origin-cas dep**

Edit `crates/origin-daemon/Cargo.toml`:

```toml
origin-cas = { path = "../origin-cas" }
```

Also add to `origin-provider-anthropic/Cargo.toml`:

```toml
origin-cas = { path = "../origin-cas" }
```

- [ ] **Step 2: Failing test** — extend `loop_e2e.rs`

Look at existing `crates/origin-daemon/tests/loop_e2e.rs`. Add a new test that asserts when a tool runs, the resulting `Block::ToolResult` carries `handle: Some(_)` and `inline: None`, and the CAS store has the bytes addressable by that handle:

```rust
use origin_cas::{Hash, Store, StoreConfig};
use origin_core::types::Block;
use origin_daemon::agent::{run_loop, LoopOptions};
use origin_daemon::session::Session;
use origin_permission::prompt::AlwaysAllow;
use std::sync::Arc;
use tempfile::tempdir;

// FakeProvider with a stubbed tool_use → tool_result round-trip is already
// imported in this test file from earlier tasks; reuse it.
mod fixtures;

#[tokio::test]
async fn tool_result_goes_to_cas_and_block_carries_handle_only() {
    let dir = tempdir().expect("tempdir");
    let store = Arc::new(
        Store::open(StoreConfig {
            root: dir.path().to_path_buf(),
            hot_capacity: 32,
            warm_pack_target_bytes: 1024 * 1024,
            cold_zstd_level: 3,
        })
        .expect("store"),
    );

    let provider = fixtures::TwoTurnReadProvider::new();
    let mut session = Session::new("fake", "fake-1");
    let opts = LoopOptions::default();

    let summary = run_loop(
        &mut session,
        "please read /etc/hostname",
        &provider,
        &AlwaysAllow,
        opts,
    )
    .with_cas(Arc::clone(&store))   // builder method added below
    .await
    .expect("loop");

    let tool_msg = session
        .messages
        .iter()
        .find(|m| matches!(m.role, origin_core::types::Role::Tool))
        .expect("tool message present");
    let block = tool_msg.blocks.first().expect("at least one block");
    match block {
        Block::ToolResult { handle: Some(h), inline: None, .. } => {
            let h = Hash::from_bytes(*h);
            let bytes = store.get(h).expect("get").expect("present");
            assert!(!bytes.is_empty(), "tool output stored in CAS");
        }
        other => panic!("expected ToolResult with handle Some / inline None, got {other:?}"),
    }
    assert_eq!(summary.turns, 2);
}
```

The `with_cas` builder will be added next.

- [ ] **Step 3: Run — expected fail**

Run: `cargo test -p origin-daemon`
Expected: compile error — `with_cas` / `LoopOptions` does not carry a CAS handle.

- [ ] **Step 4: Modify `agent.rs`** to accept a CAS store and emit handles

In `crates/origin-daemon/src/agent.rs`, change `LoopOptions` and `run_loop` to take a CAS store. Replace the existing `LoopOptions` block:

```rust
use origin_cas::{Hash, Store};
use std::sync::Arc;

#[derive(Clone)]
pub struct LoopOptions {
    pub max_turns: u32,
    pub cas: Option<Arc<Store>>,
}

impl Default for LoopOptions {
    fn default() -> Self {
        Self { max_turns: 25, cas: None }
    }
}

impl LoopOptions {
    #[must_use]
    pub fn with_cas(mut self, store: Arc<Store>) -> Self {
        self.cas = Some(store);
        self
    }
}
```

Inside the loop body, **replace** the `tool_results.push(Block::ToolResult { … inline: Some(...) … })` arm with:

```rust
            let result_bytes = result_text.into_bytes();
            let block = if let Some(cas) = opts.cas.as_ref() {
                let h: Hash = cas.put(&result_bytes).map_err(|e| LoopError::ToolFailure(e.to_string()))?;
                Block::ToolResult {
                    tool_use_id: id,
                    handle: Some(*h.as_bytes()),
                    inline: None,
                    cache_marker: None,
                }
            } else {
                Block::ToolResult {
                    tool_use_id: id,
                    handle: None,
                    inline: Some(result_bytes),
                    cache_marker: None,
                }
            };
            tool_results.push(block);
```

`opts` needs to be in scope — change the `run_loop` signature from `opts: LoopOptions` (currently `Copy`) to `opts: &LoopOptions`. Update the existing `for turn in 1..=opts.max_turns` to `for turn in 1..=opts.max_turns` (no change), and the existing callsite in `main.rs` (Step 6) to pass `&opts`.

Add a new error variant to `LoopError` for CAS failures already covered via `ToolFailure(String)`.

- [ ] **Step 5: Update Anthropic outbound wire encode** to expand handles back to inline bytes

In `crates/origin-provider-anthropic/src/lib.rs`, the existing `block_to_wire` discards `handle` and uses only `inline`. For Phase 2 we still ship inline bytes on the wire (zero-copy IPC handoff is deferred to Phase 11), but tool results now usually live in CAS. Add a CAS lookup step:

Change the `Anthropic` struct:

```rust
pub struct Anthropic {
    api_key: String,
    base: String,
    client: reqwest::Client,
    cas: Option<std::sync::Arc<origin_cas::Store>>,
}
```

Update `new` / `with_base_url` defaults to `cas: None` and add a builder:

```rust
impl Anthropic {
    #[must_use]
    pub fn with_cas(mut self, cas: std::sync::Arc<origin_cas::Store>) -> Self {
        self.cas = Some(cas);
        self
    }
}
```

Pass `self.cas.as_ref()` into a new closure variant of `message_to_wire`. Simplest path: walk messages before encoding and inline-expand any `ToolResult { handle: Some(h), inline: None, .. }` by reading the bytes from CAS into a freshly-allocated `Vec<u8>` we keep alive for the request's lifetime.

Add this helper at the bottom of `lib.rs`:

```rust
fn expand_messages_for_wire(
    messages: &[Message],
    cas: Option<&std::sync::Arc<origin_cas::Store>>,
) -> Result<Vec<Message>, ProviderError> {
    let mut out = Vec::with_capacity(messages.len());
    for m in messages {
        let mut blocks = Vec::with_capacity(m.blocks.len());
        for b in &m.blocks {
            if let Block::ToolResult { tool_use_id, handle: Some(h), inline: None, cache_marker } = b {
                let store = cas.ok_or_else(|| {
                    ProviderError::Api("ToolResult handle present but no CAS configured".into())
                })?;
                let bytes = store
                    .get(origin_cas::Hash::from_bytes(*h))
                    .map_err(|e| ProviderError::Api(format!("cas get: {e}")))?
                    .ok_or_else(|| ProviderError::Api("cas miss for tool result handle".into()))?;
                blocks.push(Block::ToolResult {
                    tool_use_id: tool_use_id.clone(),
                    handle: None,
                    inline: Some(bytes),
                    cache_marker: *cache_marker,
                });
            } else {
                blocks.push(b.clone());
            }
        }
        out.push(Message { role: m.role, blocks });
    }
    Ok(out)
}
```

In both `chat` and `chat_stream`, before building `wire_messages`, call:

```rust
let expanded = expand_messages_for_wire(&req.messages, self.cas.as_ref())?;
let wire_messages = expanded.iter().map(message_to_wire).collect::<Vec<_>>();
```

- [ ] **Step 6: Wire the daemon** in `crates/origin-daemon/src/main.rs`

Construct the CAS store and pass it both to the provider and to `run_loop`:

```rust
let cas_root = env::var("ORIGIN_CAS_ROOT").unwrap_or_else(|_| default_cas_root());
let cas = std::sync::Arc::new(origin_cas::Store::open(origin_cas::StoreConfig {
    root: cas_root.clone().into(),
    hot_capacity: 256,
    warm_pack_target_bytes: 4 * 1024 * 1024,
    cold_zstd_level: 3,
}).map_err(|e| anyhow::anyhow!("cas open: {e}"))?);
info!(cas_root = %cas_root, "cas store ready");

let provider = std::sync::Arc::new(Anthropic::new(api_key).with_cas(std::sync::Arc::clone(&cas)));
```

And add:

```rust
fn default_cas_root() -> String {
    let mut p = std::env::temp_dir();
    p.push("origin-cas");
    p.to_string_lossy().into_owned()
}
```

Change the `run_loop` call site to:

```rust
let opts = LoopOptions::default().with_cas(std::sync::Arc::clone(&cas));
match run_loop(
    &mut session,
    &req.user_text,
    provider.as_ref(),
    &AlwaysAllow,
    &opts,
)
```

- [ ] **Step 7: Run — expected pass**

Run: `cargo test --workspace`
Expected: existing tests + new CAS-handle test pass.

- [ ] **Step 8: Verification gate**

- `cargo test --workspace`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo fmt --check`

- [ ] **Step 9: Commit**

```bash
git add crates/origin-daemon/ crates/origin-provider-anthropic/
git commit -m "feat(origin-daemon): tool outputs land in CAS; ToolResult carries handle (N2.4 step 1)"
```

---

## Task P2.10 — TUI streams tokens live from the ring

**Files:**
- Create: `crates/origin-daemon/src/stream_relay.rs`
- Modify: `crates/origin-daemon/src/lib.rs`
- Modify: `crates/origin-daemon/src/main.rs`
- Modify: `crates/origin-cli/Cargo.toml`
- Modify: `crates/origin-cli/src/main.rs`
- Modify: `crates/origin-cli/src/tui.rs`
- Create: `crates/origin-daemon/tests/stream_e2e.rs`

The strategy: daemon spawns a relay task that subscribes to the per-request ring and pushes each `TokenEvent` as an IPC `Event` frame to the connected CLI. The CLI parses these frames and updates the live "current assistant turn" buffer in `App`. When `TurnEnd` arrives, the final `Response` frame closes the request.

- [ ] **Step 1: Failing test** at `crates/origin-daemon/tests/stream_e2e.rs`

```rust
use origin_core::types::{Block, Message, Role};
use origin_daemon::protocol::{PromptReply, PromptRequest, StreamEvent};
use origin_daemon::stream_relay::relay_to_connection;
use origin_ipc::frame::{validate, FrameKind};
use origin_ipc::transport::{Connector, Listener};
use origin_stream::{Ring, TokenEvent, TokenKind};

#[tokio::test]
async fn relay_publishes_token_events_as_event_frames() {
    let path = unique_path("relay");
    let listener = Listener::bind(&path).await.expect("bind");
    let server_task = tokio::spawn(async move {
        let conn = listener.accept().await.expect("accept");
        // Producer side: publish three events into a ring, relay them.
        let ring = Ring::with_capacity(64 * 1024);
        let r = ring.clone();
        let p = tokio::spawn(async move {
            ring.publish(&TokenEvent::new(TokenKind::TextDelta, b"Hel".to_vec())).expect("p1");
            ring.publish(&TokenEvent::new(TokenKind::TextDelta, b"lo".to_vec())).expect("p2");
            ring.publish(&TokenEvent::new(TokenKind::TurnEnd, Vec::new())).expect("p3");
            ring.close();
        });
        relay_to_connection(&r, conn).await.expect("relay");
        p.await.expect("producer");
    });

    let mut client = Connector::connect(&path).await.expect("connect");
    let mut got_text = String::new();
    let mut saw_turn_end = false;
    loop {
        let body = client.read_frame_body().await.expect("read");
        // Wrap into a Frame for kind inspection by re-encoding through validate.
        // Simpler: parse the body directly as a StreamEvent JSON.
        let ev: StreamEvent = serde_json::from_slice(&body).expect("decode event");
        match ev {
            StreamEvent::TextDelta { text } => got_text.push_str(&text),
            StreamEvent::TurnEnd => { saw_turn_end = true; break; }
            _ => {}
        }
    }
    assert_eq!(got_text, "Hello");
    assert!(saw_turn_end);
    server_task.await.expect("server");
}

fn unique_path(label: &str) -> String {
    let pid = std::process::id();
    let nano = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    #[cfg(windows)]
    { format!(r"\\.\pipe\origin-test-{label}-{pid}-{nano}") }
    #[cfg(unix)]
    { format!("{}/origin-test-{label}-{pid}-{nano}.sock", std::env::temp_dir().display()) }
}
```

- [ ] **Step 2: Run — expected fail**

Run: `cargo test -p origin-daemon --test stream_e2e`
Expected: compile error — `protocol::StreamEvent` and `stream_relay::relay_to_connection` don't exist.

- [ ] **Step 3: Extend the protocol** at `crates/origin-daemon/src/protocol.rs`

Append:

```rust
/// One in-flight event during a streaming response. Encoded as JSON inside
/// an IPC `Event` frame body so the CLI can decode without depending on rkyv.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StreamEvent {
    TextDelta { text: String },
    ToolUseDelta { partial_json: String },
    ThinkingDelta { thinking: String },
    Usage {
        input_tokens: u32,
        output_tokens: u32,
        cache_read_input_tokens: u32,
        cache_creation_input_tokens: u32,
    },
    TurnEnd,
}
```

- [ ] **Step 4: Implement relay** at `crates/origin-daemon/src/stream_relay.rs`

```rust
//! Bridge a per-request `Ring` to an `origin-ipc` connection as JSON-encoded
//! `Event` frames.

use crate::protocol::StreamEvent;
use origin_ipc::frame::FrameKind;
use origin_ipc::transport::Connection;
use origin_stream::{Ring, RingError, TokenKind};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RelayError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("ring: {0}")]
    Ring(#[from] RingError),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

/// Subscribe to `ring`, translate each `TokenEvent` into a JSON `StreamEvent`,
/// write each as an `Event` frame. Returns when the ring closes.
///
/// # Errors
/// Propagates I/O, ring decode, or JSON errors.
pub async fn relay_to_connection(ring: &Ring, mut conn: Connection) -> Result<(), RelayError> {
    let mut sub = ring.subscribe();
    while let Some(tev) = sub.next().await? {
        let sev = match tev.kind() {
            TokenKind::TextDelta => StreamEvent::TextDelta {
                text: String::from_utf8_lossy(tev.payload()).into_owned(),
            },
            TokenKind::ToolUseDelta => StreamEvent::ToolUseDelta {
                partial_json: String::from_utf8_lossy(tev.payload()).into_owned(),
            },
            TokenKind::ThinkingDelta => StreamEvent::ThinkingDelta {
                thinking: String::from_utf8_lossy(tev.payload()).into_owned(),
            },
            TokenKind::Usage => {
                let p = tev.payload();
                if p.len() != 16 {
                    continue;
                }
                StreamEvent::Usage {
                    input_tokens: u32::from_be_bytes(p[0..4].try_into().expect("4")),
                    output_tokens: u32::from_be_bytes(p[4..8].try_into().expect("4")),
                    cache_read_input_tokens: u32::from_be_bytes(p[8..12].try_into().expect("4")),
                    cache_creation_input_tokens: u32::from_be_bytes(p[12..16].try_into().expect("4")),
                }
            }
            TokenKind::TurnEnd => StreamEvent::TurnEnd,
        };
        let body = serde_json::to_vec(&sev)?;
        conn.write_frame(FrameKind::Event, &body).await?;
    }
    Ok(())
}
```

- [ ] **Step 5: Export from lib** in `crates/origin-daemon/src/lib.rs`

Add:

```rust
pub mod stream_relay;
```

- [ ] **Step 6: Run — relay test pass**

Run: `cargo test -p origin-daemon --test stream_e2e`
Expected: pass.

- [ ] **Step 7: Wire the relay into `main.rs`**

The daemon should now run the agent loop with a streaming provider. For Phase 2 we keep the agent loop *non-streaming* (it still calls `chat`) but **also** stream the *final assistant text* through the ring before sending the JSON `Response`. This gives the CLI live progress while we ship the full message-history persistence semantics from P1.

Actually — better: have the daemon spawn a parallel task: while `run_loop` is in flight, the ring receives "stub" deltas? No — the cleaner Phase 2 increment is:

- Replace `provider.chat(...)` calls inside `run_loop` with `provider.chat_stream(...)`. The loop subscribes to its own ring, accumulates the assistant message (text + tool_use blocks), and returns a synthesized `ChatResponse` when `TurnEnd` arrives. The same ring is *also* relayed to the CLI.

Implement this in **Step 7a–7d**:

- [ ] **Step 7a: Modify agent loop to drive streaming**

In `crates/origin-daemon/src/agent.rs`, replace the `provider.chat(req).await?` call with:

```rust
        let ring = origin_stream::Ring::with_capacity(256 * 1024);
        if let Some(tx) = &opts.relay_tx {
            // Send the ring to the relay task; ignore send errors (CLI disconnected).
            let _ = tx.send(ring.clone()).await;
        }
        provider.chat_stream(req, &ring).await?;
        // Drain the ring into a ChatResponse for the existing tool-dispatch logic.
        let resp = drain_ring_into_response(&ring).await?;
```

Add the helper:

```rust
async fn drain_ring_into_response(ring: &origin_stream::Ring) -> Result<origin_provider::ChatResponse, LoopError> {
    let mut sub = ring.subscribe();
    let mut text = String::new();
    let mut tool_use_buf: Option<(String, String, String)> = None; // (id, name, partial_json)
    let mut blocks: Vec<Block> = Vec::new();
    let mut usage = origin_provider::Usage::default();

    while let Some(ev) = sub.next().await.map_err(|e| LoopError::ToolFailure(e.to_string()))? {
        match ev.kind() {
            origin_stream::TokenKind::TextDelta => {
                text.push_str(&String::from_utf8_lossy(ev.payload()));
            }
            origin_stream::TokenKind::ToolUseDelta => {
                // For Phase 2 we accept that tool_use streaming is opaque
                // (full block arrives in a single delta in typical Anthropic
                // streams). Phase 3 adds proper incremental JSON parsing.
                if let Some((_, _, ref mut partial)) = tool_use_buf {
                    partial.push_str(&String::from_utf8_lossy(ev.payload()));
                } else {
                    let _ = tool_use_buf.insert(("".into(), "".into(),
                        String::from_utf8_lossy(ev.payload()).into_owned()));
                }
            }
            origin_stream::TokenKind::Usage => {
                let p = ev.payload();
                if p.len() == 16 {
                    usage = origin_provider::Usage {
                        input_tokens: u32::from_be_bytes(p[0..4].try_into().expect("4")),
                        output_tokens: u32::from_be_bytes(p[4..8].try_into().expect("4")),
                        cache_read_input_tokens: u32::from_be_bytes(p[8..12].try_into().expect("4")),
                        cache_creation_input_tokens: u32::from_be_bytes(p[12..16].try_into().expect("4")),
                    };
                }
            }
            origin_stream::TokenKind::TurnEnd => break,
            origin_stream::TokenKind::ThinkingDelta => {}
        }
    }
    if !text.is_empty() {
        blocks.push(Block::Text { text, cache_marker: None });
    }
    let assistant = Message { role: Role::Assistant, blocks };
    Ok(origin_provider::ChatResponse { assistant, usage })
}
```

> **Phase 2 carve-out:** the streaming-driven loop here only handles `TextDelta` correctly. `ToolUseDelta` accumulation as a single partial_json block is intentionally crude — Phase 3 (P3.3) adds the incremental JSON parser and proper `ToolUse` block reconstruction. Until then, tool-use turns will be exercised by tests via the *non-streaming* path (fall back to `provider.chat()` when `opts.streaming_disabled` is set). Add this option:

Update `LoopOptions`:

```rust
#[derive(Clone)]
pub struct LoopOptions {
    pub max_turns: u32,
    pub cas: Option<Arc<Store>>,
    pub relay_tx: Option<tokio::sync::mpsc::Sender<origin_stream::Ring>>,
    pub streaming_disabled: bool,
}

impl Default for LoopOptions {
    fn default() -> Self {
        Self { max_turns: 25, cas: None, relay_tx: None, streaming_disabled: false }
    }
}

impl LoopOptions {
    #[must_use]
    pub fn with_cas(mut self, s: Arc<Store>) -> Self { self.cas = Some(s); self }
    #[must_use]
    pub fn with_relay(mut self, tx: tokio::sync::mpsc::Sender<origin_stream::Ring>) -> Self { self.relay_tx = Some(tx); self }
    #[must_use]
    pub const fn without_streaming(mut self) -> Self { self.streaming_disabled = true; self }
}
```

Branch in the loop:

```rust
        let resp = if opts.streaming_disabled {
            provider.chat(req).await?
        } else {
            let ring = origin_stream::Ring::with_capacity(256 * 1024);
            if let Some(tx) = &opts.relay_tx {
                let _ = tx.send(ring.clone()).await;
            }
            provider.chat_stream(req, &ring).await?;
            drain_ring_into_response(&ring).await?
        };
```

Tests that involve tool dispatch (fakes constructed in P1's loop_e2e) should opt out of streaming via `LoopOptions::default().with_cas(...).without_streaming()` until P3 lands the proper tool_use parser.

- [ ] **Step 7b: Update existing loop_e2e tests** to call `.without_streaming()`

Run: `cargo test -p origin-daemon` — see which existing tests break (they expect fake provider's `chat` to be hit). For each failing test, append `.without_streaming()` to the `LoopOptions::default()`.

- [ ] **Step 7c: Wire the relay channel in `main.rs`**

For each accepted connection, spawn the relay task:

```rust
let (tx_ring, mut rx_ring) = tokio::sync::mpsc::channel::<origin_stream::Ring>(1);

let conn_for_relay = conn.try_clone().await.expect("clone conn"); // see Step 7d below
let relay_handle = tokio::spawn(async move {
    if let Some(ring) = rx_ring.recv().await {
        let _ = origin_daemon::stream_relay::relay_to_connection(&ring, conn_for_relay).await;
    }
});

let opts = LoopOptions::default()
    .with_cas(std::sync::Arc::clone(&cas))
    .with_relay(tx_ring.clone());

// existing run_loop call — pass &opts
```

- [ ] **Step 7d: `origin-ipc` `Connection::try_clone`**

`interprocess::local_socket::tokio::Stream` does not implement `Clone`. The cleanest fix is to NOT clone the connection — relay writes through the same `conn` the main handler uses, sequentially: relay produces events while the agent loop is running, then the handler writes the final `Response` frame after `run_loop` returns. Wrap `conn` in `Arc<tokio::sync::Mutex<Connection>>` and pass clones of the Arc to both the relay and the handler.

Add to `origin-ipc/src/transport.rs`:

```rust
use std::sync::Arc;
use tokio::sync::Mutex;

/// Shared, mutex-protected handle. Use when multiple writers must serialize.
pub type SharedConnection = Arc<Mutex<Connection>>;
```

Modify the daemon to wrap `conn` in `Arc<Mutex<Connection>>` and update `relay_to_connection` to accept `SharedConnection`:

```rust
pub async fn relay_to_connection(ring: &Ring, conn: origin_ipc::transport::SharedConnection) -> Result<(), RelayError> {
    let mut sub = ring.subscribe();
    while let Some(tev) = sub.next().await? {
        let sev = ...; // unchanged
        let body = serde_json::to_vec(&sev)?;
        let mut guard = conn.lock().await;
        guard.write_frame(FrameKind::Event, &body).await?;
    }
    Ok(())
}
```

Update the stream_e2e test to pass `Arc::new(Mutex::new(conn))`.

- [ ] **Step 8: Update CLI to consume `Event` frames**

Edit `crates/origin-cli/src/main.rs`. Change `call_daemon` to:

```rust
async fn call_daemon(
    path: &str,
    model: &str,
    user_text: &str,
    on_delta: impl FnMut(&str),
) -> Result<PromptReply> {
    let mut on_delta = on_delta;
    let mut client = Connector::connect(path).await?;
    let body = serde_json::to_vec(&PromptRequest { system: "", model, user_text })?;
    let frame = encode(1, FrameKind::Request, &body);
    client.write_raw(&frame).await?;

    loop {
        let body = client.read_frame_body().await?;
        // First try as StreamEvent; if that fails, parse as PromptReply.
        if let Ok(ev) = serde_json::from_slice::<StreamEvent>(&body) {
            match ev {
                StreamEvent::TextDelta { text } => on_delta(&text),
                StreamEvent::TurnEnd => continue,
                _ => {}
            }
            continue;
        }
        let reply: PromptReply = serde_json::from_slice(&body)?;
        return Ok(reply);
    }
}
```

Add an `origin-daemon` path dep to `origin-cli/Cargo.toml` so the CLI can reference `StreamEvent`:

```toml
origin-daemon = { path = "../origin-daemon" }
```

Or, cleaner: duplicate the `StreamEvent` enum into a new `origin-daemon/src/protocol.rs` re-export crate-public + add the same as a CLI-private mirror to avoid coupling. For Phase 2 we accept the path dep — it's not on the runtime path of the CLI binary, only its codegen.

Update the `Submit` branch in `main.rs`:

```rust
InputAction::Submit(text) => {
    app.add_line("you> ", &text);
    app.start_assistant_turn();
    let path_clone = path.clone();
    let reply = call_daemon(&path_clone, &model, &text, |delta| {
        app.append_to_current_assistant(delta);
    })
    .await;
    match reply {
        Ok(r) => app.finalize_assistant_turn(r.turns),
        Err(e) => app.add_line("error> ", &format!("{e}")),
    }
}
```

- [ ] **Step 9: TUI `App` extensions**

Edit `crates/origin-cli/src/tui.rs`. Add fields + methods:

```rust
#[derive(Debug)]
pub struct App {
    pub scrollback: Vec<String>,
    pub input: String,
    pub current_assistant: Option<String>,
}

impl App {
    #[must_use]
    pub const fn new() -> Self {
        Self { scrollback: Vec::new(), input: String::new(), current_assistant: None }
    }

    pub fn add_line(&mut self, prefix: &str, body: &str) {
        self.scrollback.push(format!("{prefix}{body}"));
    }

    pub fn start_assistant_turn(&mut self) {
        self.current_assistant = Some(String::new());
    }

    pub fn append_to_current_assistant(&mut self, delta: &str) {
        if let Some(buf) = &mut self.current_assistant {
            buf.push_str(delta);
        }
    }

    pub fn finalize_assistant_turn(&mut self, turns: u32) {
        if let Some(text) = self.current_assistant.take() {
            self.scrollback.push(format!("origin ({turns} turns)> {text}"));
        }
    }
}
```

Render `current_assistant` (if `Some`) at the bottom of the scrollback area in `draw`:

```rust
pub fn draw(f: &mut Frame<'_>, app: &App) {
    let (main, prompt) = split_main_input(f.area());

    let mut lines: Vec<Line> = app.scrollback.iter().map(|s| Line::from(Span::raw(s.clone()))).collect();
    if let Some(buf) = &app.current_assistant {
        lines.push(Line::from(Span::raw(format!("origin> {buf}"))));
    }
    let scroll = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::ALL).title("origin"));
    f.render_widget(scroll, main);

    let input = Paragraph::new(Line::from(vec![
        Span::raw("> "),
        Span::styled(app.input.clone(), Style::default().add_modifier(Modifier::BOLD)),
    ]))
    .block(Block::default().borders(Borders::ALL));
    f.render_widget(input, prompt);
}
```

> **UI testing carve-out:** The TUI itself is hand-tested. We do unit-test `App` state transitions:

Create `crates/origin-cli/tests/app.rs`:

```rust
use origin_cli::tui::App;

#[test]
fn assistant_turn_lifecycle() {
    let mut app = App::new();
    app.start_assistant_turn();
    app.append_to_current_assistant("Hel");
    app.append_to_current_assistant("lo");
    app.finalize_assistant_turn(2);
    assert!(app.current_assistant.is_none());
    assert!(app.scrollback.iter().any(|l| l == "origin (2 turns)> Hello"));
}
```

- [ ] **Step 10: Run — expected pass**

Run: `cargo test --workspace`
Expected: pass. Test the TUI manually in a follow-up "smoke" — see Step 11.

- [ ] **Step 11: Manual smoke**

In one terminal: `ANTHROPIC_API_KEY=... cargo run -p origin-daemon --release`
In another: `cargo run -p origin-cli --release`, type `hello`, press Enter.
Expected: the assistant text appears incrementally (chunked by SSE deltas) under `origin> …`, then a final `origin (1 turns)> …` line replaces it.

If the response appears all at once, the relay isn't wired correctly — re-check Step 7c (the `tx_ring.send` placement before `provider.chat_stream` is called by the loop).

- [ ] **Step 12: Verification gate**

- `cargo test --workspace`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo fmt --check`

- [ ] **Step 13: Commit**

```bash
git add crates/origin-daemon/ crates/origin-cli/ crates/origin-ipc/
git commit -m "feat(origin-daemon): stream relay; CLI tail consumes deltas live"
```

---

## Task P2.11 — Token + cost panel in status bar

**Files:**
- Create: `crates/origin-cli/src/status.rs`
- Modify: `crates/origin-cli/src/screen.rs`
- Modify: `crates/origin-cli/src/tui.rs`
- Modify: `crates/origin-cli/src/main.rs`

- [ ] **Step 1: Failing test** at `crates/origin-cli/tests/status.rs`

```rust
use origin_cli::status::{render_line, UsageSnapshot};
use std::time::Duration;

#[test]
fn renders_in_out_cache_cost_time() {
    let snap = UsageSnapshot {
        provider: "anthropic",
        model: "claude-opus-4-7",
        input_tokens: 1024,
        output_tokens: 512,
        cache_read_input_tokens: 256,
        cache_creation_input_tokens: 128,
        elapsed: Duration::from_millis(2_345),
    };
    let s = render_line(&snap);
    assert!(s.contains("in 1024"), "got {s}");
    assert!(s.contains("out 512"));
    assert!(s.contains("cache_r 256"));
    assert!(s.contains("cache_w 128"));
    assert!(s.contains("$"));
    assert!(s.contains("2.345s") || s.contains("2.34s"));
}

#[test]
fn cost_is_zero_for_unknown_model() {
    let snap = UsageSnapshot {
        provider: "anthropic",
        model: "unknown-model-xyz",
        input_tokens: 100,
        output_tokens: 100,
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
        elapsed: Duration::from_millis(10),
    };
    let s = render_line(&snap);
    assert!(s.contains("$0.000"));
}
```

- [ ] **Step 2: Run — expected fail**

Run: `cargo test -p origin-cli`
Expected: compile error — `status::render_line` not found.

- [ ] **Step 3: Implement** `crates/origin-cli/src/status.rs`

```rust
//! Status line: live token + cost accounting.
//!
//! Pricing is per-model and per-1M-tokens, sourced from a small lookup table.
//! Unknown models cost zero — better than silently wrong numbers. Phase 8 will
//! externalize pricing.

use std::time::Duration;

#[derive(Debug, Clone)]
pub struct UsageSnapshot {
    pub provider: &'static str,
    pub model: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_input_tokens: u32,
    pub cache_creation_input_tokens: u32,
    pub elapsed: Duration,
}

impl UsageSnapshot {
    #[must_use]
    pub fn new(provider: &'static str, model: impl Into<String>) -> Self {
        Self {
            provider,
            model: model.into(),
            input_tokens: 0,
            output_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            elapsed: Duration::ZERO,
        }
    }
}

/// Per-million-token USD prices. (input, output, cache_read, cache_write).
fn pricing(model: &str) -> (f64, f64, f64, f64) {
    match model {
        // Approximate public pricing snapshot — replace in P8 with config.
        "claude-opus-4-7"   => (15.00, 75.00, 1.50, 18.75),
        "claude-sonnet-4-6" => (3.00,  15.00, 0.30, 3.75),
        "claude-haiku-4-5"  => (0.80,  4.00,  0.08, 1.00),
        _ => (0.0, 0.0, 0.0, 0.0),
    }
}

#[must_use]
pub fn cost_usd(snap: &UsageSnapshot) -> f64 {
    let (pi, po, pcr, pcw) = pricing(&snap.model);
    let m = 1_000_000.0;
    f64::from(snap.input_tokens) / m * pi
        + f64::from(snap.output_tokens) / m * po
        + f64::from(snap.cache_read_input_tokens) / m * pcr
        + f64::from(snap.cache_creation_input_tokens) / m * pcw
}

#[must_use]
pub fn render_line(snap: &UsageSnapshot) -> String {
    let cost = cost_usd(snap);
    let secs = snap.elapsed.as_secs_f64();
    format!(
        "[{}/{}]  in {}  out {}  cache_r {}  cache_w {}  ${:.3}  {:.3}s",
        snap.provider, snap.model,
        snap.input_tokens, snap.output_tokens,
        snap.cache_read_input_tokens, snap.cache_creation_input_tokens,
        cost, secs,
    )
}
```

- [ ] **Step 4: Modify screen layout** to reserve a single-line status bar at the bottom

Edit `crates/origin-cli/src/screen.rs`:

```rust
use ratatui::layout::{Constraint, Direction, Layout, Rect};

#[must_use]
pub fn split_main_input_status(area: Rect) -> (Rect, Rect, Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(3), Constraint::Length(1)])
        .split(area);
    (chunks[0], chunks[1], chunks[2])
}
```

Keep the existing `split_main_input` for back-compat (used by P1's tests).

- [ ] **Step 5: Wire the panel into `tui.rs`**

```rust
use crate::status::{render_line, UsageSnapshot};

#[derive(Debug)]
pub struct App {
    pub scrollback: Vec<String>,
    pub input: String,
    pub current_assistant: Option<String>,
    pub usage: UsageSnapshot,
}

impl App {
    #[must_use]
    pub fn new(provider: &'static str, model: impl Into<String>) -> Self {
        Self {
            scrollback: Vec::new(),
            input: String::new(),
            current_assistant: None,
            usage: UsageSnapshot::new(provider, model),
        }
    }
    // ... existing methods unchanged ...

    pub fn record_usage(&mut self,
        input_tokens: u32, output_tokens: u32,
        cache_read: u32, cache_write: u32,
        elapsed: std::time::Duration)
    {
        self.usage.input_tokens += input_tokens;
        self.usage.output_tokens += output_tokens;
        self.usage.cache_read_input_tokens += cache_read;
        self.usage.cache_creation_input_tokens += cache_write;
        self.usage.elapsed += elapsed;
    }
}

pub fn draw(f: &mut Frame<'_>, app: &App) {
    let (main, prompt, status) = crate::screen::split_main_input_status(f.area());

    let mut lines: Vec<Line> = app.scrollback.iter().map(|s| Line::from(Span::raw(s.clone()))).collect();
    if let Some(buf) = &app.current_assistant {
        lines.push(Line::from(Span::raw(format!("origin> {buf}"))));
    }
    let scroll = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::ALL).title("origin"));
    f.render_widget(scroll, main);

    let input = Paragraph::new(Line::from(vec![
        Span::raw("> "),
        Span::styled(app.input.clone(), Style::default().add_modifier(Modifier::BOLD)),
    ]))
    .block(Block::default().borders(Borders::ALL));
    f.render_widget(input, prompt);

    let s = render_line(&app.usage);
    let status_p = Paragraph::new(Line::from(Span::raw(s)));
    f.render_widget(status_p, status);
}
```

> The previously-existing `App::new()` no-arg constructor is replaced — update any callsite that breaks. Existing TUI test from P2.10 (`app.rs`) becomes `App::new("anthropic", "claude-opus-4-7")`.

- [ ] **Step 6: Pump `Usage` events into the App**

In `crates/origin-cli/src/main.rs`, change `call_daemon` to invoke a second closure on `StreamEvent::Usage`:

```rust
async fn call_daemon(
    path: &str,
    model: &str,
    user_text: &str,
    mut on_delta: impl FnMut(&str),
    mut on_usage: impl FnMut(u32, u32, u32, u32),
) -> Result<PromptReply> {
    let mut client = Connector::connect(path).await?;
    let body = serde_json::to_vec(&PromptRequest { system: "", model, user_text })?;
    let frame = encode(1, FrameKind::Request, &body);
    client.write_raw(&frame).await?;

    loop {
        let body = client.read_frame_body().await?;
        if let Ok(ev) = serde_json::from_slice::<StreamEvent>(&body) {
            match ev {
                StreamEvent::TextDelta { text } => on_delta(&text),
                StreamEvent::Usage { input_tokens, output_tokens, cache_read_input_tokens, cache_creation_input_tokens } => {
                    on_usage(input_tokens, output_tokens, cache_read_input_tokens, cache_creation_input_tokens);
                }
                StreamEvent::TurnEnd => continue,
                _ => {}
            }
            continue;
        }
        let reply: PromptReply = serde_json::from_slice(&body)?;
        return Ok(reply);
    }
}
```

Track elapsed wall-time around the call and feed it via `record_usage`.

- [ ] **Step 7: Run — expected pass**

Run: `cargo test -p origin-cli`
Expected: status tests pass.

- [ ] **Step 8: Workspace + verification gate**

- `cargo test --workspace`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo fmt --check`

- [ ] **Step 9: Commit**

```bash
git add crates/origin-cli/
git commit -m "feat(origin-cli): token / cost status bar driven by stream Usage events"
```

---

## Task P2.12 — Phase 2 checkpoint + RAM soak bench

**Goal:** verify the ring + CAS combo delivers flat RAM over a long synthetic session. Tag `p2-complete` on green.

**Files:**
- Create: `crates/origin-daemon/benches/phase2_ram_soak.rs`
- Modify: `crates/origin-daemon/Cargo.toml`
- Create: `CHANGELOG.md` (or append the existing one) for the Phase 2 entry
- Update: `docs/superpowers/plans/2026-05-19-origin-phase-2.md` — append a "phase-2 outcome" log

- [ ] **Step 1: Add bench config** to `crates/origin-daemon/Cargo.toml`

```toml
[[bench]]
name = "phase2_ram_soak"
harness = false

[dev-dependencies]
# already has tempfile
criterion = { version = "0.5", features = ["html_reports"] }
sysinfo = "0.30"
```

If criterion or sysinfo trip MSRV, pin them.

- [ ] **Step 2: Bench** `crates/origin-daemon/benches/phase2_ram_soak.rs`

```rust
//! Soak bench: synthesise a 1000-message session using a fake provider, all
//! tool outputs land in CAS, the ring is reused. Assert peak RSS stays under
//! 200 MiB (process RSS minus mmap pack-file pages we don't control).
//!
//! Run with: `cargo bench -p origin-daemon --bench phase2_ram_soak`

use origin_cas::{Store, StoreConfig};
use origin_core::types::{Block, Message, Role};
use origin_daemon::agent::{run_loop, LoopOptions};
use origin_daemon::session::Session;
use origin_permission::prompt::AlwaysAllow;
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use std::sync::Arc;
use sysinfo::{Pid, System};
use tempfile::tempdir;

struct FakeNoToolProvider {
    counter: std::sync::atomic::AtomicU32,
}

#[async_trait::async_trait]
impl Provider for FakeNoToolProvider {
    fn name(&self) -> &'static str { "fake" }
    async fn chat(&self, _: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let n = self.counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(ChatResponse {
            assistant: Message::new(Role::Assistant)
                .with_block(Block::text(format!("ack-{n}"))),
            usage: Usage::default(),
        })
    }
}

fn main() {
    let dir = tempdir().expect("tempdir");
    let store = Arc::new(
        Store::open(StoreConfig {
            root: dir.path().to_path_buf(),
            hot_capacity: 64,
            warm_pack_target_bytes: 4 * 1024 * 1024,
            cold_zstd_level: 3,
        })
        .expect("store"),
    );

    let prov = FakeNoToolProvider { counter: std::sync::atomic::AtomicU32::new(0) };
    let opts = LoopOptions::default().with_cas(Arc::clone(&store)).without_streaming();

    let mut sys = System::new();
    let pid = Pid::from_u32(std::process::id());
    sys.refresh_process(pid);
    let baseline_rss = sys.process(pid).map(sysinfo::ProcessExt::memory).unwrap_or(0);

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("rt");
    let mut session = Session::new("fake", "fake-1");

    for i in 0..1000 {
        rt.block_on(run_loop(
            &mut session,
            &format!("user msg {i}"),
            &prov,
            &AlwaysAllow,
            &opts,
        ))
        .expect("turn");
    }

    sys.refresh_process(pid);
    let final_rss = sys.process(pid).map(sysinfo::ProcessExt::memory).unwrap_or(0);
    let delta_mib = final_rss.saturating_sub(baseline_rss) / 1024;

    println!("baseline_rss_kb={baseline_rss}");
    println!("final_rss_kb={final_rss}");
    println!("delta_mib={delta_mib}");
    println!("messages_in_session={}", session.messages.len());

    assert!(delta_mib < 200, "RSS growth {delta_mib} MiB exceeds 200 MiB budget");
}
```

- [ ] **Step 3: Run the bench**

Run: `cargo bench -p origin-daemon --bench phase2_ram_soak`
Expected: completes; the printed `delta_mib` is below 200. If it isn't, look at:
- `Session::messages` — currently grows unbounded. Phase 2 explicitly accepts this; compaction is Phase 5. For the soak to pass we cap `session.messages` growth by virtue of tool-result handles (32 bytes vs N-KiB inline). Confirm the fake provider produces small blocks; if RSS still climbs, drop assistant text length or shrink the loop iteration count.
- CAS Hot LRU not bounded — confirm `hot_capacity: 64` is honoured.

- [ ] **Step 4: Append phase summary** to `CHANGELOG.md`

```markdown
## Phase 2 — Streaming + CAS + Ring Buffer (2026-05-19)

- `origin-cas` crate: blake3 Hash, FastCDC chunker, mmap pack files,
  three-tier (Hot LRU + Warm mmap + Cold zstd) Store, refcount/GC enumerator.
- `origin-stream` crate: single-producer multi-tail byte ring;
  rkyv-archived TokenEvent records.
- Anthropic provider gains native SSE → ring streaming.
- `Provider::chat_stream` added to trait; default impl wraps `chat`.
- Daemon: tool outputs land in CAS; `Block::ToolResult` carries 32-byte
  handle, not inline bytes. `expand_messages_for_wire` re-inflates on the
  outbound encode hop.
- CLI: live token streaming into TUI; assistant turn buffer renders
  incrementally; token/cost status bar.
- RAM soak: 1000-msg session, ΔRSS < 200 MiB.
- New SQLite migration V2: `cas_refs(hash, refcount, tier, last_access)`.
```

- [ ] **Step 5: Final verification gate**

All must exit 0:
- `cargo test --workspace`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo fmt --check`
- `cargo bench -p origin-daemon --bench phase2_ram_soak`

- [ ] **Step 6: Commit + tag**

```bash
git add CHANGELOG.md crates/origin-daemon/benches/ crates/origin-daemon/Cargo.toml docs/superpowers/plans/2026-05-19-origin-phase-2.md
git commit -m "test(origin-daemon): phase-2 RAM soak bench + CHANGELOG entry"
git tag p2-complete
```

---

## Phase 2 self-review checklist (apply once after writing each task; final pass after P2.12)

**Spec coverage:**
- N2.1 multi-tail ring → P2.6, P2.10. ✅
- N2.4 step 1 handles outbound → P2.9. ✅
- N3.1 FastCDC chunker → P2.2. ✅
- N3.3 three-tier addressable store → P2.4. ✅
- N4.4 streaming → ring → P2.7, P2.8. ✅
- N10.4 token/usage feeding the status bar (groundwork for tracing) → P2.11. ✅
- CAS refs migration + GC → P2.5. ✅

**Deferred, called out in scope:**
- N3.2 dictionary zstd → P5. Not in this plan.
- N3.5 zero-copy IPC handoff → P11. Phase 2 ships bytes through `Event` frames.
- N2.2 speculative dispatch / N5.5 Recall / N4.2 CachePlanner → Phase 3.
- N2.5 sidecar → Phase 5.
- io_uring → Phase 12.

**Placeholder scan:** none of the steps end with "TBD" / "handle edge cases" / "similar to" — all code blocks are concrete. The two intentional carve-outs (P2.10 Step 7a's `ToolUseDelta` accumulation, and the P5 dictionary training) are flagged explicitly.

**Type consistency check:**
- `Hash([u8; 32])` consistent everywhere.
- `ChunkRef { offset, length, hash }` consistent in chunker.
- `Store::put` returns `Result<Hash, StoreError>` — matches all callsites.
- `Block::ToolResult { handle: Option<[u8; 32]>, inline: Option<Vec<u8>>, … }` — handle is the raw bytes, not `Hash`, matching the pre-existing `origin-core::types` definition.
- `LoopOptions` mutates from `Copy` (P1) to `Clone` because it now holds `Option<Arc<Store>>` + a tokio sender — every callsite updated to pass `&opts`.
- `TokenEvent`/`TokenKind` consistent across `origin-stream`, the SSE parser, the relay, and the CLI consumer; `Usage` payload is 4×u32 BE = 16 bytes, all sites agree.
- `StreamEvent` enum mirrors `TokenKind` 1:1 plus a `TurnEnd` variant — daemon-side relay and CLI-side consumer share the same `#[serde(tag = "kind", rename_all = "snake_case")]` definition.

**Open notes for the executing engineer:**
- If you find a clippy lint we didn't anticipate, prefer narrowing the offending block over `#[allow(...)]`. When `#[allow]` is necessary, include an inline justification comment, per workspace style.
- `eventsource-stream` 0.2 expects `Stream<Item = Result<Bytes, _>>`. The fixture test path uses `ReaderStream<&[u8]>` which yields `Result<Bytes, std::io::Error>` — correct. The live reqwest path needs the `e.to_string()` mapping shown.
- Pack-file format does not include a length-of-payload-section in the header; readers find the index via the footer. If you encounter pack corruption in tests, double-check that `finalize` is the LAST thing called — drop alone won't flush the `BufWriter`.

---

## Execution Handoff

The user has chosen **Subagent-Driven** execution upfront. Use **superpowers:subagent-driven-development** to dispatch one fresh subagent per Task (P2.1 → P2.12). Each subagent must:
1. Follow **superpowers:test-driven-development** (failing test first, run-to-fail, implement, run-to-pass).
2. Run the per-task verification gate.
3. Apply **superpowers:verification-before-completion** — do NOT mark the task complete or hand back to the orchestrator until the gate is green.

The orchestrator (this main session) does NOT move on to the next task until the verification result for the current task is reviewed and accepted.
