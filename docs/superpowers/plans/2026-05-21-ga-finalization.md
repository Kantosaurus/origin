# Origin GA Finalization Plan (v1.0.0 → v1.1.0)

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` to execute the parallelisable tasks (F2, F3, F4 can run concurrently), then sequentialise the final smoke + tag. Every task ends with a `superpowers:verification-before-completion` gate; do NOT move on until verification is green. Use `superpowers:test-driven-development` discipline for any inline fixes.

**Goal:** Close out the GA production-readiness plan and the multi-provider expansion plan by running every remaining gate, fixing anything red, and cutting the `v1.1.0` tag.

**Status going in (as of 2026-05-21 on `dev`):**

- Phases 1–14 of the original implementation plan are merged and tagged `1.0.0` (commit `e41bf8c`).
- The GA consolidation plan's Tasks A1 (`f2c69bf`), A2 (`5ff9755`), A3 (`ed4ef24`) are merged.
- Task B "stabilise in-progress" is essentially done — only `crates/origin-mem/tests/index.rs` remains modified (HNSW padding fix; test now passes locally).
- The multi-provider expansion plan's Tasks 1–23 are merged (last commit `eca88f9`).
- Multi-provider Task 24 "final integration sweep" and GA Task D "final integration verification" are the open items, plus the dangling HNSW commit.

**Architecture (no change):** Daemon + CLI workspace, catalog-driven providers, three-tier CAS, sandboxed sidecar, encrypted KeyVault.

**Tech Stack (no change):** Rust 1.83 MSRV pinned, Tokio, rusqlite, rkyv, reqwest+rustls, wiremock for HTTP tests.

---

## Conventions

**Verification gate (per crate):**
```
cargo test -p <crate>
cargo clippy -p <crate> -- -D warnings
cargo fmt --check
```

**Workspace gate (this plan's primary discipline):**
```
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
```

**Commit style:** Conventional commits, scope to crate where possible. Co-author Claude.

**Parallelism:** F2, F3, F4 are independent (separate cargo invocations on read-only crate trees once F1 is in). F5, F6, F7, F8, F9 are sequential.

---

## File Structure

This plan does not create new source files. It exercises and stabilises the existing tree. Any inline fixes the verification gates surface should be scoped to the smallest crate that owns the failing code.

The one pending edit going in:

| File | Change | Reason |
|---|---|---|
| `crates/origin-mem/tests/index.rs` | Pad HNSW index with 50 ids before each search | `hnsw_rs` uses `StdRng::from_os_rng()` for layer assignment; tiny 2-point graphs flake intermittently. Padding ids carry no metadata so the `lookup` closure drops them after re-rank. |

---

## Task F1 — Land pending HNSW test fix

**Files:**
- Modify: `crates/origin-mem/tests/index.rs` (already modified in working tree)

- [ ] **Step 1: Confirm the test is green locally**

Run:
```
cargo test -p origin-mem --test index
```
Expected: 3 tests pass (`decay_demotes_old_match`, `supersede_drops_loser`, `cluster_priority_and_edge_boost_affect_rank`). If any flake, re-run 5×; expected: 5/5 pass.

- [ ] **Step 2: Lint the test**

Run:
```
cargo clippy -p origin-mem --tests -- -D warnings
cargo fmt --check
```
Expected: no warnings, no diff.

- [ ] **Step 3: Commit**

```
git add crates/origin-mem/tests/index.rs
git commit -m "test(origin-mem): pad HNSW graph to stabilise re-rank tests"
```

- [ ] **Step 4: Verify clean tree**

Run: `git status --short`
Expected: no entries under `crates/`. `.claude/` untracked is fine (local IDE state).

---

## Task F2 — Workspace test sweep (parallel)

**Files:** None.

- [ ] **Step 1: Run the full workspace under all features**

Run:
```
cargo test --workspace --all-features 2>&1 | tail -80
```
Expected: every `test result` line ends `... 0 failed`. Total binaries should be ≥ 90 (one per crate + tests + integration tests).

- [ ] **Step 2: If any test fails**

Do NOT mass-skip. For each failure:
1. Identify the owning crate.
2. Apply `superpowers:systematic-debugging` to find the root cause.
3. Write a regression test if one is missing for the failure mode.
4. Fix in place, re-run just that crate (`cargo test -p <crate>`).
5. Re-run the workspace sweep from Step 1.

- [ ] **Step 3: Verification gate**

The workspace sweep IS the verification gate for this task. No green = no commit, no progression.

- [ ] **Step 4: If a fix landed in Step 2, commit it now**

Use a focused commit per crate. Example:
```
git add crates/<crate>
git commit -m "fix(<crate>): <one-line root cause>"
```

---

## Task F3 — Clippy + fmt sweep (parallel)

**Files:** None.

- [ ] **Step 1: Clippy across the workspace**

Run:
```
cargo clippy --workspace --all-targets --all-features -- -D warnings 2>&1 | tail -60
```
Expected: `Finished` line, zero `warning:` or `error:` lines.

- [ ] **Step 2: If clippy flags anything**

Fix each lint at the source. Do NOT add `#[allow(...)]` annotations unless the lint is provably wrong (rare). Prefer:
- Rename a shadowed binding rather than `#[allow(clippy::shadow_unrelated)]`.
- Use `&str` over `String` if the lint is `clippy::ptr_arg`.
- Use `.get(i)?` over indexing if the lint is `clippy::indexing_slicing` in a `Result` returning function.

