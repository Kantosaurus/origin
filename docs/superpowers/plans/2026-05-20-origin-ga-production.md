# Origin GA Production-Readiness Plan (Post-1.0.0)

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` to implement this plan task-by-task. Each task ends with a `superpowers:verification-before-completion` gate; do NOT move on until verification is green. Use `superpowers:test-driven-development` discipline — failing test first, then minimum impl, then verify, then commit.

**Goal:** Take `origin` from its current Phase 14 GA tag (`1.0.0`) to a fully wired, fully tested production state by (a) finishing the multi-provider expansion already in flight, (b) closing every remaining TODO/FIXME in the codebase, and (c) committing/stabilising in-progress uncommitted work.

**Architecture (no change):** Daemon + CLI workspace, catalog-driven providers, three-tier CAS, sandboxed sidecar, encrypted KeyVault. The plan is consolidation, not redesign.

**Tech Stack (no change):** Rust 1.83 MSRV pinned, Tokio, rusqlite, rkyv, reqwest+rustls, wiremock for HTTP tests.

**Companion plan:** `docs/superpowers/plans/2026-05-20-multi-provider-expansion.md` already specifies tasks 1-24 for the multi-provider expansion. Tasks 1-3 are already merged. This plan picks up at task 4 and adds three TODO-cleanup tasks plus an in-progress-work stabilisation task.

---

## Conventions (apply to every task)

**TDD shape:** failing test → run (confirm fail) → minimum impl → run (confirm pass) → verification gate → commit.

**Verification gate (default):**
```bash
cargo test -p <crate>
cargo clippy -p <crate> -- -D warnings
cargo fmt --check
```
**Workspace gate** (final integration only):
```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

**Commit style:** Conventional commits, scope to crate where possible. Co-author Claude.

**Parallelism note:** Tasks A1, A2, A3, B are independent and can run in parallel subagents. C tasks are sequential (depend on previous catalog work). D depends on everything else.

---

## Task A1 — Streaming `index` propagation (3 TODOs)

**Files:**
- Modify: `crates/origin-provider-anthropic/src/streaming.rs:124`
- Modify: `crates/origin-provider-openai/src/streaming.rs:78,107`
- Modify: `crates/origin-provider-gemini/src/streaming.rs:63,66`
- Test: `crates/origin-provider-anthropic/tests/streaming_index.rs`, `crates/origin-provider-openai/tests/streaming_index.rs`, `crates/origin-provider-gemini/tests/streaming_usage_final.rs`

### Step 1: Failing tests

`crates/origin-provider-anthropic/tests/streaming_index.rs`:
```rust
//! When the wire emits two concurrent `content_block_start` events with
//! different `index` values, parse_into_ring must annotate each frame
//! with its index so the daemon can route deltas to the right block.
use origin_provider_anthropic::streaming::parse_chunk_for_test;

#[test]
fn parses_index_on_content_block_start() {
    let line = br#"data: {"type":"content_block_start","index":2,"content_block":{"type":"text","text":""}}"#;
    let evt = parse_chunk_for_test(line).expect("frame");
    assert_eq!(evt.index, Some(2));
}

#[test]
fn parses_index_on_tool_use_delta() {
    let line = br#"data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"x\":1}"}}"#;
    let evt = parse_chunk_for_test(line).expect("frame");
    assert_eq!(evt.index, Some(1));
}
```

`crates/origin-provider-openai/tests/streaming_index.rs`:
```rust
use origin_provider_openai::streaming::parse_chunk_for_test;

#[test]
fn openai_tool_call_index_preserved() {
    let line = br#"data: {"choices":[{"delta":{"tool_calls":[{"index":1,"id":"call_a","function":{"name":"r","arguments":"{}"}}]}}]}"#;
    let evt = parse_chunk_for_test(line).expect("frame");
    assert_eq!(evt.index, Some(1));
}

#[test]
fn openai_usage_when_include_usage_set() {
    let line = br#"data: {"choices":[],"usage":{"prompt_tokens":7,"completion_tokens":3,"total_tokens":10}}"#;
    let evt = parse_chunk_for_test(line).expect("usage frame");
    assert_eq!(evt.usage.unwrap().input_tokens, 7);
}
```

