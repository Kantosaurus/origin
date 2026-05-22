# `/model` Slash Command Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `/model <name>` TUI slash command that swaps the active model for subsequent prompts in the running session, with the new model reflected in the status bar.

**Architecture:** The model is per-prompt (carried in `PromptRequest.model`), not daemon state. So `/model` is a **client-only** mutation: change the `model` string owned by the TUI event loop and mirror it into `App.usage.model` for the status line. No protocol or daemon changes are needed. The slash command is registered alongside `/account`, `/mem`, `/help` in `RESERVED_SLASH_VERBS` so the skill parser doesn't shadow it.

**Tech Stack:** Rust 2021 edition, Tokio (current_thread), `serde`, `crossterm`. Tests use the standard Cargo harness (`#[test]`/`#[tokio::test]`). The workspace is at `C:\Users\wooai\Documents\GitHub\origin` and uses `cargo test -p <crate>` to scope test runs.

**Why no daemon changes:** Search confirms the daemon never stores an "active model" — see [`crates/origin-daemon/src/protocol.rs:10-14`](../../../crates/origin-daemon/src/protocol.rs) where `PromptRequest { system, model, user_text }` is the only carrier, and the TUI rebuilds it on every `handle_submit` call. Compare with `SwitchAccount` which mutates daemon-side `ActiveProvider` state — different mechanism, not applicable here.

---

## File Structure

| File | Responsibility | Change |
|---|---|---|
| `crates/origin-cli/src/input.rs` | Slash-command parsers + `RESERVED_SLASH_VERBS` | **Modify** — add `parse_model_command` and register `"model"` |
| `crates/origin-cli/src/tui.rs` | `App` state + draw routine | **Modify** — add `App::set_model` setter |
| `crates/origin-cli/src/main.rs` | TUI event loop + `handle_submit` dispatch | **Modify** — change `model: &str` → `model: &mut String` and add `/model` branch |

No new files. No daemon changes. No `Cargo.toml` changes.

---

## Task 1: `parse_model_command` parser in `input.rs`

**Files:**
- Modify: `crates/origin-cli/src/input.rs:91` (extend `RESERVED_SLASH_VERBS`)
- Modify: `crates/origin-cli/src/input.rs:167` (add `parse_model_command` after `parse_workflow_command`)
- Test: `crates/origin-cli/src/input.rs:171-351` (extend the existing `#[cfg(test)] mod tests`)

### Step 1: Write the failing test

Append the following to the existing `mod tests { ... }` block in `crates/origin-cli/src/input.rs` (just before the closing `}` of the module, after `parse_workflow_command_rejects_malformed`):

```rust
    #[test]
    fn parse_model_basic() {
        let name = parse_model_command("/model claude-opus-4-7").expect("parse");
        assert_eq!(name, "claude-opus-4-7");
    }

    #[test]
    fn parse_model_tolerates_surrounding_whitespace() {
        let name = parse_model_command("   /model   claude-sonnet-4-6   ").expect("parse");
        assert_eq!(name, "claude-sonnet-4-6");
    }

    #[test]
    fn parse_model_rejects_no_argument() {
        assert!(parse_model_command("/model").is_none());
        assert!(parse_model_command("/model    ").is_none());
    }

    #[test]
    fn parse_model_rejects_multiple_args() {
        // Model names are a single token; extra args is a usage error,
        // surfaced as None so the caller can show the usage hint.
        assert!(parse_model_command("/model foo bar").is_none());
    }

    #[test]
    fn parse_model_requires_word_boundary() {
        // `/modelfoo` is not `/model foo` — must not be treated as a model
        // command. (The skill parser will pick it up instead.)
        assert!(parse_model_command("/modelfoo").is_none());
    }

    #[test]
    fn parse_skill_does_not_shadow_model() {
        // After registering "model" as reserved, the skill parser must
        // refuse `/model` so /model handling owns the verb.
        assert!(parse_skill_command("/model").is_none());
        assert!(parse_skill_command("/model:foo").is_none());
    }
```