After every fix run the targeted crate test (`cargo test -p <crate>`) before re-running the workspace clippy.

- [ ] **Step 3: Format check**

Run:
```
cargo fmt --all -- --check
```
Expected: exit 0, no diff printed.

- [ ] **Step 4: If `fmt --check` exits non-zero**

Run `cargo fmt --all` and commit:
```
git add -A
git commit -m "chore: cargo fmt sweep"
```

- [ ] **Step 5: Commit any clippy fixes**

If Step 2 produced changes, commit per-crate as in F2/Step 4.

---

## Task F4 — Final TODO/FIXME sweep (parallel)

**Files:** None.

- [ ] **Step 1: Grep the crates tree**

Run (PowerShell):
```
rg -n "TODO|FIXME|XXX|unimplemented!|todo!\(\)" crates
```

Expected output: **only** these acceptable references:
- `crates/origin-mem/src/proposer.rs` lines 59–60 — the regex literal `r"(?i)\bTODO\b: (.+)"` that detects user TODOs in source they ask `origin` to triage. This is feature behaviour, not a workitem.
- `crates/origin-cli/src/keyring_login.rs` lines 276–277 — comment block `Parse: GET /?code=XXX&state=YYY HTTP/1.1` (the literal `XXX` is wire-format example syntax, not a placeholder).

- [ ] **Step 2: If any other hits remain**

Each hit is either:
- A real workitem the prior phases missed → file a follow-up task, do NOT silently delete the marker.
- A stale comment → delete it in the smallest possible commit:
  ```
  git commit -m "chore(<crate>): drop stale TODO marker"
  ```

- [ ] **Step 3: Verification**

Re-run Step 1. Confirm only the two acceptable hits remain. Record the count in the PR/final-tag description.

---

## Task F5 — Manual CLI smoke (sequential, after F2+F3+F4)

**Files:** None.

- [ ] **Step 1: Build the release binaries**

Run:
```
cargo build --release -p origin-cli -p origin-daemon
```
Expected: `Finished` line, no warnings.

- [ ] **Step 2: `providers ls`**

Run:
```
cargo run --release -p origin-cli -- providers ls
```
Expected: ≥30 rows in the table, with columns for provider id, wire format, auth scheme, default base URL. Anthropic, OpenAI, Gemini, OpenRouter, DeepSeek, Bedrock, Ollama, GitHub Copilot, OpenAI Codex must all appear.

- [ ] **Step 3: `providers describe`**

Run:
```
cargo run --release -p origin-cli -- providers describe openai-codex
```
Expected: prints OAuth spec — `authorize_url`, `token_url`, `client_id`, `scopes`, `pkce=true`. Run again with `anthropic`, `gemini-oauth`, `github-copilot` and confirm each shows its respective OAuth spec.

- [ ] **Step 4: Custom `providers.toml` round-trip**

Write a minimal user override:
```toml
# %USERPROFILE%\.origin\providers.toml
[[provider]]
id = "smoke-test"
wire = "openai-chat"
base_url = "https://example.invalid/v1"
auth = { scheme = "api-key", header = "Authorization", prefix = "Bearer " }
```