`crates/origin-provider-gemini/tests/streaming_usage_final.rs`:
```rust
use origin_provider_gemini::streaming::parse_chunk_for_test;

#[test]
fn gemini_usage_metadata_on_final_frame() {
    let line = br#"data: {"candidates":[{"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":9,"candidatesTokenCount":4}}"#;
    let evt = parse_chunk_for_test(line).expect("final frame");
    assert_eq!(evt.usage.unwrap().input_tokens, 9);
    assert_eq!(evt.usage.unwrap().output_tokens, 4);
}
```

### Step 2: Run — confirm fail

```bash
cargo test -p origin-provider-anthropic --test streaming_index
cargo test -p origin-provider-openai --test streaming_index
cargo test -p origin-provider-gemini --test streaming_usage_final
```
Expected: compile error (missing `parse_chunk_for_test`, missing `index`/`usage` fields).

### Step 3: Implementation

For each provider:

1. Add `pub index: Option<u32>` and `pub usage: Option<Usage>` to the per-frame event struct in `streaming.rs`.
2. Extend the SSE parser to extract `.index` from `content_block_start`/`content_block_delta`/`tool_calls[].index` and set the `usage` field when the wire emits a usage block (always for Anthropic `message_delta`, on final frame for Gemini, when `stream_options.include_usage` is set for OpenAI).
3. Set `stream_options.include_usage = true` in `wire::encode_request(_, stream=true)` for OpenAI.
4. Expose `pub fn parse_chunk_for_test(line: &[u8]) -> Option<Event>` behind `#[cfg(any(test, feature = "test-util"))]` so the tests above can call the per-line parser without spinning up reqwest.
5. Delete the `TODO` and `FIXME` comments addressed.

### Step 4: Run — confirm pass

```bash
cargo test -p origin-provider-anthropic
cargo test -p origin-provider-openai
cargo test -p origin-provider-gemini
```

### Step 5: Verification gate

```bash
cargo test -p origin-provider-anthropic -p origin-provider-openai -p origin-provider-gemini
cargo clippy -p origin-provider-anthropic -p origin-provider-openai -p origin-provider-gemini -- -D warnings
cargo fmt --check
```

### Step 6: Commit

```bash
git add crates/origin-provider-anthropic crates/origin-provider-openai crates/origin-provider-gemini
git commit -m "feat(providers): preserve streaming index + usage on final frame"
```

---

## Task A2 — tool_use_parser string-state-aware nesting (N10.10 FIXME)

**Files:**
- Modify: `crates/origin-daemon/src/tool_use_parser.rs:200` (the `InNested` state)
- Test: `crates/origin-daemon/tests/tool_use_parser_nested.rs`

### Step 1: Failing test

```rust
//! When tool input JSON contains a string with `{` or `]` inside it, the
//! current parser miscounts depth and closes the nested object early.
//! Make the parser string-state aware.
use origin_daemon::tool_use_parser::feed_for_test;

#[test]
fn brace_inside_string_does_not_close_nested() {
    let chunks = [
        br#"{"x":{"y":"contains } and ] chars","#.as_slice(),
        br#""z":1}}"#.as_slice(),
    ];
    let mut p = feed_for_test();
    for c in chunks { p.feed(c); }
    let done = p.finish();
    assert_eq!(done.input_json, r#"{"x":{"y":"contains } and ] chars","z":1}}"#);
}

#[test]
fn escaped_quote_inside_string_does_not_exit_string_state() {
    let chunks = [br#"{"a":"he said \"hi\"","b":2}"#.as_slice()];
    let mut p = feed_for_test();
    for c in chunks { p.feed(c); }
    let done = p.finish();
    assert!(done.complete, "parser should mark complete after balanced braces");
}
```

### Step 2: Run — fail (missing test helper).

### Step 3: Implementation

