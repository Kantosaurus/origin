# Changelog

All notable changes to `origin` will be documented in this file.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) loosely;
versions correspond to phase milestones from the implementation plan.

## [phase-0] — 2026-05-19

### Added

- **Workspace scaffold** — Cargo workspace at `crates/*`, Rust 1.83.0 toolchain pin,
  workspace lint policy (`unsafe_code = forbid`, `unwrap_used = deny`,
  `clippy::pedantic + nursery` warns).
- **Cross-platform CI** — `.github/workflows/ci.yml` running fmt + clippy + test on
  Ubuntu, macOS, and Windows.
- **`origin-core`** — Pure type crate.
  - `Role` (User / Assistant / Tool / System) with rkyv archive + check_bytes.
  - `MessageId` (ULID wrapper) and `TurnIndex` (overflow-safe `checked_add`).
  - `Block` enum (Text / ToolUse / ToolResult / Thinking) with optional cache markers.
  - `Message` (role + Vec<Block>).
  - `ProviderCaps` IR scaffold (const-constructible).
- **`origin-ipc`** — IPC transport crate.
  - Wire frame with magic, 17-byte header, length-prefixed body, validation against
    truncation / bad magic / unknown kind / length mismatch.
  - Cross-platform local-socket transport (Unix domain sockets / Windows named pipes)
    via `interprocess` crate.
- **`origin-store`** — SQLite persistence with `refinery` migrations.
  - WAL mode, V1 schema for `sessions` + `messages`.
- **`origin-daemon`** + **`origin-cli`** (binary `origin`) — Echo handshake over the
  IPC transport, verified end-to-end on Windows.

### Architecture milestones reached

- Daemon process, IPC transport, and client binary separated by crate boundary —
  same code shape will support multi-client and future remote/desktop surfaces.
- All types are `rkyv::Archive` from day 1 so the same bytes can flow through IPC,
  storage, and in-memory ring buffers without serialize/deserialize hops.
- 18 tests passing across the workspace; property tests for frame encoding.