Run `providers ls` again. Expected: `smoke-test` appears at the bottom of the list. Delete the file when done.

- [ ] **Step 5: Verification gate**

If any of the three smoke checks fails, treat it as a regression in the catalog or CLI surface. Apply `superpowers:systematic-debugging` and fix forward. Do NOT proceed to F6 until all three pass.

- [ ] **Step 6: Commit (only if a fix landed)**

```
git commit -m "fix(origin-cli|origin-provider): smoke gap surfaced by GA finalization"
```

---

## Task F6 — Tag v1.1.0 (sequential, last)

**Files:** None (annotated tag only).

- [ ] **Step 1: Confirm the workspace gate one more time**

Run:
```
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
```
Expected: all three green. If any flake, fix and re-run before tagging.

- [ ] **Step 2: Confirm `git status` is clean**

```
git status --short
```
Expected: empty under `crates/` and `docs/`. (`.claude/` untracked is fine.)

- [ ] **Step 3: Confirm we are on `dev` at the expected HEAD**

```
git rev-parse --abbrev-ref HEAD
git log -1 --pretty=oneline
```
Expected: branch `dev`, HEAD is the most recent finalization commit (or `d334e8a` if F1–F5 produced no new commits).

- [ ] **Step 4: Pause and ask the user before tagging**

Tags are visible state that survive force-pushes and ship to users. Before running `git tag v1.1.0` show the user:
- The full `git log --oneline` since `e41bf8c` (the 1.0.0 GA tag).
- The output of F4's final TODO grep.
- A one-paragraph release-note summary.

Wait for explicit confirmation.

- [ ] **Step 5: Create the annotated tag**

```
git tag -a v1.1.0 -m "GA + multi-provider expansion + TODO cleanup

- Phases 1-14 implementation complete (tagged 1.0.0)
- Multi-provider expansion: 30+ providers via catalog-driven architecture
- OAuth flows for Anthropic, Gemini, OpenAI Codex (ChatGPT), GitHub Copilot
- ~/.origin/providers.toml loader for custom providers
- All TODO/FIXME markers from the GA consolidation plan closed
"
```

- [ ] **Step 6: Do NOT push the tag**

Tags ship to the world. Mention to the user:
```
Tag v1.1.0 created locally. Push when ready: git push origin v1.1.0
```

---

## Self-Review (writing-plans checklist)

1. **Spec coverage:**
   - GA plan Task A1, A2, A3 — verified merged (commits f2c69bf, 5ff9755, ed4ef24).
   - GA plan Task B — verified merged except `crates/origin-mem/tests/index.rs` → covered by F1.
   - GA plan Tasks C1–C7 — multi-provider expansion plan Tasks 4–23 merged; Task 24 covered by F2+F3+F4.
   - GA plan Task D — covered by F2 (workspace test), F3 (clippy+fmt), F5 (manual smoke), F6 (tag). Fuzz and bench from D Step 2/3 are deferred — the existing nightly CI matrix (commit `8670b9f`) already runs them on schedule; rerunning here adds 5+ min per target without surfacing new info, and the perf gate (commit `3e71501`) already CI-gates wall_ms p99.

2. **Placeholder scan:** No `TBD`/`fill in details`/`implement later`. Every step has an exact command and expected output.

3. **Type consistency:** No new types introduced. Existing names (`ProviderEntry`, `Catalog`, `OAuthSpec`, `OpenAiCompat`) are only referenced, not redefined.

4. **Subagent dispatch map (for `superpowers:subagent-driven-development`):**
   - **Sequential:** F1 (must land first — its commit becomes the base for the others).
   - **Parallel batch:** F2, F3, F4 (three subagents on independent gates).
   - **Sequential after batch:** F5 → F6.

5. **Risk surface:**
   - F2/F3 may surface real regressions on `--all-features` that the per-crate CI did not exercise (e.g. cross-feature interactions in `origin-daemon`'s `oauth-providers + custom-providers + openai-compat` matrix). Fix forward.
   - F5 requires a live `cargo run` of `origin-cli`; if the daemon isn't auto-started by the CLI the smoke check will surface that. Fix forward.
   - F6 is gated on explicit user confirmation; do NOT auto-tag.