In `tool_use_parser.rs`:
1. Extend the existing state machine with an explicit `InString { escape_next: bool }` sub-state of `InNested`.
2. On `"`, if not in string → enter string state; if in string and `!escape_next` → exit; if in string and `escape_next` → consume and reset escape flag.
3. On `\\` inside string → set `escape_next = true`.
4. Only count `{ [ ] }` toward nesting depth when **not** in string state.
5. Add `#[cfg(any(test, feature = "test-util"))] pub fn feed_for_test() -> ParserHandle` exposing `.feed(&[u8])` and `.finish() -> CompletedToolUse` for the integration test.
6. Delete the `FIXME(N10.10)` comment.

### Step 4: Run — pass.

### Step 5: Verification gate

```bash
cargo test -p origin-daemon --test tool_use_parser_nested
cargo clippy -p origin-daemon -- -D warnings
```

### Step 6: Commit

```bash
git add crates/origin-daemon
git commit -m "fix(origin-daemon): tool_use_parser string-state-aware nesting (N10.10)"
```

---

## Task A3 — `MemoryStore` wires `RefTable::decr` + Anthropic cache gate lift

**Files:**
- Modify: `crates/origin-mem/src/storage.rs:199`
- Modify: `crates/origin-provider-anthropic/src/lib.rs:227`
- Test: `crates/origin-mem/tests/refcount_decrement.rs`, `crates/origin-provider-anthropic/tests/cache_marker_multi_msg.rs`

### Step 1: Failing tests

`crates/origin-mem/tests/refcount_decrement.rs`:
```rust
//! When a memory record is dropped, MemoryStore must decrement the refcount
//! of every CAS handle the record referenced so GC can reclaim the shards.
use origin_mem::MemoryStore;
use origin_cas::Store as CasStore;

#[tokio::test]
async fn drop_record_decrements_refcount() {
    let dir = tempfile::tempdir().unwrap();
    let cas = std::sync::Arc::new(CasStore::open(dir.path()).unwrap());
    let mem = MemoryStore::open(dir.path().join("mem.db"), cas.clone()).unwrap();
    let id = mem.write_record("note", b"long body bytes that get CAS'd").await.unwrap();
    let pre = cas.refcount_for_test(&mem.handle_for_test(id)).unwrap();
    assert_eq!(pre, 1);
    mem.delete_record(id).await.unwrap();
    let post = cas.refcount_for_test(&mem.handle_for_test(id)).unwrap();
    assert_eq!(post, 0, "RefTable::decr must run on delete_record");
}
```

`crates/origin-provider-anthropic/tests/cache_marker_multi_msg.rs`:
```rust
//! After Phase 11 handle-substitution, cache markers may appear on any
//! message, not just msg_idx == 0. Confirm encode_request emits
//! cache_control on the marker block regardless of message position.
use origin_provider_anthropic::wire::encode_request_for_test;
use origin_core::types::{Block, CacheBoundary, Message, Role};
use origin_provider::ChatRequest;

#[test]
fn cache_marker_on_non_first_message_is_emitted() {
    let m0 = Message { role: Role::User, blocks: vec![Block::Text { text: "a".into(), cache_marker: None }] };
    let m1 = Message {
        role: Role::User,
        blocks: vec![Block::Text { text: "b".into(), cache_marker: Some(CacheBoundary::Sticky) }],
    };
    let req = ChatRequest { system: String::new(), messages: vec![m0, m1], model: "claude".into(), tools: vec![] };
    let body = encode_request_for_test(&req);
    let s = serde_json::to_string(&body).unwrap();
    assert!(s.contains(r#""cache_control":{"type":"ephemeral"}"#), "cache_control missing on msg 1: {s}");
}
```

### Step 2: Run — fail.

### Step 3: Implementation

In `crates/origin-mem/src/storage.rs`:
1. In `delete_record` (and any other path that drops a record), iterate the record's referenced handles and call `cas.refs().decr(handle)?` for each. Drop the `TODO(P6.x)` comment.
2. Add `#[cfg(any(test, feature = "test-util"))] pub fn handle_for_test(...) -> Hash` to expose handles for the integration test. Add `refcount_for_test` on `CasStore` similarly (if not already present).