### Step 2: Run tests to verify they fail

Run:
```
cargo test -p origin-cli --lib input::tests::parse_model -- --nocapture
cargo test -p origin-cli --lib input::tests::parse_skill_does_not_shadow_model -- --nocapture
```

Expected: compile error — `parse_model_command` not defined. (That's a valid "red" — the test cannot run.) The shadow test should compile but FAIL with `assertion failed` because `model` is not yet reserved.

### Step 3: Register `model` as a reserved verb

In `crates/origin-cli/src/input.rs`, find this line (around line 91):

```rust
const RESERVED_SLASH_VERBS: &[&str] = &["mem", "account", "help"];
```

Replace it with:

```rust
const RESERVED_SLASH_VERBS: &[&str] = &["mem", "account", "help", "model"];
```

### Step 4: Add the `parse_model_command` function

In `crates/origin-cli/src/input.rs`, append the following function **after** `parse_workflow_command` (around line 167, before the `#[cfg(test)]` block):

```rust
/// Parse a `/model <name>` slash command into the requested model name.
///
/// Recognized form:
/// - `/model <name>` — switch the TUI's active model to `<name>` for
///   subsequent prompts. Surrounding whitespace is tolerated; the name
///   itself must be a single token.
///
/// Returns `None` for any non-matching input (including `/model` with no
/// argument and `/model foo bar` with extra tokens) so the caller can
/// surface a usage hint.
#[must_use]
pub fn parse_model_command(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let rest = trimmed.strip_prefix("/model")?;
    // Require a word boundary so `/modelfoo` is not matched.
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let mut parts = rest.split_whitespace();
    let name = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    Some(name.to_string())
}
```

### Step 5: Run tests to verify they pass

Run:
```
cargo test -p origin-cli --lib input::tests -- --nocapture
```

Expected: all `parse_model_*` tests PASS, the existing tests still pass, and `parse_skill_does_not_shadow_model` passes.

### Step 6: Commit

```
git add crates/origin-cli/src/input.rs
git commit -m "feat(cli): add parse_model_command parser + reserve /model verb"
```

---

## Task 2: `App::set_model` setter in `tui.rs`

**Files:**
- Modify: `crates/origin-cli/src/tui.rs:31` (add a setter method after `new`)
- Test: `crates/origin-cli/src/tui.rs` — add a new `#[cfg(test)] mod tests` block at the bottom of the file

### Step 1: Write the failing test

Append the following to the **end** of `crates/origin-cli/src/tui.rs` (after the `write_str` helper):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_model_updates_usage_snapshot() {
        let mut app = App::new("anthropic", "claude-opus-4-7");
        assert_eq!(app.usage.model, "claude-opus-4-7");
        app.set_model("claude-sonnet-4-6");
        assert_eq!(app.usage.model, "claude-sonnet-4-6");
    }

    #[test]
    fn set_model_does_not_reset_token_counters() {
        // Accumulated usage must survive a model swap — otherwise the
        // status bar would zero out mid-session every time the user runs
        // `/model`, which is misleading. (Pricing is per-model lookup,
        // so the cost reading after a swap reflects new model's rates
        // applied to the running token totals — that's intentional.)
        let mut app = App::new("anthropic", "claude-opus-4-7");
        app.record_usage(100, 50, 0, 0, std::time::Duration::from_millis(200));
        app.set_model("claude-sonnet-4-6");
        assert_eq!(app.usage.input_tokens, 100);
        assert_eq!(app.usage.output_tokens, 50);
        assert_eq!(app.usage.model, "claude-sonnet-4-6");
    }
}
```

### Step 2: Run tests to verify they fail

Run:
```
cargo test -p origin-cli --lib tui::tests -- --nocapture
```

Expected: compile error — `set_model` method does not exist on `App`.

### Step 3: Implement `set_model`

In `crates/origin-cli/src/tui.rs`, find the `impl App` block. Just after the `pub fn new(...)` method (around line 31, before `add_line`), add:

```rust
    /// Replace the model name shown on the status line. Used by the
    /// `/model <name>` slash command to reflect the new active model
    /// without resetting the running token / cost counters.
    pub fn set_model(&mut self, model: impl Into<String>) {
        self.usage.model = model.into();
    }
