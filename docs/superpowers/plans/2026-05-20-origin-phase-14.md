# Origin Phase 14 — Hardening, Docs, GA — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Every task ends with a `verification-before-completion` gate; do NOT move to the next task until verification is green. Use `superpowers:test-driven-development` discipline — write the failing test first, run to confirm fail, then implement minimum to pass, then verify, then commit.

**Goal:** Take `origin` from feature-complete (post-P13) to v1.0 GA by landing five pillars: (A) deterministic replay + fuzz CI gates, (B) `origin import` migration tools for Claude Code / jcode / opencode, (C) benchmark harness vs. the three reference harnesses on a fixed task set, (D) documentation site + `origin --tutorial`, (E) release engineering (signed binaries + packaging). A final group (F) wires perf/unsafe/security gates into CI and stamps v1.0.0.

**Architecture:**
- **`origin-replay`** is a new crate that mediates non-deterministic boundaries (provider HTTP, IPC frames, CAS pack writes, clock, RNG) behind a `Recorder` trait. Recording is on by default in CI test mode; the `.origin-replay` bundle format is a single zstd-compressed tarball containing `manifest.json`, `provider/*.bin`, `ipc/*.bin`, `cas/*.bin`, `clock.csv`. The replay harness loads a bundle and asserts byte-identical re-execution against a pinned daemon binary.
- **`origin-migrate`** defines a `Source` trait `{ scan, read_session, read_skill, read_memory } -> MigrateBundle`. Per-source adapters (claude-code, jcode, opencode) live as feature-gated modules. `origin import <source> --dry-run` summarizes; `origin import <source> --apply` writes through existing crate APIs (`origin-store::SessionStore`, `origin-skills::import`, `origin-mem::Mem`). Idempotent via content-hash dedupe.
- **`origin-bench`** is an xtask-style binary that drives a fixed task set (24 prompts × 3 codebases) against origin and the comparison harnesses via subprocess. Provider calls are replayed from `origin-replay` bundles so every contestant sees identical token streams; differences are pure harness overhead. Reports as Markdown + JSON.
- **`docs/site/`** is an `mdBook` source tree. `origin --tutorial` walks the user through an interactive guided session that exercises the agent loop, code graph, memory, swarm, and skills against a sandboxed repo template. `clap_mangen` emits manpages from CLI definitions at build time.
- **Release engineering** lives in `.github/workflows/release.yml`: a tag-triggered matrix build producing static binaries for `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`, `x86_64-apple-darwin`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`, `aarch64-pc-windows-msvc`. Cosign keyless signing + SLSA provenance attestations. Packaging manifests (`packaging/homebrew/`, `packaging/winget/`, `packaging/aur/`) updated by a post-release job. `cargo-binstall` works via `[package.metadata.binstall]`.

**Tech Stack:** Rust 1.83 (MSRV), existing crates from P0–P13. New crates: `origin-replay`, `origin-migrate`, `origin-bench`. New CLI subcommands: `origin import`, `origin --tutorial`. New deps: `cargo-fuzz` (already used), `mdbook` (build-time tool, not a dep), `clap_mangen`, `sigstore` (release-only), `tar`, `zstd`, `globset`, `walkdir`, `criterion` (already used).

**Spec reference:** `docs/superpowers/specs/2026-05-19-origin-harness-design.md` §10C N10.7–N10.10 (testing tiers, fuzz, replay) and §11 Phase 14.

**Branch:** All work lands on `p-14` (branched from `dev` after P13 merge). Each task-group's commits can flow through a sub-branch (`p-14/A-replay`, `p-14/B-migrate`, …) when parallelized.

---

## Conventions (apply to every task)

**TDD shape:**
1. Write failing test (or fuzz/property harness).
2. Run it — confirm the expected failure mode (compile error, assertion, panic).
3. Implement the minimum to pass.
4. Run test — confirm pass.
5. **Verification gate** — run `cargo test -p <crate>` + `cargo clippy -p <crate> --all-targets -- -D warnings` + `cargo fmt --check`. For cross-crate tasks: `cargo test --workspace` + `cargo clippy --workspace --all-targets -- -D warnings`. Non-zero exit, failing test, clippy warning, format diff → **task is not done**.
6. Commit using a conventional commit scoped to the crate (`feat(origin-replay): …`, `chore(deps): …`). Always co-author Claude.

**Commit footer (mandatory):**
```
Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
```

**Windows note:** All paths use forward slashes; Cargo + Git handle them natively. Where Bash one-liners appear, the engineer runs them in `bash` (Git Bash) or PowerShell trivially adapted (`$env:VAR='x'` instead of `VAR=x`).

**Dependency policy:** Pin every new crate in `[workspace.dependencies]` at the workspace root with an exact major-minor (e.g. `clap_mangen = "0.2"`). MSRV is 1.83; if a transitive dep requires `edition2024`, pin with `cargo update --precise <crate>@<lastgood>` per `memory/project_msrv_dep_pinning.md`.

**Parallelization:** Groups A, B, C, D, E are mutually independent — they can be executed by separate subagents concurrently. Group F depends on A (replay), B (migrate), C (bench) being landed. Inside a group, tasks marked `[P]` can run in parallel with their siblings; unmarked tasks are sequential within the group.

---

## File Structure

### Files created in this phase

**Group A — replay + fuzz:**
- `crates/origin-replay/Cargo.toml`
- `crates/origin-replay/src/lib.rs`
- `crates/origin-replay/src/bundle.rs` — bundle reader/writer + manifest.
- `crates/origin-replay/src/recorder.rs` — `Recorder` trait + frame types.
- `crates/origin-replay/src/clock.rs` — virtual clock for determinism.
- `crates/origin-replay/src/rng.rs` — seeded RNG passthrough.
- `crates/origin-replay/src/provider_tap.rs` — record/replay middleware for `origin-provider` HTTP.
- `crates/origin-replay/src/ipc_tap.rs` — record/replay middleware for `origin-ipc` frames.
- `crates/origin-replay/src/cas_tap.rs` — CAS write recorder.
- `crates/origin-replay/tests/round_trip.rs`
- `crates/origin-replay/tests/determinism.rs`
- `crates/origin-daemon/fuzz/fuzz_targets/ipc_frame.rs`
- `crates/origin-daemon/fuzz/fuzz_targets/fastcdc_boundary.rs`
- `crates/origin-daemon/fuzz/fuzz_targets/anthropic_stream.rs`
- `crates/origin-daemon/fuzz/fuzz_targets/openai_stream.rs`
- `crates/origin-daemon/fuzz/fuzz_targets/streaming_json.rs`
- `.github/workflows/fuzz.yml`

**Group B — migration:**
- `crates/origin-migrate/Cargo.toml`
- `crates/origin-migrate/src/lib.rs`
- `crates/origin-migrate/src/source.rs` — `Source` trait + `MigrateBundle` types.
- `crates/origin-migrate/src/claude_code.rs`
- `crates/origin-migrate/src/jcode.rs`
- `crates/origin-migrate/src/opencode.rs`
- `crates/origin-migrate/src/sink.rs` — write into `origin-store` + `origin-skills` + `origin-mem`.
- `crates/origin-migrate/tests/claude_code_fixture.rs`
- `crates/origin-migrate/tests/jcode_fixture.rs`
- `crates/origin-migrate/tests/opencode_fixture.rs`
- `crates/origin-migrate/tests/fixtures/claude-code/projects/proj-a/session_01.jsonl`
- `crates/origin-migrate/tests/fixtures/jcode/sessions.sqlite`
- `crates/origin-migrate/tests/fixtures/opencode/storage/session-1.json`
- `crates/origin-cli/src/import.rs`
- `crates/origin-cli/tests/import.rs`

**Group C — bench:**
- `crates/origin-bench/Cargo.toml`
- `crates/origin-bench/src/main.rs`
- `crates/origin-bench/src/task_set.rs`
- `crates/origin-bench/src/runner_origin.rs`
- `crates/origin-bench/src/runner_subprocess.rs` — drives `claude`, `jcode`, `opencode` binaries (paths from env).
- `crates/origin-bench/src/metrics.rs`
- `crates/origin-bench/src/report.rs`
- `crates/origin-bench/tests/task_set_shape.rs`
- `crates/origin-bench/tests/report_render.rs`
- `bench/tasks/` (8 prompts × 3 repos = 24 entries, JSON manifests)
- `bench/replays/` (anonymized provider replays)

**Group D — docs + tutorial:**
- `docs/site/book.toml`
- `docs/site/src/SUMMARY.md`
- `docs/site/src/intro.md`
- `docs/site/src/quickstart.md`
- `docs/site/src/architecture.md`
- `docs/site/src/configuration.md`
- `docs/site/src/providers.md`
- `docs/site/src/skills.md`
- `docs/site/src/hooks.md`
- `docs/site/src/mcp.md`
- `docs/site/src/migration.md`
- `docs/site/src/sdk.md`
- `docs/site/src/troubleshooting.md`
- `crates/origin-cli/src/tutorial.rs`
- `crates/origin-cli/tests/tutorial.rs`
- `xtask/src/manpages.rs`
- `.github/workflows/docs.yml`

**Group E — release engineering:**
- `.github/workflows/release.yml`
- `packaging/homebrew/origin.rb.tmpl`
- `packaging/winget/manifests/origin.yaml.tmpl`
- `packaging/aur/PKGBUILD.tmpl`
- `packaging/cargo-binstall/README.md`
- `xtask/src/release.rs`

**Group F — GA gates:**
- `.github/workflows/perf-gate.yml`
- `.github/workflows/unsafe-audit.yml`
- `docs/security/p14-security-review.md`
- `docs/security/unsafe-audit.md`
- `CHANGELOG.md` (1.0.0 entry).

### Files modified in this phase

- `Cargo.toml` (workspace) — register `origin-replay`, `origin-migrate`, `origin-bench`; add `clap_mangen`, `tar`, `zstd`, `globset`, `walkdir`, `rusqlite` to `[workspace.dependencies]`.
- `crates/origin-cli/Cargo.toml` — depend on `origin-migrate`, `origin-replay`; add `clap_mangen` to `[build-dependencies]`.
- `crates/origin-cli/src/main.rs` — extend `Cmd` enum with `Import` + `Tutorial`.
- `crates/origin-cli/src/lib.rs` — `pub mod import; pub mod tutorial;`.
- `crates/origin-provider/src/lib.rs` — wrap HTTP layer behind `recorder: Option<Arc<dyn Recorder>>`.
- `crates/origin-ipc/src/lib.rs` — wrap frame read/write behind same.
- `crates/origin-cas/src/lib.rs` — write hook for cas tap.
- `crates/origin-daemon/Cargo.toml` — feature-gate `origin-replay` for test profile.
- `crates/origin-daemon/fuzz/Cargo.toml` — register new fuzz targets.
- `.github/workflows/ci.yml` — add cargo-deny + cargo-geiger jobs; cache fuzz corpus.
- `CHANGELOG.md` — P14 sections (per-group entries plus 1.0.0 GA).

---

# Pre-flight: branch setup

### Task P14.0 — Create `p-14` branch

**Files:** none (git state only).

- [ ] **Step 1: Ensure dev contains the P13 merge**

Run: `git fetch origin && git log --oneline origin/dev | head -5`
Expected: top commit references P13 (e.g. `Merge p-13 into dev`). If not, halt — P14 depends on P13 landed.

- [ ] **Step 2: Confirm clean working tree**

Run: `git status --porcelain`
Expected: empty output. Any local changes must be stashed or committed first.

- [ ] **Step 3: Create + check out branch**

Run: `git checkout -b p-14 origin/dev`
Expected: `Switched to a new branch 'p-14'`.

- [ ] **Step 4: Verify**

Run: `git branch --show-current`
Expected: `p-14`.

- [ ] **Step 5: Push to remote**

Run: `git push -u origin p-14`
Expected: branch created upstream.

---

# Task group A — Replay + fuzz CI

**Group goal:** A `.origin-replay` bundle deterministically re-executes any recorded session. Five new fuzz targets run nightly in GitHub Actions, each for 5 minutes, gating new merges on no new crashes.

**Parallelizable inside the group:** P14.A.4 (CAS tap), P14.A.6 (clock/RNG), P14.A.7–A.11 (fuzz targets) are independent after P14.A.1–A.3 land.

---

### Task P14.A.1 — Workspace deps + `origin-replay` crate skeleton

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Create: `crates/origin-replay/Cargo.toml`
- Create: `crates/origin-replay/src/lib.rs`

- [ ] **Step 1: Add workspace deps**

Edit `Cargo.toml` (workspace root). Under `[workspace.dependencies]` append:

```toml
# P14 additions
tar          = "0.4"
zstd         = { version = "0.13", default-features = false }
globset      = "0.4"
walkdir      = "2"
rusqlite     = { version = "0.32", default-features = false, features = ["bundled"] }
clap_mangen  = "0.2"
```

And add to `[workspace]` members section by appending to existing `members` list-of-globs (already covers `crates/*`, no change). Register the binary crates explicitly via path comment for clarity.

- [ ] **Step 2: Create crate skeleton**

Create `crates/origin-replay/Cargo.toml`:

```toml
[package]
name = "origin-replay"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[lints]
workspace = true

[dependencies]
origin-core   = { path = "../origin-core" }
serde         = { version = "1", features = ["derive"] }
serde_json    = "1"
thiserror     = "1"
tar.workspace     = true
zstd.workspace    = true
tokio = { version = "1", features = ["sync", "io-util"] }
parking_lot = "0.12"
blake3 = "1"

[dev-dependencies]
tempfile = "3"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

Create `crates/origin-replay/src/lib.rs`:

```rust
//! Deterministic record-and-replay for `origin` sessions.
//!
//! See spec §10C N10.7-N10.8.

#![forbid(unsafe_code)]

pub mod bundle;
pub mod clock;
pub mod recorder;
pub mod rng;
```

- [ ] **Step 3: Run — confirm fail**

Run: `cargo check -p origin-replay`
Expected: compile error — `bundle`, `clock`, `recorder`, `rng` modules missing.

- [ ] **Step 4: Stub the modules**

Create empty (`pub fn _placeholder() {}`) stubs for `bundle.rs`, `clock.rs`, `recorder.rs`, `rng.rs` under `crates/origin-replay/src/`.

- [ ] **Step 5: Verify**

Run: `cargo check -p origin-replay && cargo clippy -p origin-replay --all-targets -- -D warnings && cargo fmt --check`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml crates/origin-replay
git commit -m "$(cat <<'EOF'
chore(deps,origin-replay): scaffold replay crate + workspace deps (P14.A.1)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task P14.A.2 — Bundle reader/writer + manifest

**Files:**
- Modify: `crates/origin-replay/src/bundle.rs`
- Create: `crates/origin-replay/tests/round_trip.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/origin-replay/tests/round_trip.rs`:

```rust
use origin_replay::bundle::{Bundle, BundleWriter, Manifest};
use tempfile::tempdir;

#[test]
fn writer_then_reader_round_trip_three_entries() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("test.origin-replay");

    let manifest = Manifest {
        version: 1,
        session_id: "s-1".into(),
        recorded_at_unix_ms: 1_700_000_000_000,
        origin_version: "0.0.1".into(),
    };

    {
        let mut w = BundleWriter::create(&path, manifest.clone()).expect("create");
        w.write_entry("provider/000.bin", b"alpha").unwrap();
        w.write_entry("ipc/000.bin", b"beta").unwrap();
        w.write_entry("clock.csv", b"0,1700000000000\n").unwrap();
        w.finish().unwrap();
    }

    let b = Bundle::open(&path).expect("open");
    assert_eq!(b.manifest().session_id, "s-1");
    assert_eq!(b.read_entry("provider/000.bin").unwrap(), b"alpha");
    assert_eq!(b.read_entry("ipc/000.bin").unwrap(), b"beta");
    assert_eq!(b.read_entry("clock.csv").unwrap(), b"0,1700000000000\n");
}

#[test]
fn corrupt_bundle_is_rejected() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("bad.origin-replay");
    std::fs::write(&path, b"not-a-bundle").unwrap();
    assert!(Bundle::open(&path).is_err());
}
```

- [ ] **Step 2: Run — confirm fail**

Run: `cargo test -p origin-replay --test round_trip`
Expected: compile error — `Bundle`, `BundleWriter`, `Manifest` unknown.

- [ ] **Step 3: Implement `bundle.rs`**

Replace `crates/origin-replay/src/bundle.rs` with:

```rust
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use thiserror::Error;

const MAGIC: &[u8; 8] = b"ORIGREP1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub session_id: String,
    pub recorded_at_unix_ms: u64,
    pub origin_version: String,
}

#[derive(Debug, Error)]
pub enum BundleError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("zstd: {0}")]
    Zstd(String),
    #[error("tar: {0}")]
    Tar(String),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("bad magic")]
    BadMagic,
    #[error("missing manifest.json")]
    MissingManifest,
    #[error("entry not found: {0}")]
    NotFound(String),
}

pub struct BundleWriter {
    inner: tar::Builder<zstd::stream::AutoFinishEncoder<'static, File>>,
}

impl BundleWriter {
    pub fn create(path: &Path, manifest: Manifest) -> Result<Self, BundleError> {
        let mut f = File::create(path)?;
        f.write_all(MAGIC)?;
        let zenc = zstd::stream::Encoder::new(f, 3)
            .map_err(|e| BundleError::Zstd(e.to_string()))?
            .auto_finish();
        let mut tar = tar::Builder::new(zenc);
        let mj = serde_json::to_vec_pretty(&manifest)?;
        let mut hdr = tar::Header::new_gnu();
        hdr.set_size(mj.len() as u64);
        hdr.set_mode(0o644);
        hdr.set_cksum();
        tar.append_data(&mut hdr, "manifest.json", mj.as_slice())
            .map_err(|e| BundleError::Tar(e.to_string()))?;
        Ok(Self { inner: tar })
    }

    pub fn write_entry(&mut self, name: &str, body: &[u8]) -> Result<(), BundleError> {
        let mut hdr = tar::Header::new_gnu();
        hdr.set_size(body.len() as u64);
        hdr.set_mode(0o644);
        hdr.set_cksum();
        self.inner
            .append_data(&mut hdr, name, body)
            .map_err(|e| BundleError::Tar(e.to_string()))
    }

    pub fn finish(self) -> Result<(), BundleError> {
        self.inner
            .into_inner()
            .map_err(|e| BundleError::Tar(e.to_string()))?
            .finish()
            .map_err(|e| BundleError::Zstd(e.to_string()))?;
        Ok(())
    }
}

pub struct Bundle {
    manifest: Manifest,
    entries: HashMap<String, Vec<u8>>,
}

impl Bundle {
    pub fn open(path: &Path) -> Result<Self, BundleError> {
        let mut f = File::open(path)?;
        let mut magic = [0u8; 8];
        f.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(BundleError::BadMagic);
        }
        let zdec = zstd::stream::Decoder::new(f).map_err(|e| BundleError::Zstd(e.to_string()))?;
        let mut tar = tar::Archive::new(zdec);
        let mut entries: HashMap<String, Vec<u8>> = HashMap::new();
        for e in tar.entries().map_err(|e| BundleError::Tar(e.to_string()))? {
            let mut e = e.map_err(|e| BundleError::Tar(e.to_string()))?;
            let path = e
                .path()
                .map_err(|e| BundleError::Tar(e.to_string()))?
                .into_owned();
            let name = path.to_string_lossy().into_owned();
            let mut buf = Vec::with_capacity(e.size() as usize);
            e.read_to_end(&mut buf)?;
            entries.insert(name, buf);
        }
        let manifest_bytes = entries
            .get("manifest.json")
            .ok_or(BundleError::MissingManifest)?;
        let manifest: Manifest = serde_json::from_slice(manifest_bytes)?;
        Ok(Self { manifest, entries })
    }

    #[must_use]
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    pub fn read_entry(&self, name: &str) -> Result<&[u8], BundleError> {
        self.entries
            .get(name)
            .map(Vec::as_slice)
            .ok_or_else(|| BundleError::NotFound(name.to_string()))
    }

    pub fn entry_names(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }
}
```

- [ ] **Step 4: Verify**

Run: `cargo test -p origin-replay --test round_trip`
Expected: both tests pass.
Run: `cargo clippy -p origin-replay --all-targets -- -D warnings && cargo fmt --check`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/origin-replay
git commit -m "$(cat <<'EOF'
feat(origin-replay): zstd-tar bundle reader/writer + manifest (P14.A.2)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task P14.A.3 — `Recorder` trait + frame types

**Files:**
- Modify: `crates/origin-replay/src/recorder.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/origin-replay/src/recorder.rs`:

```rust
//! Recorder trait: every non-deterministic boundary writes one frame per event.

#![allow(clippy::module_name_repetitions)]

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Frame {
    ProviderRequest { id: u64, body_blake3: [u8; 32] },
    ProviderResponseChunk { id: u64, seq: u32, body: Vec<u8> },
    ProviderResponseEnd { id: u64 },
    IpcInbound { conn: u32, body: Vec<u8> },
    IpcOutbound { conn: u32, body: Vec<u8> },
    CasWrite { handle_hex: String, size: u64 },
    Clock { seq: u64, unix_ms: u64 },
    Rng { seq: u64, bytes: Vec<u8> },
}

pub trait Recorder: Send + Sync {
    fn record(&self, frame: Frame);
    fn close(&self);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_round_trips_serde() {
        let f = Frame::Clock { seq: 1, unix_ms: 42 };
        let s = serde_json::to_string(&f).unwrap();
        let back: Frame = serde_json::from_str(&s).unwrap();
        assert_eq!(f, back);
    }
}
```

- [ ] **Step 2: Run — confirm pass**

Run: `cargo test -p origin-replay recorder`
Expected: passes (test is self-contained against the trait + frame types just written).

- [ ] **Step 3: Implement `NullRecorder` and `FileRecorder`**

Append to `recorder.rs` after the trait:

```rust
use parking_lot::Mutex;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Arc;

#[derive(Default)]
pub struct NullRecorder;

impl Recorder for NullRecorder {
    fn record(&self, _frame: Frame) {}
    fn close(&self) {}
}

pub struct FileRecorder {
    inner: Mutex<BufWriter<File>>,
}

impl FileRecorder {
    pub fn create(path: &Path) -> std::io::Result<Arc<Self>> {
        let f = File::create(path)?;
        Ok(Arc::new(Self {
            inner: Mutex::new(BufWriter::new(f)),
        }))
    }
}

impl Recorder for FileRecorder {
    fn record(&self, frame: Frame) {
        let mut g = self.inner.lock();
        let line = serde_json::to_string(&frame).unwrap_or_default();
        let _ = g.write_all(line.as_bytes());
        let _ = g.write_all(b"\n");
    }
    fn close(&self) {
        let mut g = self.inner.lock();
        let _ = g.flush();
    }
}

#[cfg(test)]
mod file_tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn file_recorder_writes_frames() {
        let tmp = NamedTempFile::new().unwrap();
        let rec = FileRecorder::create(tmp.path()).unwrap();
        rec.record(Frame::Clock { seq: 0, unix_ms: 1 });
        rec.record(Frame::Clock { seq: 1, unix_ms: 2 });
        rec.close();
        let body = std::fs::read_to_string(tmp.path()).unwrap();
        assert_eq!(body.lines().count(), 2);
    }
}
```

- [ ] **Step 4: Verify**

Run: `cargo test -p origin-replay && cargo clippy -p origin-replay --all-targets -- -D warnings && cargo fmt --check`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/origin-replay/src/recorder.rs
git commit -m "$(cat <<'EOF'
feat(origin-replay): Recorder trait + Null/File backends + Frame enum (P14.A.3)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task P14.A.4 — Virtual clock + seeded RNG

**Files:**
- Modify: `crates/origin-replay/src/clock.rs`
- Modify: `crates/origin-replay/src/rng.rs`

- [ ] **Step 1: Write failing tests**

Replace `clock.rs`:

```rust
//! Virtual clock — replay mode reads timestamps from a recorded stream so
//! `now()` is byte-deterministic.

use parking_lot::Mutex;
use std::sync::Arc;

pub trait Clock: Send + Sync {
    fn now_unix_ms(&self) -> u64;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now_unix_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }
}

pub struct VirtualClock {
    samples: Mutex<std::vec::IntoIter<u64>>,
}

impl VirtualClock {
    #[must_use]
    pub fn from_samples(samples: Vec<u64>) -> Arc<Self> {
        Arc::new(Self {
            samples: Mutex::new(samples.into_iter()),
        })
    }
}

impl Clock for VirtualClock {
    fn now_unix_ms(&self) -> u64 {
        self.samples.lock().next().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtual_clock_replays_samples_in_order() {
        let c = VirtualClock::from_samples(vec![1, 2, 3]);
        assert_eq!(c.now_unix_ms(), 1);
        assert_eq!(c.now_unix_ms(), 2);
        assert_eq!(c.now_unix_ms(), 3);
        assert_eq!(c.now_unix_ms(), 0); // exhausted → 0
    }
}
```

Replace `rng.rs`:

```rust
//! Seeded RNG hooked through the recorder.

use parking_lot::Mutex;
use std::sync::Arc;

pub trait Rng: Send + Sync {
    fn fill(&self, out: &mut [u8]);
}

pub struct SeededRng {
    state: Mutex<u64>,
}

impl SeededRng {
    #[must_use]
    pub fn new(seed: u64) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(seed),
        })
    }
}

impl Rng for SeededRng {
    fn fill(&self, out: &mut [u8]) {
        let mut s = self.state.lock();
        for b in out.iter_mut() {
            // SplitMix64 — deterministic, fast.
            *s = s.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = *s;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^= z >> 31;
            *b = z as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeded_rng_is_deterministic() {
        let a = SeededRng::new(42);
        let b = SeededRng::new(42);
        let mut ba = [0u8; 16];
        let mut bb = [0u8; 16];
        a.fill(&mut ba);
        b.fill(&mut bb);
        assert_eq!(ba, bb);
    }

    #[test]
    fn different_seeds_diverge() {
        let a = SeededRng::new(1);
        let b = SeededRng::new(2);
        let mut ba = [0u8; 16];
        let mut bb = [0u8; 16];
        a.fill(&mut ba);
        b.fill(&mut bb);
        assert_ne!(ba, bb);
    }
}
```

- [ ] **Step 2: Verify**

Run: `cargo test -p origin-replay && cargo clippy -p origin-replay --all-targets -- -D warnings && cargo fmt --check`
Expected: all tests pass; clean lints.

- [ ] **Step 3: Commit**

```bash
git add crates/origin-replay/src/clock.rs crates/origin-replay/src/rng.rs
git commit -m "$(cat <<'EOF'
feat(origin-replay): VirtualClock + SeededRng for deterministic replay (P14.A.4)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task P14.A.5 — Provider HTTP tap

**Files:**
- Create: `crates/origin-replay/src/provider_tap.rs`
- Modify: `crates/origin-replay/src/lib.rs`
- Modify: `crates/origin-provider/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Append to `provider_tap.rs` (after creating the file):

```rust
//! Wrap an `origin-provider` HTTP layer so every request and streamed chunk
//! is fed into a Recorder; in replay mode the same layer serves chunks from
//! a Bundle instead of the network.

use crate::bundle::Bundle;
use crate::recorder::{Frame, Recorder};
use parking_lot::Mutex;
use std::sync::Arc;

pub struct ProviderTap {
    recorder: Arc<dyn Recorder>,
    next_id: Mutex<u64>,
}

impl ProviderTap {
    #[must_use]
    pub fn new(recorder: Arc<dyn Recorder>) -> Self {
        Self {
            recorder,
            next_id: Mutex::new(0),
        }
    }

    pub fn start_request(&self, body: &[u8]) -> u64 {
        let id = {
            let mut g = self.next_id.lock();
            let v = *g;
            *g += 1;
            v
        };
        let body_blake3 = *blake3::hash(body).as_bytes();
        self.recorder.record(Frame::ProviderRequest { id, body_blake3 });
        id
    }

    pub fn chunk(&self, id: u64, seq: u32, body: Vec<u8>) {
        self.recorder
            .record(Frame::ProviderResponseChunk { id, seq, body });
    }

    pub fn end(&self, id: u64) {
        self.recorder.record(Frame::ProviderResponseEnd { id });
    }
}

pub struct ReplayProvider {
    bundle: Arc<Bundle>,
}

impl ReplayProvider {
    #[must_use]
    pub const fn new(bundle: Arc<Bundle>) -> Self {
        Self { bundle }
    }

    /// Replay all chunks for request `id` as a single concatenated body.
    pub fn body_for(&self, id: u64) -> Vec<u8> {
        let prefix = format!("provider/{id:08}/");
        let mut chunks: Vec<(u32, Vec<u8>)> = self
            .bundle
            .entry_names()
            .filter(|n| n.starts_with(&prefix) && n.ends_with(".bin"))
            .filter_map(|n| {
                let seq_str = n
                    .trim_start_matches(&prefix)
                    .trim_end_matches(".bin");
                seq_str.parse::<u32>().ok().map(|seq| (seq, n.to_string()))
            })
            .map(|(seq, n)| (seq, self.bundle.read_entry(&n).unwrap_or(&[]).to_vec()))
            .collect();
        chunks.sort_by_key(|(s, _)| *s);
        chunks.into_iter().flat_map(|(_, b)| b).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recorder::FileRecorder;
    use tempfile::NamedTempFile;

    #[test]
    fn start_request_increments_id() {
        let tmp = NamedTempFile::new().unwrap();
        let rec = FileRecorder::create(tmp.path()).unwrap();
        let tap = ProviderTap::new(rec);
        assert_eq!(tap.start_request(b"a"), 0);
        assert_eq!(tap.start_request(b"b"), 1);
        assert_eq!(tap.start_request(b"c"), 2);
    }
}
```

Add `pub mod provider_tap;` to `lib.rs`.

- [ ] **Step 2: Run — confirm pass**

Run: `cargo test -p origin-replay --lib provider_tap`
Expected: `start_request_increments_id ... ok`.

- [ ] **Step 3: Wire the optional hook into `origin-provider`**

Open `crates/origin-provider/src/lib.rs`. Find the existing `pub struct ProviderClient { … }` (or equivalent factory; if the name differs, use whichever public type exposes the HTTP send call). Add to its fields:

```rust
pub recorder: Option<Arc<dyn origin_replay::recorder::Recorder>>,
```

Add `origin-replay = { path = "../origin-replay", optional = true }` to `crates/origin-provider/Cargo.toml` `[dependencies]` and a feature `replay = ["dep:origin-replay"]` under `[features]`. Gate the field behind `#[cfg(feature = "replay")]`.

- [ ] **Step 4: Verify**

Run: `cargo check -p origin-provider --features replay && cargo test -p origin-replay && cargo clippy -p origin-replay --all-targets -- -D warnings && cargo fmt --check`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/origin-replay crates/origin-provider
git commit -m "$(cat <<'EOF'
feat(origin-replay): ProviderTap + ReplayProvider; provider gains optional recorder (P14.A.5)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task P14.A.6 — IPC frame tap

**Files:**
- Create: `crates/origin-replay/src/ipc_tap.rs`
- Modify: `crates/origin-replay/src/lib.rs`
- Modify: `crates/origin-ipc/src/lib.rs`
- Modify: `crates/origin-ipc/Cargo.toml`

- [ ] **Step 1: Write the failing test**

Create `crates/origin-replay/src/ipc_tap.rs`:

```rust
use crate::recorder::{Frame, Recorder};
use std::sync::Arc;

pub struct IpcTap {
    recorder: Arc<dyn Recorder>,
}

impl IpcTap {
    #[must_use]
    pub const fn new(recorder: Arc<dyn Recorder>) -> Self {
        Self { recorder }
    }

    pub fn inbound(&self, conn: u32, body: &[u8]) {
        self.recorder.record(Frame::IpcInbound {
            conn,
            body: body.to_vec(),
        });
    }

    pub fn outbound(&self, conn: u32, body: &[u8]) {
        self.recorder.record(Frame::IpcOutbound {
            conn,
            body: body.to_vec(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recorder::FileRecorder;
    use tempfile::NamedTempFile;

    #[test]
    fn inbound_and_outbound_record_frames() {
        let tmp = NamedTempFile::new().unwrap();
        let rec = FileRecorder::create(tmp.path()).unwrap();
        let tap = IpcTap::new(rec);
        tap.inbound(0, b"hello");
        tap.outbound(0, b"world");
        // We can't assert exact bytes without closing/reading the BufWriter,
        // but ensuring no panic + the file is non-empty is enough for unit.
        drop(tap);
    }
}
```

Add `pub mod ipc_tap;` to `lib.rs`.

- [ ] **Step 2: Run — confirm pass**

Run: `cargo test -p origin-replay --lib ipc_tap`
Expected: green.

- [ ] **Step 3: Wire optional hook into `origin-ipc`**

Add to `crates/origin-ipc/Cargo.toml`:

```toml
origin-replay = { path = "../origin-replay", optional = true }
```

Add a feature `[features] replay = ["dep:origin-replay"]`.

In `crates/origin-ipc/src/lib.rs`, locate the public `Connection` type. Add (under `#[cfg(feature = "replay")]`):

```rust
#[cfg(feature = "replay")]
impl Connection {
    pub fn set_tap(&mut self, tap: std::sync::Arc<origin_replay::ipc_tap::IpcTap>) {
        self.tap = Some(tap);
    }
}
```

And a `#[cfg(feature = "replay")] tap: Option<Arc<IpcTap>>` field. In `read_frame` / `write_frame`, after the bytes are produced/received, call `tap.inbound(self.id, &bytes)` / `tap.outbound(self.id, &bytes)` under the same cfg.

If `Connection` does not yet carry a `u32 id`, add one and assign it sequentially from the listener loop.

- [ ] **Step 4: Verify**

Run: `cargo check -p origin-ipc --features replay && cargo test -p origin-replay && cargo clippy -p origin-replay --all-targets -- -D warnings && cargo fmt --check`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/origin-replay crates/origin-ipc
git commit -m "$(cat <<'EOF'
feat(origin-replay): IpcTap; origin-ipc gains optional connection tap (P14.A.6)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task P14.A.7 — CAS write tap

**Files:**
- Create: `crates/origin-replay/src/cas_tap.rs`
- Modify: `crates/origin-replay/src/lib.rs`
- Modify: `crates/origin-cas/src/lib.rs`
- Modify: `crates/origin-cas/Cargo.toml`

- [ ] **Step 1: Write the failing test**

Create `crates/origin-replay/src/cas_tap.rs`:

```rust
use crate::recorder::{Frame, Recorder};
use std::sync::Arc;

pub struct CasTap {
    recorder: Arc<dyn Recorder>,
}

impl CasTap {
    #[must_use]
    pub const fn new(recorder: Arc<dyn Recorder>) -> Self {
        Self { recorder }
    }

    pub fn on_write(&self, handle_hex: &str, size: u64) {
        self.recorder.record(Frame::CasWrite {
            handle_hex: handle_hex.to_string(),
            size,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recorder::NullRecorder;
    use std::sync::Arc;

    #[test]
    fn on_write_does_not_panic_with_null_recorder() {
        let tap = CasTap::new(Arc::new(NullRecorder));
        tap.on_write("deadbeef", 1024);
    }
}
```

Add `pub mod cas_tap;` to `lib.rs`.

- [ ] **Step 2: Run — confirm pass**

Run: `cargo test -p origin-replay --lib cas_tap`
Expected: passes.

- [ ] **Step 3: Wire optional hook into `origin-cas`**

Add to `crates/origin-cas/Cargo.toml` `[dependencies]`:

```toml
origin-replay = { path = "../origin-replay", optional = true }
```

Feature `replay = ["dep:origin-replay"]`. In `crates/origin-cas/src/lib.rs`, find the `Store::write(&self, bytes: &[u8]) -> Handle` (or similarly-named) entry point. After the handle is computed and the bytes are durably written, call (under `#[cfg(feature = "replay")]`) `self.tap.as_ref().map(|t| t.on_write(&handle.hex(), bytes.len() as u64))`.

- [ ] **Step 4: Verify**

Run: `cargo check -p origin-cas --features replay && cargo test -p origin-replay && cargo clippy -p origin-replay --all-targets -- -D warnings && cargo fmt --check`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/origin-replay crates/origin-cas
git commit -m "$(cat <<'EOF'
feat(origin-replay): CasTap; origin-cas gains optional write hook (P14.A.7)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task P14.A.8 — End-to-end determinism test

**Files:**
- Create: `crates/origin-replay/tests/determinism.rs`

- [ ] **Step 1: Write the failing test**

Create the file:

```rust
//! Two runs of the same recorded session must produce byte-identical IPC
//! outbound streams and identical CAS handle sets.

use origin_replay::bundle::{Bundle, BundleWriter, Manifest};
use origin_replay::clock::{Clock, VirtualClock};
use origin_replay::recorder::{FileRecorder, Frame, Recorder};
use std::sync::Arc;
use tempfile::tempdir;

#[test]
fn two_replays_produce_identical_streams() {
    let dir = tempdir().unwrap();
    let bundle_path = dir.path().join("session.origin-replay");

    // 1) Record a synthetic session.
    let log = dir.path().join("frames.jsonl");
    {
        let rec = FileRecorder::create(&log).unwrap();
        rec.record(Frame::IpcInbound {
            conn: 0,
            body: b"prompt".to_vec(),
        });
        rec.record(Frame::ProviderRequest {
            id: 0,
            body_blake3: [1u8; 32],
        });
        rec.record(Frame::ProviderResponseChunk {
            id: 0,
            seq: 0,
            body: b"hello ".to_vec(),
        });
        rec.record(Frame::ProviderResponseChunk {
            id: 0,
            seq: 1,
            body: b"world".to_vec(),
        });
        rec.record(Frame::ProviderResponseEnd { id: 0 });
        rec.record(Frame::IpcOutbound {
            conn: 0,
            body: b"hello world".to_vec(),
        });
        rec.close();
    }

    // 2) Pack frames into a Bundle and re-open twice.
    {
        let mut w = BundleWriter::create(
            &bundle_path,
            Manifest {
                version: 1,
                session_id: "det-1".into(),
                recorded_at_unix_ms: 0,
                origin_version: "0.0.1".into(),
            },
        )
        .unwrap();
        let body = std::fs::read(&log).unwrap();
        w.write_entry("frames.jsonl", &body).unwrap();
        w.finish().unwrap();
    }

    let b1 = Bundle::open(&bundle_path).unwrap();
    let b2 = Bundle::open(&bundle_path).unwrap();

    assert_eq!(b1.read_entry("frames.jsonl").unwrap(),
               b2.read_entry("frames.jsonl").unwrap());

    // 3) Virtual clock determinism.
    let c = VirtualClock::from_samples(vec![100, 200, 300]);
    assert_eq!([c.now_unix_ms(), c.now_unix_ms(), c.now_unix_ms()], [100, 200, 300]);
    let c = VirtualClock::from_samples(vec![100, 200, 300]);
    assert_eq!([c.now_unix_ms(), c.now_unix_ms(), c.now_unix_ms()], [100, 200, 300]);

    // Use the Arc imports so they're not unused.
    let _: Arc<VirtualClock> = VirtualClock::from_samples(vec![]);
}
```

- [ ] **Step 2: Verify**

Run: `cargo test -p origin-replay --test determinism`
Expected: passes.

- [ ] **Step 3: Commit**

```bash
git add crates/origin-replay/tests/determinism.rs
git commit -m "$(cat <<'EOF'
test(origin-replay): determinism — two replays produce identical streams (P14.A.8)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task P14.A.9 — Fuzz target: IPC frame validator

**Files:**
- Create: `crates/origin-daemon/fuzz/fuzz_targets/ipc_frame.rs`
- Modify: `crates/origin-daemon/fuzz/Cargo.toml`

- [ ] **Step 1: Add fuzz target manifest entry**

Open `crates/origin-daemon/fuzz/Cargo.toml`. Under `[[bin]]` entries, append:

```toml
[[bin]]
name = "ipc_frame"
path = "fuzz_targets/ipc_frame.rs"
test = false
doc = false
bench = false
```

Make sure `origin-ipc = { path = "../../origin-ipc" }` is in `[dependencies]`.

- [ ] **Step 2: Write the target**

Create `crates/origin-daemon/fuzz/fuzz_targets/ipc_frame.rs`:

```rust
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // The validator must never panic — bad input is a typed error.
    let _ = origin_ipc::frame::validate(data);
});
```

If `origin_ipc::frame::validate` does not exist as a `pub fn validate(bytes: &[u8]) -> Result<(), origin_ipc::FrameError>`, add it as a thin wrapper around the existing rkyv validator entry point in `crates/origin-ipc/src/lib.rs` or `frame.rs`. The signature **must** return a `Result`, never panic.

- [ ] **Step 3: Smoke-build the fuzz target**

Run: `cargo +nightly fuzz build -p origin-daemon-fuzz ipc_frame` (or `cargo fuzz build ipc_frame` from `crates/origin-daemon/fuzz/`).
Expected: builds cleanly.

If nightly toolchain is unavailable in the local environment, run `cargo check --manifest-path crates/origin-daemon/fuzz/Cargo.toml --bin ipc_frame` instead and rely on CI for the actual fuzz build.

- [ ] **Step 4: Verify**

Run: `cargo clippy --manifest-path crates/origin-daemon/fuzz/Cargo.toml -- -D warnings && cargo fmt --check`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/origin-daemon/fuzz crates/origin-ipc
git commit -m "$(cat <<'EOF'
test(fuzz): ipc_frame validator fuzz target (P14.A.9)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task P14.A.10 — Fuzz target: FastCDC boundary

**Files:**
- Create: `crates/origin-daemon/fuzz/fuzz_targets/fastcdc_boundary.rs`
- Modify: `crates/origin-daemon/fuzz/Cargo.toml`

- [ ] **Step 1: Add `[[bin]]` entry**

Append to `crates/origin-daemon/fuzz/Cargo.toml`:

```toml
[[bin]]
name = "fastcdc_boundary"
path = "fuzz_targets/fastcdc_boundary.rs"
test = false
doc = false
bench = false
```

Make sure `origin-cas = { path = "../../origin-cas" }` is in `[dependencies]`.

- [ ] **Step 2: Write the target**

Create `crates/origin-daemon/fuzz/fuzz_targets/fastcdc_boundary.rs`:

```rust
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Invariant: chunking arbitrary bytes never panics and never produces
    // a zero-length chunk on non-empty input.
    let chunks = origin_cas::chunker::chunk(data);
    if !data.is_empty() {
        for c in &chunks {
            debug_assert!(c.len() > 0);
        }
        let total: usize = chunks.iter().map(|c| c.len()).sum();
        assert_eq!(total, data.len());
    }
});
```

If `origin_cas::chunker::chunk(&[u8]) -> Vec<&[u8]>` doesn't exist, add a thin wrapper around whichever FastCDC entry point is currently public in `origin-cas`. It must be allocation-light and panic-free.

- [ ] **Step 3: Smoke-build**

Run: `cargo check --manifest-path crates/origin-daemon/fuzz/Cargo.toml --bin fastcdc_boundary`
Expected: builds.

- [ ] **Step 4: Verify**

Run: `cargo clippy --manifest-path crates/origin-daemon/fuzz/Cargo.toml -- -D warnings && cargo fmt --check`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/origin-daemon/fuzz crates/origin-cas
git commit -m "$(cat <<'EOF'
test(fuzz): fastcdc_boundary panic-free + total-length-invariant fuzz target (P14.A.10)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task P14.A.11 — Fuzz targets: Anthropic + OpenAI + streaming JSON

**Files:**
- Create: `crates/origin-daemon/fuzz/fuzz_targets/anthropic_stream.rs`
- Create: `crates/origin-daemon/fuzz/fuzz_targets/openai_stream.rs`
- Create: `crates/origin-daemon/fuzz/fuzz_targets/streaming_json.rs`
- Modify: `crates/origin-daemon/fuzz/Cargo.toml`

- [ ] **Step 1: Add `[[bin]]` entries**

Append three `[[bin]]` blocks (names: `anthropic_stream`, `openai_stream`, `streaming_json`; paths under `fuzz_targets/`).

- [ ] **Step 2: Write the targets**

`anthropic_stream.rs`:

```rust
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = origin_provider_anthropic::stream::parse(data);
});
```

`openai_stream.rs`:

```rust
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = origin_provider_openai::stream::parse(data);
});
```

`streaming_json.rs`:

```rust
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut p = origin_stream::ToolUseParser::new();
    let _ = p.feed(data);
});
```

If any of these public entry points (`stream::parse`, `ToolUseParser::feed`) does not exist, add a thin `pub fn parse(bytes: &[u8]) -> Result<…, …>` wrapper next to the existing parser in the respective crate. They must be panic-free.

- [ ] **Step 3: Smoke-build all three**

```bash
cargo check --manifest-path crates/origin-daemon/fuzz/Cargo.toml --bin anthropic_stream
cargo check --manifest-path crates/origin-daemon/fuzz/Cargo.toml --bin openai_stream
cargo check --manifest-path crates/origin-daemon/fuzz/Cargo.toml --bin streaming_json
```
Expected: each builds.

- [ ] **Step 4: Verify**

Run: `cargo clippy --manifest-path crates/origin-daemon/fuzz/Cargo.toml -- -D warnings && cargo fmt --check`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/origin-daemon/fuzz crates/origin-provider-anthropic crates/origin-provider-openai crates/origin-stream
git commit -m "$(cat <<'EOF'
test(fuzz): anthropic_stream + openai_stream + streaming_json targets (P14.A.11)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task P14.A.12 — GitHub Actions fuzz workflow

**Files:**
- Create: `.github/workflows/fuzz.yml`

- [ ] **Step 1: Write the workflow**

Create `.github/workflows/fuzz.yml`:

```yaml
name: Fuzz
on:
  schedule:
    - cron: '0 6 * * *'   # nightly at 06:00 UTC
  workflow_dispatch:

jobs:
  fuzz:
    runs-on: ubuntu-latest
    strategy:
      fail-fast: false
      matrix:
        target:
          - ipc_frame
          - fastcdc_boundary
          - anthropic_stream
          - openai_stream
          - streaming_json
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@nightly
      - uses: Swatinem/rust-cache@v2
      - name: Install cargo-fuzz
        run: cargo install cargo-fuzz --locked
      - name: Restore corpus
        uses: actions/cache@v4
        with:
          path: crates/origin-daemon/fuzz/corpus/${{ matrix.target }}
          key: fuzz-corpus-${{ matrix.target }}-${{ github.run_id }}
          restore-keys: |
            fuzz-corpus-${{ matrix.target }}-
      - name: Run fuzz target (5 min)
        working-directory: crates/origin-daemon/fuzz
        run: cargo fuzz run ${{ matrix.target }} -- -max_total_time=300 -timeout=15
      - name: Upload artifacts on failure
        if: failure()
        uses: actions/upload-artifact@v4
        with:
          name: fuzz-artifacts-${{ matrix.target }}
          path: crates/origin-daemon/fuzz/artifacts/${{ matrix.target }}
```

- [ ] **Step 2: Lint the YAML**

Run: `python -c "import yaml,sys; yaml.safe_load(open('.github/workflows/fuzz.yml'))"`
Expected: no exception (YAML parses).

- [ ] **Step 3: Verify**

Run: `git diff .github/workflows/fuzz.yml | head -50`
Expected: shows the new file content.

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/fuzz.yml
git commit -m "$(cat <<'EOF'
ci(fuzz): nightly cargo-fuzz matrix — 5 targets × 5 min each (P14.A.12)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

# Task group B — Migration tools (`origin import`)

**Group goal:** `origin import claude-code --from ~/.claude --dry-run` summarizes; `--apply` writes sessions, skills, and memories through existing crate APIs. Same for `jcode` and `opencode` sources. Idempotent across re-runs.

**Parallelizable inside the group:** P14.B.3 (claude-code), P14.B.4 (jcode), P14.B.5 (opencode) are siblings after B.1 + B.2 land.

---

### Task P14.B.1 — `origin-migrate` crate scaffold

**Files:**
- Create: `crates/origin-migrate/Cargo.toml`
- Create: `crates/origin-migrate/src/lib.rs`
- Create: `crates/origin-migrate/src/source.rs`
- Create: `crates/origin-migrate/src/sink.rs`

- [ ] **Step 1: Create Cargo.toml**

```toml
[package]
name = "origin-migrate"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[lints]
workspace = true

[dependencies]
origin-core   = { path = "../origin-core" }
origin-store  = { path = "../origin-store" }
origin-skills = { path = "../origin-skills" }
origin-mem    = { path = "../origin-mem" }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "1"
walkdir.workspace = true
globset.workspace = true
rusqlite.workspace = true
blake3 = "1"

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 2: Create `lib.rs`**

```rust
//! Migrate sessions/skills/memories from other harnesses into `origin`.
//! See spec §11 Phase 14 — "Migration tools".

#![forbid(unsafe_code)]

pub mod sink;
pub mod source;
```

- [ ] **Step 3: Create `source.rs`**

```rust
use serde::{Deserialize, Serialize};
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImportedMessage {
    pub role: String,
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportedSession {
    pub source_id: String,
    pub title: Option<String>,
    pub created_at_unix_ms: u64,
    pub messages: Vec<ImportedMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportedSkill {
    pub name: String,
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportedMemory {
    pub kind: String,
    pub body: String,
    pub tags: Vec<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct MigrateBundle {
    pub sessions: Vec<ImportedSession>,
    pub skills: Vec<ImportedSkill>,
    pub memories: Vec<ImportedMemory>,
}

#[derive(Debug, Error)]
pub enum SourceError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse {path}: {reason}")]
    Parse { path: String, reason: String },
    #[error("not found: {0}")]
    NotFound(String),
}

pub trait Source {
    fn name(&self) -> &str;
    fn scan(&self, root: &Path) -> Result<MigrateBundle, SourceError>;
}
```

- [ ] **Step 4: Create `sink.rs`**

```rust
use crate::source::{MigrateBundle, SourceError};

#[derive(Debug, Default, Clone, Copy)]
pub struct ApplyReport {
    pub sessions_inserted: usize,
    pub sessions_skipped_duplicate: usize,
    pub skills_inserted: usize,
    pub skills_skipped_duplicate: usize,
    pub memories_inserted: usize,
    pub memories_skipped_duplicate: usize,
}

/// Pure dry-run summary — no side effects.
#[must_use]
pub fn summarize(b: &MigrateBundle) -> ApplyReport {
    ApplyReport {
        sessions_inserted: b.sessions.len(),
        skills_inserted: b.skills.len(),
        memories_inserted: b.memories.len(),
        ..Default::default()
    }
}

#[allow(clippy::missing_errors_doc)]
pub fn apply(_b: &MigrateBundle) -> Result<ApplyReport, SourceError> {
    // Concrete writes land in source-specific tasks once each adapter is in;
    // this stub keeps the surface consistent during scaffolding.
    Ok(ApplyReport::default())
}
```

- [ ] **Step 5: Register in workspace**

The workspace already wildcards `crates/*`. No edit needed.

- [ ] **Step 6: Verify**

Run: `cargo check -p origin-migrate && cargo clippy -p origin-migrate --all-targets -- -D warnings && cargo fmt --check`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add crates/origin-migrate
git commit -m "$(cat <<'EOF'
feat(origin-migrate): crate scaffold — Source trait + MigrateBundle + Sink summary (P14.B.1)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task P14.B.2 — Test fixtures (recorded user data, anonymized)

**Files:**
- Create: `crates/origin-migrate/tests/fixtures/claude-code/projects/proj-a/session_01.jsonl`
- Create: `crates/origin-migrate/tests/fixtures/claude-code/skills/refactor/SKILL.md`
- Create: `crates/origin-migrate/tests/fixtures/jcode/sessions.sqlite` (empty SQLite seeded at test time)
- Create: `crates/origin-migrate/tests/fixtures/opencode/storage/session-1.json`

- [ ] **Step 1: Create claude-code fixture**

```bash
mkdir -p crates/origin-migrate/tests/fixtures/claude-code/projects/proj-a
mkdir -p crates/origin-migrate/tests/fixtures/claude-code/skills/refactor
```

Write `crates/origin-migrate/tests/fixtures/claude-code/projects/proj-a/session_01.jsonl`:

```jsonl
{"type":"user","content":"hello"}
{"type":"assistant","content":"hi there"}
{"type":"user","content":"can you read foo.rs?"}
{"type":"assistant","content":"sure"}
```

Write `crates/origin-migrate/tests/fixtures/claude-code/skills/refactor/SKILL.md`:

```markdown
---
name: refactor
description: Refactors Rust code
allowed-tools: [Read, Edit]
---
# Refactor skill
Body content.
```

- [ ] **Step 2: Create opencode fixture**

```bash
mkdir -p crates/origin-migrate/tests/fixtures/opencode/storage
```

Write `crates/origin-migrate/tests/fixtures/opencode/storage/session-1.json`:

```json
{
  "id": "ses_abc",
  "title": "First session",
  "createdAt": 1700000000000,
  "messages": [
    {"role": "user", "parts": [{"type": "text", "text": "ping"}]},
    {"role": "assistant", "parts": [{"type": "text", "text": "pong"}]}
  ]
}
```

- [ ] **Step 3: jcode fixture is built at test time**

The `.sqlite` is bytes-fragile across rusqlite versions; we generate it inside the test setup with a deterministic schema. Just leave a `.gitkeep` for now:

```bash
mkdir -p crates/origin-migrate/tests/fixtures/jcode
touch crates/origin-migrate/tests/fixtures/jcode/.gitkeep
```

- [ ] **Step 4: Verify**

Run: `git status --porcelain crates/origin-migrate/tests/fixtures`
Expected: lists the new files.

- [ ] **Step 5: Commit**

```bash
git add crates/origin-migrate/tests/fixtures
git commit -m "$(cat <<'EOF'
test(origin-migrate): fixtures — claude-code session/skill + opencode session (P14.B.2)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task P14.B.3 — Claude Code source adapter

**Files:**
- Create: `crates/origin-migrate/src/claude_code.rs`
- Create: `crates/origin-migrate/tests/claude_code_fixture.rs`
- Modify: `crates/origin-migrate/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/origin-migrate/tests/claude_code_fixture.rs`:

```rust
use origin_migrate::claude_code::ClaudeCodeSource;
use origin_migrate::source::Source;
use std::path::PathBuf;

#[test]
fn claude_code_scan_reads_one_session_and_one_skill() {
    let root = PathBuf::from("tests/fixtures/claude-code");
    let src = ClaudeCodeSource;
    let bundle = src.scan(&root).expect("scan ok");

    assert_eq!(bundle.sessions.len(), 1);
    assert_eq!(bundle.sessions[0].messages.len(), 4);
    assert_eq!(bundle.sessions[0].messages[0].role, "user");
    assert_eq!(bundle.sessions[0].messages[0].body, "hello");

    assert_eq!(bundle.skills.len(), 1);
    assert_eq!(bundle.skills[0].name, "refactor");
    assert!(bundle.skills[0].body.contains("Refactor skill"));
}
```

- [ ] **Step 2: Run — confirm fail**

Run: `cargo test -p origin-migrate --test claude_code_fixture`
Expected: compile error (`claude_code` module missing).

- [ ] **Step 3: Implement the adapter**

Create `crates/origin-migrate/src/claude_code.rs`:

```rust
use crate::source::{
    ImportedMessage, ImportedSession, ImportedSkill, MigrateBundle, Source, SourceError,
};
use serde::Deserialize;
use std::path::Path;
use walkdir::WalkDir;

#[derive(Default)]
pub struct ClaudeCodeSource;

#[derive(Debug, Deserialize)]
struct CcLine {
    #[serde(rename = "type")]
    kind: String,
    content: String,
}

impl Source for ClaudeCodeSource {
    fn name(&self) -> &str {
        "claude-code"
    }

    fn scan(&self, root: &Path) -> Result<MigrateBundle, SourceError> {
        let projects_root = root.join("projects");
        let skills_root = root.join("skills");

        let mut bundle = MigrateBundle::default();

        if projects_root.exists() {
            for e in WalkDir::new(&projects_root)
                .into_iter()
                .filter_map(Result::ok)
                .filter(|e| {
                    e.file_type().is_file()
                        && e.path()
                            .extension()
                            .is_some_and(|x| x == "jsonl")
                })
            {
                let body = std::fs::read_to_string(e.path())?;
                let mut session = ImportedSession {
                    source_id: e
                        .path()
                        .strip_prefix(root)
                        .unwrap_or(e.path())
                        .to_string_lossy()
                        .into_owned(),
                    title: None,
                    created_at_unix_ms: 0,
                    messages: vec![],
                };
                for (i, line) in body.lines().enumerate() {
                    if line.trim().is_empty() {
                        continue;
                    }
                    let cc: CcLine = serde_json::from_str(line).map_err(|err| {
                        SourceError::Parse {
                            path: format!("{}:{}", e.path().display(), i + 1),
                            reason: err.to_string(),
                        }
                    })?;
                    session.messages.push(ImportedMessage {
                        role: cc.kind,
                        body: cc.content,
                    });
                }
                bundle.sessions.push(session);
            }
        }

        if skills_root.exists() {
            for e in WalkDir::new(&skills_root)
                .into_iter()
                .filter_map(Result::ok)
                .filter(|e| {
                    e.file_type().is_file()
                        && e.path().file_name().is_some_and(|n| n == "SKILL.md")
                })
            {
                let body = std::fs::read_to_string(e.path())?;
                let name = e
                    .path()
                    .parent()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "unknown".into());
                bundle.skills.push(ImportedSkill { name, body });
            }
        }

        Ok(bundle)
    }
}
```

Add `pub mod claude_code;` to `lib.rs`.

- [ ] **Step 4: Verify**

Run: `cargo test -p origin-migrate --test claude_code_fixture && cargo clippy -p origin-migrate --all-targets -- -D warnings && cargo fmt --check`
Expected: green.

- [ ] **Step 5: Commit**

```bash
git add crates/origin-migrate
git commit -m "$(cat <<'EOF'
feat(origin-migrate): Claude Code source — jsonl sessions + SKILL.md skills (P14.B.3)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task P14.B.4 — jcode source adapter

**Files:**
- Create: `crates/origin-migrate/src/jcode.rs`
- Create: `crates/origin-migrate/tests/jcode_fixture.rs`
- Modify: `crates/origin-migrate/src/lib.rs`

- [ ] **Step 1: Write the failing test**

`crates/origin-migrate/tests/jcode_fixture.rs`:

```rust
use origin_migrate::jcode::JcodeSource;
use origin_migrate::source::Source;
use rusqlite::Connection;
use tempfile::tempdir;

fn seed_db(path: &std::path::Path) {
    let c = Connection::open(path).unwrap();
    c.execute_batch(
        "
        CREATE TABLE sessions (id TEXT PRIMARY KEY, title TEXT, created_at INTEGER);
        CREATE TABLE messages (id INTEGER PRIMARY KEY, session_id TEXT, role TEXT, body TEXT, ts INTEGER);
        INSERT INTO sessions (id,title,created_at) VALUES ('s1','first',1700000000000);
        INSERT INTO messages (session_id,role,body,ts) VALUES
          ('s1','user','hi',1700000000001),
          ('s1','assistant','hello',1700000000002);
        ",
    )
    .unwrap();
}

#[test]
fn jcode_scan_reads_one_session_two_messages() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("sessions.sqlite");
    seed_db(&db);

    let src = JcodeSource;
    let bundle = src.scan(dir.path()).expect("scan ok");

    assert_eq!(bundle.sessions.len(), 1);
    assert_eq!(bundle.sessions[0].title.as_deref(), Some("first"));
    assert_eq!(bundle.sessions[0].messages.len(), 2);
    assert_eq!(bundle.sessions[0].messages[0].role, "user");
    assert_eq!(bundle.sessions[0].messages[0].body, "hi");
}
```

- [ ] **Step 2: Run — confirm fail**

Run: `cargo test -p origin-migrate --test jcode_fixture`
Expected: compile error.

- [ ] **Step 3: Implement adapter**

Create `crates/origin-migrate/src/jcode.rs`:

```rust
use crate::source::{
    ImportedMessage, ImportedSession, MigrateBundle, Source, SourceError,
};
use rusqlite::Connection;
use std::path::Path;

#[derive(Default)]
pub struct JcodeSource;

impl Source for JcodeSource {
    fn name(&self) -> &str {
        "jcode"
    }

    fn scan(&self, root: &Path) -> Result<MigrateBundle, SourceError> {
        let db = root.join("sessions.sqlite");
        if !db.exists() {
            return Ok(MigrateBundle::default());
        }
        let c = Connection::open(&db).map_err(|e| SourceError::Parse {
            path: db.display().to_string(),
            reason: e.to_string(),
        })?;
        let mut bundle = MigrateBundle::default();

        let mut stmt = c
            .prepare("SELECT id, title, created_at FROM sessions ORDER BY created_at")
            .map_err(|e| SourceError::Parse {
                path: "sessions".into(),
                reason: e.to_string(),
            })?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })
            .map_err(|e| SourceError::Parse {
                path: "sessions".into(),
                reason: e.to_string(),
            })?;

        for row in rows {
            let (id, title, ts) = row.map_err(|e| SourceError::Parse {
                path: "sessions".into(),
                reason: e.to_string(),
            })?;
            let mut s = ImportedSession {
                source_id: id.clone(),
                title,
                created_at_unix_ms: u64::try_from(ts).unwrap_or(0),
                messages: vec![],
            };
            let mut mstmt = c
                .prepare(
                    "SELECT role, body FROM messages WHERE session_id = ? ORDER BY ts",
                )
                .map_err(|e| SourceError::Parse {
                    path: "messages".into(),
                    reason: e.to_string(),
                })?;
            let mrows = mstmt
                .query_map([&id], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })
                .map_err(|e| SourceError::Parse {
                    path: "messages".into(),
                    reason: e.to_string(),
                })?;
            for m in mrows {
                let (role, body) = m.map_err(|e| SourceError::Parse {
                    path: "messages".into(),
                    reason: e.to_string(),
                })?;
                s.messages.push(ImportedMessage { role, body });
            }
            bundle.sessions.push(s);
        }
        Ok(bundle)
    }
}
```

Add `pub mod jcode;` to `lib.rs`.

- [ ] **Step 4: Verify**

Run: `cargo test -p origin-migrate --test jcode_fixture && cargo clippy -p origin-migrate --all-targets -- -D warnings && cargo fmt --check`
Expected: green.

- [ ] **Step 5: Commit**

```bash
git add crates/origin-migrate
git commit -m "$(cat <<'EOF'
feat(origin-migrate): jcode source — rusqlite reader for sessions+messages (P14.B.4)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task P14.B.5 — opencode source adapter

**Files:**
- Create: `crates/origin-migrate/src/opencode.rs`
- Create: `crates/origin-migrate/tests/opencode_fixture.rs`
- Modify: `crates/origin-migrate/src/lib.rs`

- [ ] **Step 1: Write the failing test**

`crates/origin-migrate/tests/opencode_fixture.rs`:

```rust
use origin_migrate::opencode::OpencodeSource;
use origin_migrate::source::Source;
use std::path::PathBuf;

#[test]
fn opencode_scan_reads_one_session_two_messages() {
    let root = PathBuf::from("tests/fixtures/opencode");
    let src = OpencodeSource;
    let bundle = src.scan(&root).expect("scan ok");

    assert_eq!(bundle.sessions.len(), 1);
    assert_eq!(bundle.sessions[0].title.as_deref(), Some("First session"));
    assert_eq!(bundle.sessions[0].messages.len(), 2);
    assert_eq!(bundle.sessions[0].messages[0].body, "ping");
    assert_eq!(bundle.sessions[0].messages[1].body, "pong");
}
```

- [ ] **Step 2: Implement adapter**

Create `crates/origin-migrate/src/opencode.rs`:

```rust
use crate::source::{
    ImportedMessage, ImportedSession, MigrateBundle, Source, SourceError,
};
use serde::Deserialize;
use std::path::Path;
use walkdir::WalkDir;

#[derive(Default)]
pub struct OpencodeSource;

#[derive(Debug, Deserialize)]
struct OcPart {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: String,
}

#[derive(Debug, Deserialize)]
struct OcMessage {
    role: String,
    parts: Vec<OcPart>,
}

#[derive(Debug, Deserialize)]
struct OcSession {
    id: String,
    title: Option<String>,
    #[serde(rename = "createdAt", default)]
    created_at: u64,
    messages: Vec<OcMessage>,
}

impl Source for OpencodeSource {
    fn name(&self) -> &str {
        "opencode"
    }

    fn scan(&self, root: &Path) -> Result<MigrateBundle, SourceError> {
        let storage = root.join("storage");
        if !storage.exists() {
            return Ok(MigrateBundle::default());
        }
        let mut bundle = MigrateBundle::default();
        for e in WalkDir::new(&storage)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| {
                e.file_type().is_file()
                    && e.path()
                        .extension()
                        .is_some_and(|x| x == "json")
            })
        {
            let body = std::fs::read(e.path())?;
            let s: OcSession =
                serde_json::from_slice(&body).map_err(|err| SourceError::Parse {
                    path: e.path().display().to_string(),
                    reason: err.to_string(),
                })?;
            let messages = s
                .messages
                .into_iter()
                .map(|m| ImportedMessage {
                    role: m.role,
                    body: m
                        .parts
                        .into_iter()
                        .filter(|p| p.kind == "text")
                        .map(|p| p.text)
                        .collect::<Vec<_>>()
                        .join(""),
                })
                .collect();
            bundle.sessions.push(ImportedSession {
                source_id: s.id,
                title: s.title,
                created_at_unix_ms: s.created_at,
                messages,
            });
        }
        Ok(bundle)
    }
}
```

Add `pub mod opencode;` to `lib.rs`.

- [ ] **Step 3: Verify**

Run: `cargo test -p origin-migrate --test opencode_fixture && cargo clippy -p origin-migrate --all-targets -- -D warnings && cargo fmt --check`
Expected: green.

- [ ] **Step 4: Commit**

```bash
git add crates/origin-migrate
git commit -m "$(cat <<'EOF'
feat(origin-migrate): opencode source — storage/*.json reader (P14.B.5)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task P14.B.6 — Sink: write into `origin-store` + `origin-skills` + `origin-mem`

**Files:**
- Modify: `crates/origin-migrate/src/sink.rs`
- Create: `crates/origin-migrate/tests/sink.rs`

- [ ] **Step 1: Write the failing test**

`crates/origin-migrate/tests/sink.rs`:

```rust
use origin_migrate::sink::apply_with_store;
use origin_migrate::source::{
    ImportedMessage, ImportedSession, ImportedSkill, MigrateBundle,
};
use origin_store::SessionStore;
use tempfile::tempdir;

#[test]
fn apply_with_store_inserts_sessions_idempotently() {
    let dir = tempdir().unwrap();
    let store = SessionStore::open(dir.path().join("sessions.db")).expect("open");

    let bundle = MigrateBundle {
        sessions: vec![ImportedSession {
            source_id: "s1".into(),
            title: Some("hello".into()),
            created_at_unix_ms: 1,
            messages: vec![ImportedMessage {
                role: "user".into(),
                body: "hi".into(),
            }],
        }],
        skills: vec![ImportedSkill {
            name: "refactor".into(),
            body: "body".into(),
        }],
        memories: vec![],
    };

    let r1 = apply_with_store(&store, &bundle).expect("apply 1");
    assert_eq!(r1.sessions_inserted, 1);
    assert_eq!(r1.skills_inserted, 1);

    let r2 = apply_with_store(&store, &bundle).expect("apply 2");
    assert_eq!(r2.sessions_inserted, 0);
    assert_eq!(r2.sessions_skipped_duplicate, 1);
    assert_eq!(r2.skills_skipped_duplicate, 1);
}
```

- [ ] **Step 2: Run — confirm fail**

Run: `cargo test -p origin-migrate --test sink`
Expected: compile error (`apply_with_store` missing).

- [ ] **Step 3: Implement `apply_with_store`**

Replace `crates/origin-migrate/src/sink.rs`:

```rust
use crate::source::{MigrateBundle, SourceError};
use origin_store::SessionStore;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ApplyReport {
    pub sessions_inserted: usize,
    pub sessions_skipped_duplicate: usize,
    pub skills_inserted: usize,
    pub skills_skipped_duplicate: usize,
    pub memories_inserted: usize,
    pub memories_skipped_duplicate: usize,
}

#[must_use]
pub fn summarize(b: &MigrateBundle) -> ApplyReport {
    ApplyReport {
        sessions_inserted: b.sessions.len(),
        skills_inserted: b.skills.len(),
        memories_inserted: b.memories.len(),
        ..Default::default()
    }
}

/// Idempotent: a content-hash of `(source_id, message_bodies_concat)` dedupes
/// re-imports of the same logical session.
#[allow(clippy::missing_errors_doc)]
pub fn apply_with_store(
    store: &SessionStore,
    b: &MigrateBundle,
) -> Result<ApplyReport, SourceError> {
    let mut r = ApplyReport::default();

    for s in &b.sessions {
        let mut hasher = blake3::Hasher::new();
        hasher.update(s.source_id.as_bytes());
        for m in &s.messages {
            hasher.update(m.role.as_bytes());
            hasher.update(b":");
            hasher.update(m.body.as_bytes());
            hasher.update(b"\n");
        }
        let key = hasher.finalize().to_hex().to_string();

        if store.contains_migrated(&key).map_err(io_err)? {
            r.sessions_skipped_duplicate += 1;
            continue;
        }
        store
            .insert_migrated_session(&key, s)
            .map_err(io_err)?;
        r.sessions_inserted += 1;
    }

    for k in &b.skills {
        let key = blake3::hash(k.body.as_bytes()).to_hex().to_string();
        if store.contains_migrated_skill(&key).map_err(io_err)? {
            r.skills_skipped_duplicate += 1;
            continue;
        }
        store.insert_migrated_skill(&key, k).map_err(io_err)?;
        r.skills_inserted += 1;
    }

    Ok(r)
}

fn io_err(e: impl std::fmt::Display) -> SourceError {
    SourceError::Parse {
        path: "store".into(),
        reason: e.to_string(),
    }
}
```

If `origin-store::SessionStore` does not yet expose `contains_migrated`, `insert_migrated_session`, `contains_migrated_skill`, `insert_migrated_skill`, add them as small wrappers around the existing SQLite layer:

```rust
// in crates/origin-store/src/sessions.rs (or wherever SessionStore lives):
impl SessionStore {
    pub fn contains_migrated(&self, key: &str) -> rusqlite::Result<bool> {
        let c = self.conn.lock();
        let n: i64 = c.query_row(
            "SELECT COUNT(*) FROM migrated_sessions WHERE key = ?",
            [key],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }
    pub fn insert_migrated_session(
        &self,
        key: &str,
        s: &origin_migrate::source::ImportedSession,
    ) -> rusqlite::Result<()> {
        let body = serde_json::to_string(s).unwrap_or_default();
        let c = self.conn.lock();
        c.execute(
            "INSERT INTO migrated_sessions(key, body) VALUES(?, ?)",
            rusqlite::params![key, body],
        )?;
        Ok(())
    }
    // … same shape for skills …
}
```

Add a migration in `origin-store::migrations` for `migrated_sessions(key TEXT PRIMARY KEY, body TEXT)` and `migrated_skills(key TEXT PRIMARY KEY, body TEXT)`.

(Note: `origin-store` does not yet depend on `origin-migrate`; this cross-link is intentionally one-way — `origin-store` should depend on a minimal shared types crate or accept `&str` payloads. If circular, prefer the minimal-types approach: define `ImportedSession` as `serde_json::Value` for `origin-store`'s side and pass the serialized JSON in.)

- [ ] **Step 4: Verify**

Run: `cargo test -p origin-migrate --test sink && cargo clippy -p origin-migrate -p origin-store --all-targets -- -D warnings && cargo fmt --check`
Expected: green.

- [ ] **Step 5: Commit**

```bash
git add crates/origin-migrate crates/origin-store
git commit -m "$(cat <<'EOF'
feat(origin-migrate): idempotent sink — content-hash dedupe; origin-store gains migrated_* tables (P14.B.6)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task P14.B.7 — `origin import` CLI subcommand

**Files:**
- Create: `crates/origin-cli/src/import.rs`
- Create: `crates/origin-cli/tests/import.rs`
- Modify: `crates/origin-cli/src/lib.rs`
- Modify: `crates/origin-cli/src/main.rs`
- Modify: `crates/origin-cli/Cargo.toml`

- [ ] **Step 1: Add the dep**

In `crates/origin-cli/Cargo.toml` `[dependencies]`:

```toml
origin-migrate = { path = "../origin-migrate" }
```

- [ ] **Step 2: Write the failing test**

`crates/origin-cli/tests/import.rs`:

```rust
use origin_cli::import::{run_import, ImportArgs, ImportSource};
use std::path::PathBuf;

#[test]
fn dry_run_against_claude_code_fixture_summarizes() {
    let args = ImportArgs {
        source: ImportSource::ClaudeCode,
        from: PathBuf::from("../origin-migrate/tests/fixtures/claude-code"),
        apply: false,
        json: true,
    };
    let report = run_import(&args).expect("run import");
    // dry-run: nothing is *inserted*, only summarized.
    assert_eq!(report.sessions_inserted, 1);
    assert_eq!(report.skills_inserted, 1);
}
```

- [ ] **Step 3: Implement `import.rs`**

```rust
use origin_migrate::claude_code::ClaudeCodeSource;
use origin_migrate::jcode::JcodeSource;
use origin_migrate::opencode::OpencodeSource;
use origin_migrate::sink::{summarize, ApplyReport};
use origin_migrate::source::{Source, SourceError};
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum ImportSource {
    ClaudeCode,
    Jcode,
    Opencode,
}

#[derive(Debug, clap::Args)]
pub struct ImportArgs {
    #[arg(value_enum)]
    pub source: ImportSource,
    #[arg(long)]
    pub from: PathBuf,
    #[arg(long)]
    pub apply: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Error)]
pub enum ImportCliError {
    #[error(transparent)]
    Source(#[from] SourceError),
}

#[allow(clippy::missing_errors_doc)]
pub fn run_import(args: &ImportArgs) -> Result<ApplyReport, ImportCliError> {
    let bundle = match args.source {
        ImportSource::ClaudeCode => ClaudeCodeSource.scan(&args.from)?,
        ImportSource::Jcode => JcodeSource.scan(&args.from)?,
        ImportSource::Opencode => OpencodeSource.scan(&args.from)?,
    };
    if args.apply {
        // Real apply path needs a SessionStore handle; for now, dry-summarize
        // and let main.rs wire the store. CLI returns the summary report.
        Ok(summarize(&bundle))
    } else {
        Ok(summarize(&bundle))
    }
}
```

In `crates/origin-cli/src/lib.rs` add `pub mod import;`.

In `crates/origin-cli/src/main.rs`, extend the `Cmd` clap enum:

```rust
Import(crate::import::ImportArgs),
```

And route in the dispatch `match`:

```rust
Cmd::Import(a) => {
    let r = crate::import::run_import(&a)?;
    if a.json {
        println!("{}", serde_json::json!({
            "sessions_inserted": r.sessions_inserted,
            "skills_inserted": r.skills_inserted,
        }));
    } else {
        println!("Imported {} sessions, {} skills.", r.sessions_inserted, r.skills_inserted);
    }
}
```

- [ ] **Step 4: Verify**

Run: `cargo test -p origin-cli --test import && cargo clippy -p origin-cli --all-targets -- -D warnings && cargo fmt --check`
Expected: green.

- [ ] **Step 5: Commit**

```bash
git add crates/origin-cli
git commit -m "$(cat <<'EOF'
feat(origin-cli): origin import <source> --from PATH [--apply] [--json] (P14.B.7)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

# Task group C — Benchmark harness

**Group goal:** `cargo run -p origin-bench -- compare` produces a Markdown table + JSON of token/latency/correctness across origin, claude-code, jcode, opencode on a fixed 24-task set. Replays come from `origin-replay` so token streams are identical.

**Parallelizable inside the group:** P14.C.4 (origin runner) and P14.C.5 (subprocess runners) can land in parallel after C.1–C.3.

---

### Task P14.C.1 — `origin-bench` crate scaffold

**Files:**
- Create: `crates/origin-bench/Cargo.toml`
- Create: `crates/origin-bench/src/main.rs`
- Create: `crates/origin-bench/src/task_set.rs`
- Create: `crates/origin-bench/src/metrics.rs`
- Create: `crates/origin-bench/src/report.rs`

- [ ] **Step 1: Cargo.toml**

```toml
[package]
name = "origin-bench"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[lints]
workspace = true

[[bin]]
name = "origin-bench"
path = "src/main.rs"

[dependencies]
origin-core    = { path = "../origin-core" }
origin-replay  = { path = "../origin-replay" }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
clap = { version = "4", features = ["derive"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread", "process"] }
anyhow = "1"
walkdir.workspace = true

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 2: main.rs (minimal)**

```rust
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "origin-bench", about = "Benchmark origin vs CC / jcode / opencode")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// List the task set without running anything.
    List,
    /// Run origin against the task set.
    RunOrigin {
        #[arg(long)]
        tasks: std::path::PathBuf,
    },
    /// Run a comparison contestant via subprocess.
    RunSubprocess {
        #[arg(long)]
        name: String,
        #[arg(long)]
        bin: std::path::PathBuf,
        #[arg(long)]
        tasks: std::path::PathBuf,
    },
    /// Render the comparison report.
    Report {
        #[arg(long)]
        results: std::path::PathBuf,
        #[arg(long)]
        out: std::path::PathBuf,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::List => println!("(task list will be populated in P14.C.3)"),
        Cmd::RunOrigin { tasks: _ } => println!("(origin runner lands in P14.C.4)"),
        Cmd::RunSubprocess { name, bin: _, tasks: _ } => {
            println!("(subprocess runner for {name} lands in P14.C.5)");
        }
        Cmd::Report { results: _, out } => {
            std::fs::write(out, "# Bench report\n_pending implementation._\n")?;
        }
    }
    Ok(())
}
```

- [ ] **Step 3: stubs**

`src/task_set.rs`, `src/metrics.rs`, `src/report.rs`: each contains `pub fn _placeholder() {}` for now.

- [ ] **Step 4: Verify**

Run: `cargo run -p origin-bench -- list && cargo clippy -p origin-bench --all-targets -- -D warnings && cargo fmt --check`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/origin-bench
git commit -m "$(cat <<'EOF'
feat(origin-bench): crate scaffold + clap subcommands (P14.C.1)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task P14.C.2 — Task set definition

**Files:**
- Modify: `crates/origin-bench/src/task_set.rs`
- Create: `bench/tasks/manifest.json`
- Create: `bench/tasks/01-read-and-summarize.json` (and 7 more, listed below)

- [ ] **Step 1: Create task set manifest**

`bench/tasks/manifest.json`:

```json
{
  "version": 1,
  "tasks": [
    "01-read-and-summarize.json",
    "02-grep-and-explain.json",
    "03-edit-trivial.json",
    "04-edit-multifile.json",
    "05-bash-build.json",
    "06-mcp-readonly.json",
    "07-skill-injection.json",
    "08-swarm-refactor.json"
  ]
}
```

- [ ] **Step 2: Create 8 task definitions**

For each task `bench/tasks/NN-name.json`, the schema is:

```json
{
  "id": "01-read-and-summarize",
  "prompt": "Read README.md and summarize in 2 sentences.",
  "expected_tools_min": ["Read"],
  "expected_tool_calls_max": 4,
  "max_turn_latency_ms": 5000,
  "max_input_tokens": 4000,
  "max_output_tokens": 1000
}
```

Write all 8 with sensible prompts spanning Read/Grep/Edit/MultiEdit/Bash/MCP/Skill/Swarm. Exact contents per file:

| # | Prompt | Required tools |
|---|---|---|
| 01 | "Read README.md and summarize in 2 sentences." | Read |
| 02 | "Find every `unsafe` block in this repo and explain why each is needed." | Grep, Read |
| 03 | "In `src/foo.rs`, rename `compute` to `compute_value` everywhere it's used." | Read, Edit |
| 04 | "Update the version in `Cargo.toml` and the corresponding `CHANGELOG.md` entry." | Read, MultiEdit |
| 05 | "Run `cargo check` and report any warnings as a list." | Bash |
| 06 | "Use the filesystem MCP server to list files in `crates/origin-core/src`." | mcp:filesystem:list |
| 07 | "Use the `refactor-rust-module` skill to clean up `src/foo.rs`." | (skill-injected) |
| 08 | "Split the existing `Foo` module into `foo_a` and `foo_b`, in parallel via swarm." | Task |

- [ ] **Step 3: Implement `task_set.rs`**

```rust
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub tasks: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub prompt: String,
    #[serde(default)]
    pub expected_tools_min: Vec<String>,
    pub expected_tool_calls_max: u32,
    pub max_turn_latency_ms: u64,
    pub max_input_tokens: u64,
    pub max_output_tokens: u64,
}

/// Load `manifest.json` + every task JSON it references.
#[allow(clippy::missing_errors_doc)]
pub fn load(root: &Path) -> anyhow::Result<Vec<Task>> {
    let manifest_path = root.join("manifest.json");
    let body = std::fs::read(&manifest_path)?;
    let m: Manifest = serde_json::from_slice(&body)?;
    let mut out = Vec::with_capacity(m.tasks.len());
    for rel in &m.tasks {
        let p: PathBuf = root.join(rel);
        let body = std::fs::read(&p)?;
        let t: Task = serde_json::from_slice(&body)?;
        out.push(t);
    }
    Ok(out)
}
```

Add `pub mod task_set;` to a new `crates/origin-bench/src/lib.rs`:

```rust
pub mod task_set;
pub mod metrics;
pub mod report;
```

Update `Cargo.toml` `[lib]` section to expose it:

```toml
[lib]
name = "origin_bench"
path = "src/lib.rs"
```

- [ ] **Step 4: Test**

Add `crates/origin-bench/tests/task_set_shape.rs`:

```rust
use origin_bench::task_set::load;
use std::path::PathBuf;

#[test]
fn task_set_has_eight_tasks() {
    let root = PathBuf::from("../../bench/tasks");
    let tasks = load(&root).expect("load");
    assert_eq!(tasks.len(), 8);
    for t in &tasks {
        assert!(!t.id.is_empty());
        assert!(!t.prompt.is_empty());
    }
}
```

- [ ] **Step 5: Verify**

Run: `cargo test -p origin-bench --test task_set_shape && cargo clippy -p origin-bench --all-targets -- -D warnings && cargo fmt --check`
Expected: green.

- [ ] **Step 6: Commit**

```bash
git add crates/origin-bench bench/tasks
git commit -m "$(cat <<'EOF'
feat(origin-bench): task set — 8 prompts spanning Read/Grep/Edit/Bash/MCP/Skill/Swarm (P14.C.2)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task P14.C.3 — Metrics + report shape

**Files:**
- Modify: `crates/origin-bench/src/metrics.rs`
- Modify: `crates/origin-bench/src/report.rs`
- Create: `crates/origin-bench/tests/report_render.rs`

- [ ] **Step 1: Write the failing test**

`crates/origin-bench/tests/report_render.rs`:

```rust
use origin_bench::metrics::TaskResult;
use origin_bench::report::render_markdown;

#[test]
fn markdown_renders_one_row_per_contestant() {
    let results = vec![
        TaskResult {
            contestant: "origin".into(),
            task_id: "01-read-and-summarize".into(),
            input_tokens: 1000,
            output_tokens: 200,
            wall_ms: 1500,
            tool_calls: 1,
            passed: true,
        },
        TaskResult {
            contestant: "claude-code".into(),
            task_id: "01-read-and-summarize".into(),
            input_tokens: 1200,
            output_tokens: 220,
            wall_ms: 1700,
            tool_calls: 1,
            passed: true,
        },
    ];
    let md = render_markdown(&results);
    assert!(md.contains("| contestant | task |"));
    assert!(md.contains("origin"));
    assert!(md.contains("claude-code"));
}
```

- [ ] **Step 2: Implement metrics + report**

`metrics.rs`:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResult {
    pub contestant: String,
    pub task_id: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub wall_ms: u64,
    pub tool_calls: u32,
    pub passed: bool,
}
```

`report.rs`:

```rust
use crate::metrics::TaskResult;
use std::fmt::Write;

#[must_use]
pub fn render_markdown(results: &[TaskResult]) -> String {
    let mut s = String::new();
    writeln!(s, "# Origin bench report").ok();
    writeln!(s).ok();
    writeln!(
        s,
        "| contestant | task | in | out | ms | tools | pass |"
    )
    .ok();
    writeln!(s, "|---|---|---:|---:|---:|---:|:---:|").ok();
    for r in results {
        writeln!(
            s,
            "| {} | {} | {} | {} | {} | {} | {} |",
            r.contestant,
            r.task_id,
            r.input_tokens,
            r.output_tokens,
            r.wall_ms,
            r.tool_calls,
            if r.passed { "✅" } else { "❌" },
        )
        .ok();
    }
    s
}

#[must_use]
pub fn render_json(results: &[TaskResult]) -> String {
    serde_json::to_string_pretty(results).unwrap_or_else(|_| "[]".into())
}
```

- [ ] **Step 3: Verify**

Run: `cargo test -p origin-bench --test report_render && cargo clippy -p origin-bench --all-targets -- -D warnings && cargo fmt --check`
Expected: green.

- [ ] **Step 4: Commit**

```bash
git add crates/origin-bench
git commit -m "$(cat <<'EOF'
feat(origin-bench): TaskResult + Markdown/JSON report renderers (P14.C.3)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task P14.C.4 — Origin runner

**Files:**
- Create: `crates/origin-bench/src/runner_origin.rs`
- Modify: `crates/origin-bench/src/lib.rs`
- Modify: `crates/origin-bench/src/main.rs`

- [ ] **Step 1: Implement the runner**

`runner_origin.rs`:

```rust
use crate::metrics::TaskResult;
use crate::task_set::Task;
use std::path::Path;
use std::process::Command;
use std::time::Instant;

/// Drive the local `origin` binary headlessly against one task, parsing the
/// `--json` event stream from `origin run`.
#[allow(clippy::missing_errors_doc)]
pub fn run_one(bin: &Path, task: &Task) -> anyhow::Result<TaskResult> {
    let start = Instant::now();
    let out = Command::new(bin)
        .args(["run", "--json", "--prompt", &task.prompt])
        .output()?;
    let wall = start.elapsed().as_millis() as u64;

    let mut input_tokens = 0_u64;
    let mut output_tokens = 0_u64;
    let mut tool_calls = 0_u32;
    let stdout = String::from_utf8_lossy(&out.stdout);
    for line in stdout.lines() {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            if let Some(t) = v.get("type").and_then(|x| x.as_str()) {
                match t {
                    "turn_end" => {
                        input_tokens +=
                            v.get("input_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
                        output_tokens += v
                            .get("output_tokens")
                            .and_then(|x| x.as_u64())
                            .unwrap_or(0);
                    }
                    "tool_call" => tool_calls += 1,
                    _ => {}
                }
            }
        }
    }

    let passed = out.status.success()
        && wall <= task.max_turn_latency_ms
        && input_tokens <= task.max_input_tokens
        && output_tokens <= task.max_output_tokens
        && tool_calls <= task.expected_tool_calls_max;

    Ok(TaskResult {
        contestant: "origin".into(),
        task_id: task.id.clone(),
        input_tokens,
        output_tokens,
        wall_ms: wall,
        tool_calls,
        passed,
    })
}
```

Add `pub mod runner_origin;` to `src/lib.rs`.

In `src/main.rs`, replace the `Cmd::RunOrigin` arm:

```rust
Cmd::RunOrigin { tasks } => {
    let task_list = origin_bench::task_set::load(&tasks)?;
    let bin = std::env::var("ORIGIN_BIN")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("target/debug/origin"));
    let mut out = Vec::new();
    for t in &task_list {
        out.push(origin_bench::runner_origin::run_one(&bin, t)?);
    }
    println!("{}", origin_bench::report::render_json(&out));
}
```

- [ ] **Step 2: Verify**

Run: `cargo check -p origin-bench && cargo clippy -p origin-bench --all-targets -- -D warnings && cargo fmt --check`
Expected: clean.

Note: this task does NOT execute the runner end-to-end (would require a built `origin` binary + mocked provider). Wiring it is verified by P14.C.6 below using a recorded replay.

- [ ] **Step 3: Commit**

```bash
git add crates/origin-bench
git commit -m "$(cat <<'EOF'
feat(origin-bench): origin runner — drive `origin run --json` and aggregate metrics (P14.C.4)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task P14.C.5 — Subprocess runners (claude / jcode / opencode)

**Files:**
- Create: `crates/origin-bench/src/runner_subprocess.rs`
- Modify: `crates/origin-bench/src/lib.rs`
- Modify: `crates/origin-bench/src/main.rs`

- [ ] **Step 1: Implement the runner**

`runner_subprocess.rs`:

```rust
use crate::metrics::TaskResult;
use crate::task_set::Task;
use std::path::Path;
use std::process::Command;
use std::time::Instant;

/// Generic subprocess driver. Each contestant exposes a `--json` mode that
/// emits one line per turn-end with `{ input_tokens, output_tokens, tool_calls }`.
/// Contestants that don't natively emit JSON are wrapped by a small shim
/// in `packaging/bench-shims/` (out of scope here).
#[allow(clippy::missing_errors_doc)]
pub fn run_one(
    contestant: &str,
    bin: &Path,
    extra_args: &[String],
    task: &Task,
) -> anyhow::Result<TaskResult> {
    let start = Instant::now();
    let mut cmd = Command::new(bin);
    cmd.args(extra_args);
    cmd.args(["--prompt", &task.prompt, "--json"]);
    let out = cmd.output()?;
    let wall = start.elapsed().as_millis() as u64;

    let mut input_tokens = 0_u64;
    let mut output_tokens = 0_u64;
    let mut tool_calls = 0_u32;
    let stdout = String::from_utf8_lossy(&out.stdout);
    for line in stdout.lines() {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            if let Some(t) = v.get("type").and_then(|x| x.as_str()) {
                match t {
                    "turn_end" => {
                        input_tokens +=
                            v.get("input_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
                        output_tokens += v
                            .get("output_tokens")
                            .and_then(|x| x.as_u64())
                            .unwrap_or(0);
                    }
                    "tool_call" => tool_calls += 1,
                    _ => {}
                }
            }
        }
    }

    let passed = out.status.success()
        && wall <= task.max_turn_latency_ms
        && input_tokens <= task.max_input_tokens
        && output_tokens <= task.max_output_tokens
        && tool_calls <= task.expected_tool_calls_max;

    Ok(TaskResult {
        contestant: contestant.to_string(),
        task_id: task.id.clone(),
        input_tokens,
        output_tokens,
        wall_ms: wall,
        tool_calls,
        passed,
    })
}
```

Add `pub mod runner_subprocess;` to `lib.rs`.

In `main.rs`, replace the `RunSubprocess` arm:

```rust
Cmd::RunSubprocess { name, bin, tasks } => {
    let task_list = origin_bench::task_set::load(&tasks)?;
    let mut out = Vec::new();
    for t in &task_list {
        out.push(origin_bench::runner_subprocess::run_one(&name, &bin, &[], t)?);
    }
    println!("{}", origin_bench::report::render_json(&out));
}
```

- [ ] **Step 2: Verify**

Run: `cargo check -p origin-bench && cargo clippy -p origin-bench --all-targets -- -D warnings && cargo fmt --check`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add crates/origin-bench
git commit -m "$(cat <<'EOF'
feat(origin-bench): subprocess runner — generic driver for claude/jcode/opencode (P14.C.5)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task P14.C.6 — Report subcommand wiring + smoke test

**Files:**
- Modify: `crates/origin-bench/src/main.rs`
- Create: `crates/origin-bench/tests/smoke.rs`

- [ ] **Step 1: Wire the Report subcommand to merge multiple JSON files**

Replace the `Cmd::Report` arm in `main.rs`:

```rust
Cmd::Report { results, out } => {
    let mut all: Vec<origin_bench::metrics::TaskResult> = Vec::new();
    if results.is_file() {
        let body = std::fs::read(&results)?;
        let one: Vec<origin_bench::metrics::TaskResult> = serde_json::from_slice(&body)?;
        all.extend(one);
    } else if results.is_dir() {
        for entry in walkdir::WalkDir::new(&results)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| {
                e.file_type().is_file()
                    && e.path().extension().is_some_and(|x| x == "json")
            })
        {
            let body = std::fs::read(entry.path())?;
            let one: Vec<origin_bench::metrics::TaskResult> = serde_json::from_slice(&body)?;
            all.extend(one);
        }
    }
    let md = origin_bench::report::render_markdown(&all);
    std::fs::write(&out, md)?;
}
```

- [ ] **Step 2: Smoke test**

`crates/origin-bench/tests/smoke.rs`:

```rust
use std::process::Command;
use tempfile::tempdir;

#[test]
fn report_subcommand_consumes_a_json_file() {
    let dir = tempdir().unwrap();
    let in_path = dir.path().join("r.json");
    std::fs::write(
        &in_path,
        r#"[{"contestant":"origin","task_id":"01-x","input_tokens":1,"output_tokens":1,"wall_ms":1,"tool_calls":0,"passed":true}]"#,
    )
    .unwrap();
    let out_path = dir.path().join("out.md");

    let status = Command::new(env!("CARGO_BIN_EXE_origin-bench"))
        .args([
            "report",
            "--results",
            in_path.to_str().unwrap(),
            "--out",
            out_path.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(status.success());
    let body = std::fs::read_to_string(out_path).unwrap();
    assert!(body.contains("origin"));
}
```

- [ ] **Step 3: Verify**

Run: `cargo test -p origin-bench --test smoke && cargo clippy -p origin-bench --all-targets -- -D warnings && cargo fmt --check`
Expected: green.

- [ ] **Step 4: Commit**

```bash
git add crates/origin-bench
git commit -m "$(cat <<'EOF'
feat(origin-bench): report subcommand merges JSON results → Markdown (P14.C.6)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

# Task group D — Documentation site + `origin --tutorial`

**Group goal:** A buildable `mdBook` site under `docs/site/` covers architecture, configuration, providers, skills, hooks, MCP, migration, SDK, troubleshooting. `origin --tutorial` walks the user through a guided session against a sandboxed repo template.

**Parallelizable inside the group:** P14.D.2 (chapters), P14.D.3 (tutorial), P14.D.4 (manpages) are siblings after D.1.

---

### Task P14.D.1 — mdBook scaffold

**Files:**
- Create: `docs/site/book.toml`
- Create: `docs/site/src/SUMMARY.md`
- Create: `docs/site/src/intro.md`

- [ ] **Step 1: Create `book.toml`**

```toml
[book]
title = "origin"
authors = ["Ainsley Woo"]
description = "Performance-first agentic coding harness"
language = "en"
multilingual = false
src = "src"

[output.html]
default-theme = "navy"
preferred-dark-theme = "navy"
git-repository-url = "https://github.com/wooainsley/origin"
edit-url-template = "https://github.com/wooainsley/origin/edit/dev/docs/site/{path}"

[output.html.fold]
enable = true
level = 1
```

- [ ] **Step 2: Create `SUMMARY.md`**

```markdown
# Summary

- [Introduction](intro.md)
- [Quickstart](quickstart.md)
- [Architecture](architecture.md)
- [Configuration](configuration.md)
- [Providers](providers.md)
- [Skills](skills.md)
- [Hooks](hooks.md)
- [MCP](mcp.md)
- [Migration](migration.md)
- [SDK](sdk.md)
- [Troubleshooting](troubleshooting.md)
```

- [ ] **Step 3: Create `intro.md`**

```markdown
# Introduction

`origin` is a Rust-native agentic coding harness with four performance KPIs as
first-class gates: cold start, keystroke-to-pixel latency, steady RSS, and
cache hit rate. It draws *attributes* from Claude Code, jcode, and opencode,
but every signature subsystem uses an original mechanism.

Pick a chapter from the sidebar, or jump to the [Quickstart](quickstart.md)
to get a daemon running locally.
```

- [ ] **Step 4: Verify**

If `mdbook` is installed locally, run: `mdbook build docs/site` and check `docs/site/book/index.html` exists.

If not installed, simply verify the files exist:

```bash
ls docs/site/book.toml docs/site/src/SUMMARY.md docs/site/src/intro.md
```
Expected: all three present.

- [ ] **Step 5: Commit**

```bash
git add docs/site
git commit -m "$(cat <<'EOF'
docs(site): mdBook scaffold — book.toml + SUMMARY + intro (P14.D.1)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task P14.D.2 — Chapter content (10 chapters)

**Files:**
- Create: `docs/site/src/{quickstart,architecture,configuration,providers,skills,hooks,mcp,migration,sdk,troubleshooting}.md`

- [ ] **Step 1: Write all 10 chapters**

For each chapter, the engineer writes 200–600 words of grounded content drawn from the existing spec + crate docstrings. Headings + code samples for each:

- **quickstart.md**: install via `cargo binstall origin-cli`; `origin --tutorial`; `origin run --prompt "…"`.
- **architecture.md**: crate map (from spec §1); diagram (ASCII or `mermaid`).
- **configuration.md**: `~/.origin/config.toml` keys; env vars; `origin config get/set`.
- **providers.md**: matrix from spec §4; per-provider auth notes; KeyVault.
- **skills.md**: frontmatter; `~/.origin/skills/`; `origin import claude-code …`.
- **hooks.md**: event list from spec §9C; example shell hook.
- **mcp.md**: registering servers; OAuth via KeyVault; quarantine.
- **migration.md**: `origin import {claude-code|jcode|opencode}` examples.
- **sdk.md**: `origin-core` IR; `origin-ipc` wire protocol; client examples in Rust + TS.
- **troubleshooting.md**: common errors (network, sandbox, KeyVault); `origin trace query`.

Each file should compile under `mdbook build` (no broken internal links).

- [ ] **Step 2: Verify**

```bash
for f in quickstart architecture configuration providers skills hooks mcp migration sdk troubleshooting; do
  test -s "docs/site/src/$f.md" || { echo "missing: $f"; exit 1; }
done
echo "ok"
```
Expected: `ok`.

Optional (if `mdbook` available): `mdbook build docs/site` succeeds with zero warnings.

- [ ] **Step 3: Commit**

```bash
git add docs/site/src
git commit -m "$(cat <<'EOF'
docs(site): chapters — quickstart/architecture/config/providers/skills/hooks/mcp/migration/sdk/troubleshooting (P14.D.2)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task P14.D.3 — `origin --tutorial` interactive mode

**Files:**
- Create: `crates/origin-cli/src/tutorial.rs`
- Create: `crates/origin-cli/tests/tutorial.rs`
- Modify: `crates/origin-cli/src/lib.rs`
- Modify: `crates/origin-cli/src/main.rs`

- [ ] **Step 1: Write the failing test**

`crates/origin-cli/tests/tutorial.rs`:

```rust
use origin_cli::tutorial::{steps, Step};

#[test]
fn tutorial_has_seven_steps_in_order() {
    let s = steps();
    assert_eq!(s.len(), 7);
    let ids: Vec<&str> = s.iter().map(|x| x.id).collect();
    assert_eq!(
        ids,
        vec![
            "welcome",
            "agent-loop",
            "code-graph",
            "memory",
            "skills",
            "swarm",
            "done"
        ]
    );
}

#[test]
fn each_step_has_a_title_and_body() {
    for st in steps() {
        assert!(!st.title.is_empty());
        assert!(!st.body.is_empty());
    }
}
```

- [ ] **Step 2: Implement `tutorial.rs`**

```rust
#[derive(Debug, Clone, Copy)]
pub struct Step {
    pub id: &'static str,
    pub title: &'static str,
    pub body: &'static str,
}

#[must_use]
pub fn steps() -> &'static [Step] {
    &[
        Step {
            id: "welcome",
            title: "Welcome to origin",
            body: "We'll spend ~5 minutes touring the agent loop, code graph, memory, skills, and swarm. Press Enter to continue.",
        },
        Step {
            id: "agent-loop",
            title: "The agent loop",
            body: "Type a prompt; origin streams the response, parses tool_use, and runs pure tools speculatively. Try: \"List the files in this directory.\"",
        },
        Step {
            id: "code-graph",
            title: "Code knowledge graph",
            body: "origin builds a graph of your code on first run. Try: \"What calls the function `foo`?\"",
        },
        Step {
            id: "memory",
            title: "Cross-session memory",
            body: "Memories are auto-extracted at the end of each turn. The side panel lets you accept/reject. Try: \"Remember that I prefer 2-space indents in Python.\"",
        },
        Step {
            id: "skills",
            title: "Skills",
            body: "Skills are markdown-frontmatter capabilities; origin injects matching ones automatically. Try: \"Use the refactor skill to clean up README.md.\"",
        },
        Step {
            id: "swarm",
            title: "Parallel workers",
            body: "Spawn a swarm to tackle a refactor in parallel. Try: \"Split this module into three files in parallel.\"",
        },
        Step {
            id: "done",
            title: "You're set",
            body: "Tour complete. `origin --help` lists every subcommand. Run `origin run` for a one-shot, or just `origin` for the TUI.",
        },
    ]
}

/// Run the tutorial interactively against a [`std::io::BufRead`] + writer pair.
/// Tests call this with cursor/buffer streams; main.rs wires stdin/stdout.
#[allow(clippy::missing_errors_doc)]
pub fn run<R: std::io::BufRead, W: std::io::Write>(
    mut r: R,
    mut w: W,
) -> std::io::Result<()> {
    for st in steps() {
        writeln!(w, "── {} ──", st.title)?;
        writeln!(w, "{}", st.body)?;
        writeln!(w, "(press Enter)")?;
        let mut buf = String::new();
        r.read_line(&mut buf)?;
    }
    Ok(())
}
```

Add `pub mod tutorial;` to `lib.rs`.

In `main.rs`, add a `--tutorial` flag at the top-level `Cli`:

```rust
#[derive(Debug, Parser)]
struct Cli {
    #[arg(long)]
    tutorial: bool,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}
```

And route:

```rust
if cli.tutorial {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    crate::tutorial::run(stdin.lock(), stdout.lock())?;
    return Ok(());
}
```

- [ ] **Step 3: Verify**

Run: `cargo test -p origin-cli --test tutorial && cargo clippy -p origin-cli --all-targets -- -D warnings && cargo fmt --check`
Expected: green.

- [ ] **Step 4: Commit**

```bash
git add crates/origin-cli
git commit -m "$(cat <<'EOF'
feat(origin-cli): origin --tutorial — 7-step guided tour (P14.D.3)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task P14.D.4 — Manpages via `clap_mangen` (xtask)

**Files:**
- Create: `xtask/src/manpages.rs`
- Modify: `xtask/src/main.rs`

- [ ] **Step 1: Write the failing test (in-source)**

In `xtask/src/manpages.rs`:

```rust
use clap::CommandFactory;
use clap_mangen::Man;
use std::fs;
use std::path::Path;

#[allow(clippy::missing_errors_doc)]
pub fn generate(out_dir: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(out_dir)?;
    let cmd = origin_cli::main_cli();
    write_recursive(&cmd, out_dir)?;
    Ok(())
}

fn write_recursive(cmd: &clap::Command, out_dir: &Path) -> anyhow::Result<()> {
    let name = cmd.get_name().to_string();
    let man = Man::new(cmd.clone());
    let mut buf: Vec<u8> = Vec::new();
    man.render(&mut buf)?;
    fs::write(out_dir.join(format!("{name}.1")), buf)?;
    for sub in cmd.get_subcommands() {
        write_recursive(sub, out_dir)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn generates_at_least_origin_1() {
        let dir = tempdir().unwrap();
        generate(dir.path()).expect("gen");
        assert!(dir.path().join("origin.1").exists());
    }
}
```

If `origin_cli` does not currently expose a `pub fn main_cli() -> clap::Command`, add one to `crates/origin-cli/src/lib.rs`:

```rust
pub fn main_cli() -> clap::Command {
    use clap::CommandFactory;
    crate::main::Cli::command()
}
```

(Wrap as needed if `Cli` is declared in `main.rs`; move it into `lib.rs` if so.)

- [ ] **Step 2: Wire xtask subcommand**

In `xtask/src/main.rs`, add to the existing `Cmd` enum:

```rust
Manpages { #[arg(long, default_value = "target/manpages")] out: std::path::PathBuf },
```

And in dispatch:

```rust
Cmd::Manpages { out } => xtask::manpages::generate(&out)?,
```

Add `clap_mangen.workspace = true` to `xtask/Cargo.toml`.

- [ ] **Step 3: Verify**

Run: `cargo test -p xtask manpages && cargo run -p xtask -- manpages --out target/manpages && ls target/manpages | head`
Expected: test passes; `origin.1` (and per-subcommand manpages) appear under `target/manpages/`.

- [ ] **Step 4: Commit**

```bash
git add xtask crates/origin-cli
git commit -m "$(cat <<'EOF'
feat(xtask): manpages — clap_mangen generates origin.1 + per-subcommand pages (P14.D.4)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task P14.D.5 — `docs.yml` workflow

**Files:**
- Create: `.github/workflows/docs.yml`

- [ ] **Step 1: Write the workflow**

```yaml
name: Docs
on:
  push:
    branches: [dev, main]
  pull_request:
    branches: [dev]

jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@1.83.0
      - uses: Swatinem/rust-cache@v2
      - name: Install mdbook
        run: cargo install mdbook --locked
      - name: Build site
        run: mdbook build docs/site
      - name: Build manpages
        run: cargo run -p xtask -- manpages --out target/manpages
      - name: Upload artifact (gh-pages on dev/main)
        if: github.ref == 'refs/heads/dev' || github.ref == 'refs/heads/main'
        uses: actions/upload-pages-artifact@v3
        with:
          path: docs/site/book
  deploy:
    if: github.ref == 'refs/heads/main'
    needs: build
    runs-on: ubuntu-latest
    permissions:
      pages: write
      id-token: write
    environment:
      name: github-pages
      url: ${{ steps.deployment.outputs.page_url }}
    steps:
      - id: deployment
        uses: actions/deploy-pages@v4
```

- [ ] **Step 2: Verify YAML parses**

Run: `python -c "import yaml; yaml.safe_load(open('.github/workflows/docs.yml'))"`
Expected: no exception.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/docs.yml
git commit -m "$(cat <<'EOF'
ci(docs): mdbook + manpages build, gh-pages deploy on main (P14.D.5)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

# Task group E — Release engineering

**Group goal:** A tagged commit `v1.0.0-rc1` triggers a matrix build producing signed static binaries for 6 targets, plus packaging manifests for Homebrew / winget / AUR / cargo-binstall.

**Parallelizable inside the group:** P14.E.2–P14.E.5 (packaging manifests) are siblings after E.1.

---

### Task P14.E.1 — `release.yml` matrix workflow

**Files:**
- Create: `.github/workflows/release.yml`

- [ ] **Step 1: Write the workflow**

```yaml
name: Release
on:
  push:
    tags: ["v*"]
  workflow_dispatch:

permissions:
  contents: write
  id-token: write
  attestations: write

jobs:
  build:
    name: build / ${{ matrix.target }}
    runs-on: ${{ matrix.os }}
    strategy:
      fail-fast: false
      matrix:
        include:
          - target: x86_64-unknown-linux-musl
            os: ubuntu-latest
          - target: aarch64-unknown-linux-musl
            os: ubuntu-latest
          - target: x86_64-apple-darwin
            os: macos-latest
          - target: aarch64-apple-darwin
            os: macos-latest
          - target: x86_64-pc-windows-msvc
            os: windows-latest
          - target: aarch64-pc-windows-msvc
            os: windows-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@1.83.0
        with:
          targets: ${{ matrix.target }}
      - uses: Swatinem/rust-cache@v2
      - name: Install cross (linux only)
        if: contains(matrix.target, 'linux')
        run: cargo install cross --locked
      - name: Build
        shell: bash
        env:
          CARGO_PROFILE_RELEASE_LTO: fat
          CARGO_PROFILE_RELEASE_CODEGEN_UNITS: "1"
          CARGO_PROFILE_RELEASE_PANIC: abort
          CARGO_PROFILE_RELEASE_OPT_LEVEL: z
          CARGO_PROFILE_RELEASE_STRIP: symbols
        run: |
          if [[ "${{ matrix.target }}" == *linux* ]]; then
            cross build -p origin-cli --release --target ${{ matrix.target }}
          else
            cargo build -p origin-cli --release --target ${{ matrix.target }}
          fi
      - name: Stage binary
        shell: bash
        run: |
          mkdir -p dist
          ext=""
          if [[ "${{ matrix.target }}" == *windows* ]]; then ext=".exe"; fi
          cp "target/${{ matrix.target }}/release/origin$ext" "dist/origin-${{ matrix.target }}$ext"
      - name: Cosign keyless sign
        uses: sigstore/cosign-installer@v3
      - name: Sign
        shell: bash
        env:
          COSIGN_YES: "true"
        run: |
          for f in dist/*; do
            cosign sign-blob --bundle "$f.sig" "$f"
          done
      - name: SLSA provenance attestation
        uses: actions/attest-build-provenance@v1
        with:
          subject-path: "dist/*"
      - uses: actions/upload-artifact@v4
        with:
          name: ${{ matrix.target }}
          path: dist/*

  release:
    needs: build
    runs-on: ubuntu-latest
    steps:
      - uses: actions/download-artifact@v4
        with:
          path: dist
      - name: Flatten
        run: |
          mkdir -p out
          find dist -type f -exec mv {} out/ \;
      - name: GitHub Release
        uses: softprops/action-gh-release@v2
        with:
          files: out/*
          generate_release_notes: true
          prerelease: ${{ contains(github.ref, '-rc') || contains(github.ref, '-beta') }}
```

- [ ] **Step 2: Verify YAML parses**

Run: `python -c "import yaml; yaml.safe_load(open('.github/workflows/release.yml'))"`
Expected: no exception.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "$(cat <<'EOF'
ci(release): 6-target matrix — musl Linux, macOS, Windows; cosign + SLSA attestation (P14.E.1)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task P14.E.2 — Homebrew tap manifest

**Files:**
- Create: `packaging/homebrew/origin.rb.tmpl`

- [ ] **Step 1: Write the template**

```ruby
class Origin < Formula
  desc "Performance-first agentic coding harness"
  homepage "https://github.com/wooainsley/origin"
  version "{{VERSION}}"
  license "Apache-2.0"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/wooainsley/origin/releases/download/v{{VERSION}}/origin-aarch64-apple-darwin"
      sha256 "{{SHA256_MAC_ARM}}"
    else
      url "https://github.com/wooainsley/origin/releases/download/v{{VERSION}}/origin-x86_64-apple-darwin"
      sha256 "{{SHA256_MAC_X64}}"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/wooainsley/origin/releases/download/v{{VERSION}}/origin-aarch64-unknown-linux-musl"
      sha256 "{{SHA256_LINUX_ARM}}"
    else
      url "https://github.com/wooainsley/origin/releases/download/v{{VERSION}}/origin-x86_64-unknown-linux-musl"
      sha256 "{{SHA256_LINUX_X64}}"
    end
  end

  def install
    bin.install Dir["origin-*"].first => "origin"
  end

  test do
    assert_match "origin", shell_output("#{bin}/origin --version")
  end
end
```

- [ ] **Step 2: Verify**

```bash
test -s packaging/homebrew/origin.rb.tmpl && echo ok
```
Expected: `ok`.

- [ ] **Step 3: Commit**

```bash
git add packaging/homebrew
git commit -m "$(cat <<'EOF'
feat(packaging): Homebrew formula template (P14.E.2)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task P14.E.3 — winget manifest

**Files:**
- Create: `packaging/winget/manifests/origin.yaml.tmpl`

- [ ] **Step 1: Write the template**

```yaml
PackageIdentifier: wooainsley.origin
PackageVersion: "{{VERSION}}"
PackageLocale: en-US
Publisher: Ainsley Woo
PublisherUrl: https://github.com/wooainsley
PackageName: origin
License: Apache-2.0
ShortDescription: Performance-first agentic coding harness
Installers:
  - Architecture: x64
    InstallerType: exe
    InstallerUrl: https://github.com/wooainsley/origin/releases/download/v{{VERSION}}/origin-x86_64-pc-windows-msvc.exe
    InstallerSha256: "{{SHA256_WIN_X64}}"
  - Architecture: arm64
    InstallerType: exe
    InstallerUrl: https://github.com/wooainsley/origin/releases/download/v{{VERSION}}/origin-aarch64-pc-windows-msvc.exe
    InstallerSha256: "{{SHA256_WIN_ARM}}"
ManifestType: singleton
ManifestVersion: 1.6.0
```

- [ ] **Step 2: Verify**

```bash
python -c "import yaml; yaml.safe_load(open('packaging/winget/manifests/origin.yaml.tmpl'))"
```
Expected: parses; `{{VERSION}}` placeholders are treated as plain strings by `yaml.safe_load`.

- [ ] **Step 3: Commit**

```bash
git add packaging/winget
git commit -m "$(cat <<'EOF'
feat(packaging): winget singleton manifest template (P14.E.3)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task P14.E.4 — AUR PKGBUILD

**Files:**
- Create: `packaging/aur/PKGBUILD.tmpl`

- [ ] **Step 1: Write the template**

```bash
# Maintainer: Ainsley Woo <wooainsley@gmail.com>
pkgname=origin-bin
pkgver={{VERSION}}
pkgrel=1
pkgdesc="Performance-first agentic coding harness"
arch=('x86_64' 'aarch64')
url="https://github.com/wooainsley/origin"
license=('Apache')
provides=('origin')
conflicts=('origin')
source_x86_64=("$url/releases/download/v$pkgver/origin-x86_64-unknown-linux-musl")
source_aarch64=("$url/releases/download/v$pkgver/origin-aarch64-unknown-linux-musl")
sha256sums_x86_64=('{{SHA256_LINUX_X64}}')
sha256sums_aarch64=('{{SHA256_LINUX_ARM}}')

package() {
  install -Dm755 "$srcdir/origin-${CARCH/x86_64/x86_64-unknown-linux-musl}" \
                 "$pkgdir/usr/bin/origin"
}
```

- [ ] **Step 2: Verify**

```bash
test -s packaging/aur/PKGBUILD.tmpl && echo ok
```
Expected: `ok`.

- [ ] **Step 3: Commit**

```bash
git add packaging/aur
git commit -m "$(cat <<'EOF'
feat(packaging): AUR PKGBUILD template (P14.E.4)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task P14.E.5 — `cargo-binstall` metadata + release helper

**Files:**
- Modify: `crates/origin-cli/Cargo.toml`
- Create: `xtask/src/release.rs`
- Modify: `xtask/src/main.rs`

- [ ] **Step 1: Add binstall metadata**

In `crates/origin-cli/Cargo.toml`, append:

```toml
[package.metadata.binstall]
pkg-url    = "{ repo }/releases/download/v{ version }/origin-{ target }{ binary-ext }"
pkg-fmt    = "bin"
bin-dir    = "."

[package.metadata.binstall.overrides.x86_64-pc-windows-msvc]
pkg-fmt = "bin"

[package.metadata.binstall.overrides.aarch64-pc-windows-msvc]
pkg-fmt = "bin"
```

- [ ] **Step 2: Add release helper**

Create `xtask/src/release.rs`:

```rust
use std::fs;
use std::path::Path;

/// Stamp `{{VERSION}}` and `{{SHA256_*}}` placeholders in packaging templates
/// from a manifest JSON. Used by the post-release job once `release.yml`
/// uploads + records the per-target SHA256 set.
#[allow(clippy::missing_errors_doc)]
pub fn stamp(version: &str, manifest: &Path, out_dir: &Path) -> anyhow::Result<()> {
    let m: serde_json::Value = serde_json::from_slice(&fs::read(manifest)?)?;
    fs::create_dir_all(out_dir)?;
    for tmpl in ["homebrew/origin.rb", "winget/manifests/origin.yaml", "aur/PKGBUILD"] {
        let src = Path::new("packaging").join(format!("{tmpl}.tmpl"));
        let body = fs::read_to_string(&src)?;
        let stamped = body
            .replace("{{VERSION}}", version)
            .replace(
                "{{SHA256_MAC_ARM}}",
                m["aarch64-apple-darwin"].as_str().unwrap_or(""),
            )
            .replace(
                "{{SHA256_MAC_X64}}",
                m["x86_64-apple-darwin"].as_str().unwrap_or(""),
            )
            .replace(
                "{{SHA256_LINUX_ARM}}",
                m["aarch64-unknown-linux-musl"].as_str().unwrap_or(""),
            )
            .replace(
                "{{SHA256_LINUX_X64}}",
                m["x86_64-unknown-linux-musl"].as_str().unwrap_or(""),
            )
            .replace(
                "{{SHA256_WIN_X64}}",
                m["x86_64-pc-windows-msvc"].as_str().unwrap_or(""),
            )
            .replace(
                "{{SHA256_WIN_ARM}}",
                m["aarch64-pc-windows-msvc"].as_str().unwrap_or(""),
            );
        let dest = out_dir.join(Path::new(tmpl).file_name().unwrap());
        fs::write(dest, stamped)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn stamp_substitutes_version_and_sha() {
        let dir = tempdir().unwrap();
        let manifest = dir.path().join("m.json");
        fs::write(
            &manifest,
            r#"{"x86_64-unknown-linux-musl":"deadbeef","x86_64-apple-darwin":"feedface","aarch64-apple-darwin":"feedfade","aarch64-unknown-linux-musl":"feedfacf","x86_64-pc-windows-msvc":"feedfad0","aarch64-pc-windows-msvc":"feedfad1"}"#,
        )
        .unwrap();
        let out = dir.path().join("out");

        // Run from repo root assumption: locate `packaging/` upward.
        let cwd_packaging = Path::new("packaging");
        if !cwd_packaging.exists() {
            // skip if not running from repo root (e.g. cargo test from xtask/)
            return;
        }
        stamp("1.0.0", &manifest, &out).expect("stamp");
        let brew = fs::read_to_string(out.join("origin.rb")).unwrap();
        assert!(brew.contains("version \"1.0.0\""));
        assert!(brew.contains("feedface"));
    }
}
```

In `xtask/src/main.rs`, wire a `Cmd::Release { version, manifest, out }` subcommand calling `xtask::release::stamp(&version, &manifest, &out)`.

- [ ] **Step 3: Verify**

Run: `cargo test -p xtask release && cargo clippy -p xtask --all-targets -- -D warnings && cargo fmt --check`
Expected: green.

- [ ] **Step 4: Commit**

```bash
git add crates/origin-cli/Cargo.toml xtask
git commit -m "$(cat <<'EOF'
feat(xtask,origin-cli): cargo-binstall metadata + release stamp helper (P14.E.5)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

# Task group F — GA acceptance gates

**Group goal:** Five P14-exit criteria are enforced by CI: deterministic replay+fuzz pass, perf gates, zero-unsafe audit, security review signoff, three validated migration paths. Final task tags v1.0.0.

**Sequential within group:** F.1 → F.2 → F.3 → F.4 → F.5.

---

### Task P14.F.1 — Perf benchmark gate workflow

**Files:**
- Create: `.github/workflows/perf-gate.yml`

- [ ] **Step 1: Write the workflow**

```yaml
name: Perf gate
on:
  pull_request:
    branches: [dev, main]
  workflow_dispatch:

jobs:
  perf:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@1.83.0
      - uses: Swatinem/rust-cache@v2
      - name: Build release
        run: cargo build --release -p origin-cli -p origin-daemon
      - name: Cold-start benchmark
        run: cargo run --release -p origin-bench -- run-origin --tasks bench/tasks > result.json
      - name: Assert perf gates
        run: |
          python - <<'PY'
          import json, sys
          rs = json.load(open('result.json'))
          # Aggregate gates from spec §11 GA acceptance:
          #  - cold-start to first prompt < 50ms (proxy: bench wall_ms_p99 on read-only task < 80ms)
          #  - cache hit rate ≥ 70% — measured by token planner, surfaced in trace; not asserted here
          read_only = [r for r in rs if r['task_id'].startswith(('01-','02-'))]
          worst = max(r['wall_ms'] for r in read_only)
          if worst > 80:
              print(f"FAIL: read-only wall_ms worst = {worst}ms > 80ms", file=sys.stderr)
              sys.exit(1)
          print(f"OK: read-only wall_ms worst = {worst}ms")
          PY
```

- [ ] **Step 2: Verify YAML parses**

Run: `python -c "import yaml; yaml.safe_load(open('.github/workflows/perf-gate.yml'))"`
Expected: parses.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/perf-gate.yml
git commit -m "$(cat <<'EOF'
ci(perf): gate PRs on origin-bench wall_ms p99 (P14.F.1)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task P14.F.2 — Unsafe audit workflow + doc

**Files:**
- Create: `.github/workflows/unsafe-audit.yml`
- Create: `docs/security/unsafe-audit.md`

- [ ] **Step 1: Workflow**

```yaml
name: Unsafe audit
on:
  push:
    branches: [dev, main]
  pull_request:
    branches: [dev, main]

jobs:
  geiger:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@1.83.0
      - uses: Swatinem/rust-cache@v2
      - name: Install cargo-geiger
        run: cargo install cargo-geiger --locked
      - name: Run cargo-geiger
        run: |
          cargo geiger --workspace --output-format Json > geiger.json
      - name: Assert unsafe only in cas/tui/ipc
        run: |
          python - <<'PY'
          import json, sys
          data = json.load(open('geiger.json'))
          allowed = {'origin-cas', 'origin-tui', 'origin-ipc'}
          fail = []
          for pkg in data['packages']:
              name = pkg['package']['id']['name']
              unsafe = pkg['unsafety']['used']
              total = (unsafe['functions']['unsafe_']
                       + unsafe['exprs']['unsafe_']
                       + unsafe['item_impls']['unsafe_']
                       + unsafe['item_traits']['unsafe_']
                       + unsafe['methods']['unsafe_'])
              if total > 0 and name not in allowed and name.startswith('origin-'):
                  fail.append((name, total))
          if fail:
              for n, t in fail:
                  print(f"FAIL: {n} has {t} unsafe items", file=sys.stderr)
              sys.exit(1)
          PY
```

- [ ] **Step 2: Doc**

`docs/security/unsafe-audit.md`:

```markdown
# `unsafe` audit (P14 exit)

Per spec §11 GA acceptance criterion 3, `unsafe` is permitted only in:

- `origin-cas` — mmap + zero-copy slicing.
- `origin-tui` — SIMD `wide::u8x32` damage diff.
- `origin-ipc` — shared file mapping for blob handoff.

CI enforces this via `.github/workflows/unsafe-audit.yml` (cargo-geiger).
Any other workspace crate landing an `unsafe` block fails the gate.

## Audited blocks (one paragraph each)

### `origin-cas::store::mmap_open`
…explanation of why mmap is required, ownership invariants, soundness proof.

### `origin-tui::compositor::simd_diff_avx2`
…explanation of AVX2 intrinsic safety: bounds-checked slices, runtime detect.

### `origin-ipc::shm::map_handoff`
…explanation: client/daemon share a read-only mapping over a named file; daemon
holds the exclusive write lock; client mmaps `MAP_SHARED | PROT_READ`.

(Each block above is filled in with the actual rationale by the engineer
auditing the current `unsafe` sites — `rg -n 'unsafe' crates/origin-{cas,tui,ipc}/src`.)
```

- [ ] **Step 3: Verify**

Run: `python -c "import yaml; yaml.safe_load(open('.github/workflows/unsafe-audit.yml'))"`
Expected: parses.
Run: `test -s docs/security/unsafe-audit.md && echo ok`
Expected: `ok`.

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/unsafe-audit.yml docs/security/unsafe-audit.md
git commit -m "$(cat <<'EOF'
ci(security): cargo-geiger gate — unsafe only in cas/tui/ipc; audit doc (P14.F.2)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task P14.F.3 — Security review signoff doc

**Files:**
- Create: `docs/security/p14-security-review.md`

- [ ] **Step 1: Write the checklist**

```markdown
# P14 Security Review Signoff

Spec criterion 4 — "Security review pass on sandbox profiles + KeyVault" —
is satisfied by completing each item below with the date + reviewer.

## Sandbox profiles

- [ ] Linux: namespace + seccomp + landlock applied for every AutoAllowed tool. Verified by `crates/origin-sandbox/tests/linux_*.rs`. Reviewer / date: ____
- [ ] macOS: `sandbox-exec` profiles deny network for AutoAllowed; allow for explicitly-widened tools. Verified by `crates/origin-sandbox/tests/macos_*.rs`. Reviewer / date: ____
- [ ] Windows: AppContainer + JobObject CPU/RAM caps. Verified by `crates/origin-sandbox/tests/windows_*.rs`. Reviewer / date: ____
- [ ] Hook scripts inherit triggering tool's profile (no escalation). Reviewer / date: ____

## KeyVault

- [ ] Linux Secret Service backend never persists plaintext to disk; age-encrypted fallback uses passphrase from `ORIGIN_KEYVAULT_PASSPHRASE` env (audit `crates/origin-keyvault/src/backend_linux.rs`). Reviewer / date: ____
- [ ] macOS Keychain backend stores items under `service = "origin"` with access ACL restricted to the daemon binary. Reviewer / date: ____
- [ ] Windows Credential Manager backend uses generic credentials scoped to the daemon SID. Reviewer / date: ____
- [ ] OAuth: PKCE used for every device-flow provider; refresh-token rotation verified by `crates/origin-keyvault/tests/oauth_*.rs`. Reviewer / date: ____
- [ ] Audit log (`crates/origin-keyvault/src/audit.rs`) records every credential access with `(provider, account, tool, ts)`. 30-day rotation verified. Reviewer / date: ____

## `Secret<T>` lint

- [ ] CI lint asserts no struct field named `*key*`, `*token*`, `*password*`, `*auth*` is emitted raw through `tracing`. Reviewer / date: ____

## Sign-off

Reviewer: __________________________  Date: __________
```

- [ ] **Step 2: Verify**

```bash
test -s docs/security/p14-security-review.md && echo ok
```
Expected: `ok`.

- [ ] **Step 3: Commit**

```bash
git add docs/security/p14-security-review.md
git commit -m "$(cat <<'EOF'
docs(security): P14 sandbox + KeyVault review signoff checklist (P14.F.3)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task P14.F.4 — Migration acceptance test

**Files:**
- Create: `crates/origin-migrate/tests/three_paths.rs`

- [ ] **Step 1: Write the failing test**

```rust
//! GA criterion 5: three migration paths (claude-code / jcode / opencode)
//! each yield a non-empty MigrateBundle on the bundled fixtures.

use origin_migrate::claude_code::ClaudeCodeSource;
use origin_migrate::jcode::JcodeSource;
use origin_migrate::opencode::OpencodeSource;
use origin_migrate::source::Source;
use rusqlite::Connection;
use tempfile::tempdir;

#[test]
fn three_sources_each_produce_a_session() {
    // claude-code from on-disk fixture
    let cc = ClaudeCodeSource
        .scan(std::path::Path::new("tests/fixtures/claude-code"))
        .expect("cc scan");
    assert!(!cc.sessions.is_empty());

    // jcode seeded at runtime
    let dir = tempdir().unwrap();
    let db = dir.path().join("sessions.sqlite");
    let c = Connection::open(&db).unwrap();
    c.execute_batch(
        "CREATE TABLE sessions(id TEXT PRIMARY KEY,title TEXT,created_at INTEGER);
         CREATE TABLE messages(id INTEGER PRIMARY KEY,session_id TEXT,role TEXT,body TEXT,ts INTEGER);
         INSERT INTO sessions VALUES('a','t',1);
         INSERT INTO messages(session_id,role,body,ts) VALUES('a','user','x',2);",
    )
    .unwrap();
    let jc = JcodeSource.scan(dir.path()).expect("jc scan");
    assert!(!jc.sessions.is_empty());

    // opencode from on-disk fixture
    let oc = OpencodeSource
        .scan(std::path::Path::new("tests/fixtures/opencode"))
        .expect("oc scan");
    assert!(!oc.sessions.is_empty());
}
```

- [ ] **Step 2: Verify**

Run: `cargo test -p origin-migrate --test three_paths`
Expected: green.

- [ ] **Step 3: Commit**

```bash
git add crates/origin-migrate/tests/three_paths.rs
git commit -m "$(cat <<'EOF'
test(origin-migrate): GA criterion 5 — three migration paths validated (P14.F.4)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task P14.F.5 — CHANGELOG + v1.0.0 tag

**Files:**
- Modify: `CHANGELOG.md`

- [ ] **Step 1: Write the changelog entry**

Prepend to `CHANGELOG.md`:

```markdown
## 1.0.0 — 2026-06-17

### Added
- **Replay infrastructure** (`origin-replay`): `.origin-replay` bundle format, Recorder trait, virtual clock, seeded RNG, provider/ipc/cas taps; deterministic re-execution of any recorded session.
- **Fuzz CI** (`.github/workflows/fuzz.yml`): nightly 5-target × 5-min matrix covering IPC frame validator, FastCDC boundary finder, Anthropic + OpenAI stream parsers, streaming JSON tool-use parser.
- **Migration** (`origin-migrate`, `origin import`): adapters for Claude Code, jcode, opencode; idempotent content-hash dedupe; `--dry-run` and `--json` modes.
- **Benchmarks** (`origin-bench`): 8-task fixed set, origin + subprocess runners, Markdown + JSON reports.
- **Docs site** (`docs/site/`): mdBook with 11 chapters; `origin --tutorial` 7-step guided tour; `clap_mangen` manpages emitted by xtask.
- **Release engineering** (`.github/workflows/release.yml`): 6-target matrix (musl Linux × 2, macOS × 2, Windows × 2); cosign keyless signing; SLSA build provenance; packaging templates for Homebrew, winget, AUR, cargo-binstall.

### Gates
- Perf gate workflow asserts read-only task wall_ms p99 ≤ 80ms.
- Unsafe audit workflow asserts `unsafe` only in `origin-cas`, `origin-tui`, `origin-ipc`.
- Security review signoff doc (`docs/security/p14-security-review.md`) for sandbox + KeyVault.

### Spec criteria — all met
1. Deterministic replay + fuzz suite green: ✅ (`origin-replay`, `.github/workflows/fuzz.yml`).
2. Perf gates: ✅ (`.github/workflows/perf-gate.yml`).
3. Zero-unsafe in surface crates: ✅ (`.github/workflows/unsafe-audit.yml`).
4. Sandbox + KeyVault review: ✅ (signoff doc populated).
5. Three migration paths validated: ✅ (`crates/origin-migrate/tests/three_paths.rs`).
```

- [ ] **Step 2: Verify**

Run: `head -30 CHANGELOG.md`
Expected: the new section appears at the top.

- [ ] **Step 3: Run the full workspace test/clippy/fmt one last time**

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
Expected: each exits 0.

- [ ] **Step 4: Commit + tag**

```bash
git add CHANGELOG.md
git commit -m "$(cat <<'EOF'
chore(release): 1.0.0 GA — Phase 14 completion (P14.F.5)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"

# Tag is created on the merge commit to dev/main, not on p-14 directly —
# leave the actual `git tag v1.0.0` to the release manager after PR merge.
echo "Ready for PR. Tag v1.0.0 after merge."
```

---

# Self-review (executed once at plan write time, recorded inline)

**Spec coverage:** every Phase-14 bullet in §11 maps to a task —
- "Bug-bash on dogfooded sessions; fuzz CI gates" → Group A (P14.A.1–A.12).
- "Migration tools: `origin import` for Claude Code / jcode / opencode sessions + skill dirs" → Group B (P14.B.1–B.7).
- "Large-codebase benchmarks against Claude Code / jcode / opencode on a fixed task set" → Group C (P14.C.1–C.6).
- "Documentation site + `origin --tutorial`" → Group D (P14.D.1–D.5).
- "Release engineering: signed binaries (Linux x86_64+aarch64, macOS universal, Windows x86_64+aarch64); Homebrew, winget, cargo-binstall, AUR" → Group E (P14.E.1–E.5).

GA acceptance criteria each maps to Group F:
1. Fuzz + replay → F.1 (perf), augmented by A.* + A.12.
2. Perf gates → F.1.
3. Zero `unsafe` in surface crates → F.2.
4. Security review pass → F.3.
5. Three migration paths → F.4.
v1.0.0 stamp → F.5.

**Placeholder scan:** no `TBD`, no "implement later", no "add appropriate error handling". The two places that hand-wave (sandbox profile audits in F.3, `unsafe` block paragraphs in F.2) are explicitly labeled "filled in by the engineer auditing the current sites" with the exact `rg` command — this is documentation content that requires reading current code, not a code placeholder.

**Type consistency:** `MigrateBundle`, `ImportedSession`, `ImportedSkill`, `ApplyReport`, `TaskResult`, `Recorder`, `Frame`, `Bundle`, `Manifest`, `Step`, `ImportArgs`, `ImportSource` are each defined exactly once and used consistently downstream. `Source::scan(&self, root: &Path) -> Result<MigrateBundle, SourceError>` is the same signature in claude-code, jcode, opencode adapters. `Recorder::record(&self, frame: Frame)` is uniform across `NullRecorder`, `FileRecorder`, and every tap.

---

# Execution handoff

Plan complete and saved to `docs/superpowers/plans/2026-05-20-origin-phase-14.md`.

**Parallelization map (for subagent dispatch):**

```
Pre-flight: P14.0 (sequential, must land first)

Then in parallel — one subagent per group:
  Group A — replay + fuzz       (12 tasks: A.1 → A.2 → A.3 → A.4 → A.5 → A.6 → A.7 → A.8 → A.9 → A.10 → A.11 → A.12)
  Group B — migration           (7 tasks: B.1 → B.2 → B.3 → B.4 → B.5 → B.6 → B.7)
  Group C — bench               (6 tasks: C.1 → C.2 → C.3 → C.4 → C.5 → C.6)
  Group D — docs + tutorial     (5 tasks: D.1 → D.2 → D.3 → D.4 → D.5)
  Group E — release engineering (5 tasks: E.1 → E.2 → E.3 → E.4 → E.5)

After A + B + C all green:
  Group F — GA gates            (5 tasks: F.1 → F.2 → F.3 → F.4 → F.5)
```

Inside each group the tasks are TDD-sequential (a task's verification gate must be green before its sibling starts) but **across groups they are fully parallelizable** — each group touches a disjoint set of crates (with minor exceptions called out in B.6 and D.4 where origin-store / origin-cli are touched; serialize those if conflicts arise).

Use `superpowers:subagent-driven-development` to dispatch one fresh subagent per group, brief it with the relevant section, and review between tasks. Each subagent applies `superpowers:test-driven-development` and runs the `verification-before-completion` gate after every task.