In `crates/origin-provider-anthropic/src/lib.rs`:
1. Find the existing `if msg_idx == 0` gate near line 227.
2. Replace with logic that, for every message and every block, emits `cache_control` when `cache_marker.is_some()`, independent of position.
3. Add `#[cfg(any(test, feature = "test-util"))] pub fn encode_request_for_test(req: &ChatRequest) -> serde_json::Value` so the unit test above can verify the wire JSON.
4. Drop the `TODO(N4.3/Phase 11)` comment.

### Step 4: Run — pass.

### Step 5: Verification gate

```bash
cargo test -p origin-mem -p origin-provider-anthropic
cargo clippy -p origin-mem -p origin-provider-anthropic -- -D warnings
```

### Step 6: Commit

```bash
git add crates/origin-mem crates/origin-provider-anthropic
git commit -m "feat(origin-mem,origin-provider-anthropic): wire RefTable::decr + lift msg_idx cache gate"
```

---

## Task B — Stabilise in-progress uncommitted work

**Files:** Triage the 27 modified files + 11 untracked files currently uncommitted on `dev`. Group by subsystem:

| Subsystem | Files |
|---|---|
| Plan bus | `crates/origin-daemon/src/plan_bus.rs` (new), `crates/origin-cli/src/plan_panel_wiring.rs`, `crates/origin-plan/src/ops.rs`, `crates/origin-daemon/tests/plan_bus.rs` (new) |
| Proposal registry | `crates/origin-daemon/src/proposal_registry.rs` (new), `crates/origin-daemon/tests/memory_accept_registry.rs` (new), `crates/origin-mem/src/{consolidator,injector,lib,proposer}.rs`, related `memory_*` tests |
| Daemon config + shutdown | `crates/origin-daemon/src/{config,shutdown,main,lib}.rs`, `tests/{shutdown_phases_wired,resume_session,bearer_ttl}.rs` |
| Session persistence | `crates/origin-daemon/src/session_store.rs`, `crates/origin-store/src/lib.rs` |
| CLI surface | `crates/origin-cli/src/{admin,import,main}.rs`, `crates/origin-cli/tests/import.rs` |
| Sidecar runtime | `crates/origin-sidecar/src/runtime.rs` |
| Tool builtins | `crates/origin-tools/src/builtins/{recall,graph_explain}.rs`, `tests/{recall_outline,graph_explain}.rs` |
| Daemon protocol + agent | `crates/origin-daemon/src/{protocol,agent,memory_wiring}.rs`, related tests |
| CAS store extension | `crates/origin-cas/src/store.rs` |
| Misc | `Cargo.lock`, the two Cargo.toml files |

### Step 1: For each subsystem group, in this order, run

```bash
cargo test -p <primary_crate>
cargo clippy -p <primary_crate> -- -D warnings
```

If failing, fix in-place. If passing, commit just that group with a focused message:
```
feat(origin-daemon): plan bus + plan_panel_wiring
feat(origin-daemon,origin-mem): memory accept registry + consolidator/injector wiring
feat(origin-daemon): config loader + structured shutdown phases
feat(origin-cli): admin + import improvements
feat(origin-sidecar): runtime adjustments
feat(origin-tools): recall + graph_explain refinements
```

### Step 2: After each commit, re-run

```bash
cargo test --workspace
```

If a commit broke the workspace, fix-forward (do **not** revert blindly — investigate which crate consumes the change and update it). Sequential groups: protocol/agent depends on plan_bus + proposal_registry; commit those first.

### Step 3: Final verification

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

Expect zero failures, zero warnings, zero format diffs. Tag `git tag stabilise-pre-multi-provider`.

---

## Tasks C1–C7 — Multi-provider expansion tasks 4-24