```

### Step 4: Run tests to verify they pass

Run:
```
cargo test -p origin-cli --lib tui::tests -- --nocapture
```

Expected: `set_model_updates_usage_snapshot` and `set_model_does_not_reset_token_counters` both PASS.

### Step 5: Commit

```
git add crates/origin-cli/src/tui.rs
git commit -m "feat(cli): add App::set_model for /model slash command"
```

---

## Task 3: Wire `/model` into the TUI event loop

**Files:**
- Modify: `crates/origin-cli/src/main.rs:212` (event-loop call site)
- Modify: `crates/origin-cli/src/main.rs:209` (auto-fire call site)
- Modify: `crates/origin-cli/src/main.rs:220-227` (`run_event_loop` signature)
- Modify: `crates/origin-cli/src/main.rs:269` (event-loop submit call site)
- Modify: `crates/origin-cli/src/main.rs:316` (`handle_submit` signature)
- Modify: `crates/origin-cli/src/main.rs:316-339` (add `/model` branch + import)
- Test: `crates/origin-cli/tests/app.rs` — add new integration test for the parser wiring (since `handle_submit` is private, we test the parser hook-up rather than the live loop)

**Dependencies:** This task depends on Task 1 (`parse_model_command`) and Task 2 (`App::set_model`). It must run **after** both land on the branch.

### Step 1: Write the failing integration test

Open `crates/origin-cli/tests/app.rs` and append:

```rust
// /model slash command parser is reachable from the CLI surface and
// returns the requested model name. Wired into `handle_submit` in
// main.rs; this test pins the parser contract so a future refactor
// can't accidentally break the slash routing.
#[test]
fn model_command_parser_is_exported() {
    use origin_cli::input::parse_model_command;
    let name = parse_model_command("/model claude-haiku-4-5").expect("parse");
    assert_eq!(name, "claude-haiku-4-5");
}

#[test]
fn model_command_rejects_bare_verb() {
    use origin_cli::input::parse_model_command;
    assert!(parse_model_command("/model").is_none());
}
```

### Step 2: Run tests to verify they fail-or-pass appropriately

Run:
```
cargo test -p origin-cli --test app model_command -- --nocapture
```

Expected: tests PASS (because Task 1 already exported the parser). If they fail with "function not found in origin_cli::input", Task 1 has not been merged — stop and rebase onto Task 1 before continuing.

### Step 3: Change `model` to a mutable owned `String` in main

In `crates/origin-cli/src/main.rs`, find the lines around line 140:

```rust
    let path = env::var("ORIGIN_SOCK").unwrap_or_else(|_| default_path());
    let model = env::var("ORIGIN_MODEL").unwrap_or(default_model);
```

These already use `let` (not `let mut`). Change the second to:

```rust
    let path = env::var("ORIGIN_SOCK").unwrap_or_else(|_| default_path());
    let mut model = env::var("ORIGIN_MODEL").unwrap_or(default_model);
```

### Step 4: Update the auto-fire call site and the `run_event_loop` call

In `crates/origin-cli/src/main.rs`, find (around line 205-212):

```rust
    // Auto-fire the pending discovery prompt now that the TUI is wired up.
    if let Some(text) = pending_prompt {
        app.lock()
            .add_line("system> ", "Running queued first-run discovery prompt\u{2026}");
        handle.mark_dirty();
        handle_submit(&app, &handle, &path, &model, &text).await;
    }

    let result = run_event_loop(app, composer, widget, handle, &path, &model).await;
```

Replace with:

```rust
    // Auto-fire the pending discovery prompt now that the TUI is wired up.
    if let Some(text) = pending_prompt {
        app.lock()
            .add_line("system> ", "Running queued first-run discovery prompt\u{2026}");
        handle.mark_dirty();
        handle_submit(&app, &handle, &path, &mut model, &text).await;
    }

    let result = run_event_loop(app, composer, widget, handle, &path, &mut model).await;
```

### Step 5: Update `run_event_loop` to take `&mut String`

In `crates/origin-cli/src/main.rs`, find:

```rust
async fn run_event_loop(
    app: SharedApp,
    _composer: SharedComposer,
    _widget: SharedWidget,
    handle: Handle,
    path: &str,
    model: &str,
) -> Result<()> {
```

Replace with:

```rust
async fn run_event_loop(
    app: SharedApp,
    _composer: SharedComposer,
    _widget: SharedWidget,
    handle: Handle,
    path: &str,
    model: &mut String,
) -> Result<()> {
```

And in the inner `Submit` arm (around line 268-270):

```rust
                InputAction::Submit(text) => {
                    handle_submit(&app, &handle, path, model, &text).await;
                }
```

Replace with:

```rust
                InputAction::Submit(text) => {
                    handle_submit(&app, &handle, path, model, &text).await;
                }
```

(The body is unchanged — `model` is already `&mut String` in this scope after the signature change, and reborrowing happens implicitly. No edit needed beyond the signature, but **verify the call still compiles** in Step 8.)

### Step 6: Update `handle_submit` signature and add the `/model` branch

In `crates/origin-cli/src/main.rs`, find the imports near the top (around line 11):

```rust
use origin_cli::input::{
    parse_mem_command, parse_skill_command, parse_workflow_command, reduce, InputAction,
};
```

Replace with:

```rust
use origin_cli::input::{
    parse_mem_command, parse_model_command, parse_skill_command, parse_workflow_command, reduce,
    InputAction,
};
```

Then find the `handle_submit` signature (around line 316):

```rust
#[allow(clippy::too_many_lines)] // Single linear dispatch over many slash commands; splitting hurts readability.
async fn handle_submit(app: &SharedApp, handle: &Handle, path: &str, model: &str, text: &str) {
    if let Some(rest) = slash_account_args(text) {
```

Replace with:

```rust
#[allow(clippy::too_many_lines)] // Single linear dispatch over many slash commands; splitting hurts readability.
async fn handle_submit(
    app: &SharedApp,
    handle: &Handle,
    path: &str,
    model: &mut String,
    text: &str,
) {
    // `/model <name>` swaps the active model for subsequent prompts.
    // Client-side only: the daemon doesn't store an "active model" —
    // every PromptRequest carries its model string, so updating the
    // local `model` and the status-line snapshot is enough.
    if let Some(rest) = slash_model_args(text) {
        {
            let mut a = app.lock();
            a.add_line("you> ", text);
        }
        handle.mark_dirty();
        match parse_model_command(text) {
            Some(name) => {
                model.clear();
                model.push_str(&name);
                let mut a = app.lock();
                a.set_model(name.clone());
                a.add_line("system> ", &format!("model set: {name}"));
            }
            None => {
                let _ = rest; // unused when usage hint fires; matches `/account`'s shape
                app.lock()
                    .add_line("error> ", "usage: /model <name>");
            }
        }
        handle.mark_dirty();
        return;
    }
    if let Some(rest) = slash_account_args(text) {
```

(The `slash_model_args` helper is added in Step 7; it mirrors `slash_account_args` so the existing `/account` style is preserved.)

### Step 7: Add the `slash_model_args` helper

In `crates/origin-cli/src/main.rs`, find the existing `slash_account_args` helper (around line 494):

```rust
/// Returns `Some(rest)` when `line` is a `/account` command (with or
/// without arguments), where `rest` is the trimmed argument tail.
fn slash_account_args(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let rest = trimmed.strip_prefix("/account")?;
    // Require a word boundary so `/accountfoo` is not matched.
    if rest.is_empty() || rest.starts_with(char::is_whitespace) {
        Some(rest.trim())
    } else {
        None
    }
}
```

Immediately **after** it, add:

```rust
/// Returns `Some(rest)` when `line` is a `/model` command (with or
/// without arguments), where `rest` is the trimmed argument tail.
/// Mirrors the shape of [`slash_account_args`] so the `handle_submit`
/// branches read identically; the argument-validation parsing happens
/// downstream in `parse_model_command`.
fn slash_model_args(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let rest = trimmed.strip_prefix("/model")?;
    // Require a word boundary so `/modelfoo` falls through to the skill
    // parser instead of being eaten by the model handler.
    if rest.is_empty() || rest.starts_with(char::is_whitespace) {
        Some(rest.trim())
    } else {
        None
    }
}
```

### Step 8: Build + run the full origin-cli test suite

Run, in order:
```
cargo build -p origin-cli
cargo test -p origin-cli
```

Expected: clean build (no warnings about the `_ = rest` line or the new helper), and all tests — both the new `parse_model_*` / `set_model_*` / `model_command_*` and every pre-existing test — PASS.

If you see "cannot move out of `*model` which is behind a mutable reference" or similar — the new branch must use `model.clear() / model.push_str(&name)` to *mutate in place* rather than assign; this is already in Step 6. Don't switch to `*model = name`.

### Step 9: Run clippy to confirm no new lints

Run:
```
cargo clippy -p origin-cli --all-targets -- -D warnings
```

Expected: no errors. If the helper's `let _ = rest;` is flagged as `clippy::let_underscore_must_use`, replace it with `drop(rest);` — but pause and confirm with the user before deviating from the plan if anything else trips.

### Step 10: Commit

```
git add crates/origin-cli/src/main.rs crates/origin-cli/tests/app.rs
git commit -m "feat(cli): wire /model <name> into TUI handle_submit"
```

---

## Self-Review

**Spec coverage:**
- ✅ "Slash command that swaps the model" — Task 1 (parser) + Task 3 (handler).
- ✅ "Reflect in status bar" — Task 2 (`App::set_model` updates `UsageSnapshot.model`, which `render_line` reads).
- ✅ "Don't shadow `/<skill>`" — `RESERVED_SLASH_VERBS` extension covered in Task 1, with a regression test (`parse_skill_does_not_shadow_model`).
- ✅ "Doesn't disturb existing `/account`, `/mem`, `/help`, skill, workflow flows" — Task 3 inserts the `/model` branch at the top of `handle_submit`; existing branches are unchanged.

**Placeholder scan:** no `TODO`, no `TBD`, no "similar to". Every code change is a full code block.

**Type consistency:**
- `parse_model_command` returns `Option<String>` — used by `handle_submit` via `Some(name) => ...`. Match.
- `App::set_model` takes `impl Into<String>` — called with `name.clone()` (a `String`). Match.
- `handle_submit` `model: &mut String` — `run_event_loop` passes `&mut String` from a `let mut model: String` in `main`. Match.
- `slash_model_args` returns `Option<&str>`; used solely as an early-routing gate. Match.

**No daemon protocol delta** — confirmed by reading `crates/origin-daemon/src/protocol.rs` and `crates/origin-daemon/src/main.rs` `handle_switch`: no `ActiveModel` state exists. Adding a `SwitchModel` `ClientMessage` would be dead protocol surface.

---

## Execution Handoff

**Plan complete and saved to `docs/superpowers/plans/2026-05-22-slash-model-command.md`.**

Per the slash arguments, executing with **Subagent-Driven Development + Parallel Dispatch + TDD + Verification-Before-Completion**:

- Tasks 1 and 2 are **independent** (different files, no shared APIs) → dispatched in parallel.
- Task 3 **depends on** the exported `parse_model_command` (Task 1) and `App::set_model` (Task 2) → runs sequentially after both succeed.
- A final verification pass runs `cargo build` / `cargo test -p origin-cli` / `cargo clippy -p origin-cli` plus a manual TUI smoke before declaring done.