**See:** `docs/superpowers/plans/2026-05-20-multi-provider-expansion.md` — tasks 4-24 (21 tasks, fully spec'd, with code blocks and verification per task).

Group into 6 PR-sized work units (subagent boundaries):

| Unit | Tasks (from multi-provider plan) | Dependency |
|---|---|---|
| **C1** | 4, 5, 6, 7, 8, 9 — openai-compat crate + extraction + wrappers + wiremock smoke | None |
| **C2** | 10, 11, 12, 13 — ProviderId newtype + factory rewrite + integration test | C1 |
| **C3** | 14, 15 — `~/.origin/providers.toml` loader + daemon startup | C2 |
| **C4** | 16, 17 — Anthropic OAuth + Gemini OAuth | C2 |
| **C5** | 18, 19 — OpenAI Codex (ChatGPT OAuth) + GitHub Copilot device flow | C2 |
| **C6** | 20, 21, 22, 23 — CLI `providers ls/describe`, `keyring login`, docs update, features | C3+C4+C5 |
| **C7** | 24 — final sweep (workspace test, clippy, fmt, smoke, tag) | All above |

**For each unit:** follow the per-task TDD shape already in the multi-provider plan. Commit per-task (not per-unit) so blame is clean.

---

## Task D — Final integration verification

**Files:** None new — verifies state of `dev` after all C tasks merged.

### Step 1: Full workspace test

```bash
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
```

Expect: exit 0 for all.

### Step 2: Fuzz smoke (5 min per target — the nightly CI matrix runs 5 min × 5 targets)

```bash
cargo +nightly fuzz run fastcdc_boundary -- -max_total_time=60
cargo +nightly fuzz run ipc_frame -- -max_total_time=60
cargo +nightly fuzz run anthropic_stream -- -max_total_time=60
cargo +nightly fuzz run openai_stream -- -max_total_time=60
cargo +nightly fuzz run streaming_json -- -max_total_time=60
```

Expect: no panics, no crashes.

### Step 3: Headline perf bench

```bash
cargo bench -p origin-bench -- --output-format=json | tee bench.json
```

Confirm `wall_ms p99` is at or below the GA-tagged number (the P14.F.1 CI gate's threshold).

### Step 4: Manual smoke

```bash
cargo run -p origin-daemon &
cargo run -p origin-cli -- providers ls
cargo run -p origin-cli -- prompt "list rust files in this repo"
```

Expect: providers ls shows 30+ rows; prompt produces a coherent response using Glob.

### Step 5: Tag

```bash
git tag v1.1.0 -m "GA + multi-provider + TODO cleanup"
```

### Step 6: Self-review

Re-grep for any remaining `TODO`, `FIXME`, `XXX`, `unimplemented!`, `todo!()` in `crates/`:

```bash
rg -n 'TODO|FIXME|XXX|unimplemented!|todo!\(\)' crates/
```

Expect: only acceptable references (regex literals matching the word `TODO` in `origin-mem/src/proposer.rs` for the actual TODO-detection feature). Anything else → file follow-up task or fix inline.

---

## Self-Review (writing-plans checklist)

1. **Spec coverage:** ✅ All 10 source TODOs mapped to A1/A2/A3. ✅ All 21 remaining multi-provider tasks mapped to C1-C7 by reference. ✅ All 27 modified + 11 untracked files mapped to Task B groups.
2. **Placeholder scan:** No `TBD`/`fill in details` in this plan. Every test in A1/A2/A3 has full code. C1-C7 reference the existing multi-provider plan which has full code per task.
3. **Type consistency:** `parse_chunk_for_test`, `feed_for_test`, `handle_for_test`, `refcount_for_test`, `encode_request_for_test` are introduced uniformly as `#[cfg(any(test, feature = "test-util"))] pub fn`. `Event { index: Option<u32>, usage: Option<Usage> }` shape is consistent across providers.

---

## Dispatch map (for subagent fan-out)

These tasks have no shared state and can run in parallel subagents:

- **Parallel batch 1:** A1, A2, A3, B (4 subagents)
- **Sequential:** C1 → C2 → {C3, C4, C5} (3 parallel) → C6 → C7 → D

Each subagent must:
1. Read this plan + the cited TODO/FIXME line.
2. Follow the TDD shape exactly: failing test, run, impl, run, verification gate, commit.
3. Run `superpowers:verification-before-completion` before claiming done.
4. Mark the TaskList item as completed when verified, not before.
