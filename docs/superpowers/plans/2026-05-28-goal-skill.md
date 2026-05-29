# `/goal` Skill Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a real `/goal <condition>` skill to origin with inline argument parsing, an inline `<goal-status>` self-tag protocol, a Haiku-verifier called only on `met` claims, auto-iteration bounded by max-iter and token-budget caps, and resume-token persistence.

**Architecture:** New leaf crate `origin-goal` owns the state machine, tag parser, flag parser, and verifier. CLI parser is generalized so `/name args...` activates a skill with optional args. Daemon recognizes `name == "goal"` and instantiates a `GoalState` per connection; after each `run_loop` the daemon runs the driver (parse tag → verify | iterate | clear). System prompt injects an `<origin-goal>` block when active. Resume token carries an optional `GoalSnapshot`.

**Tech Stack:** Rust 1.83 (MSRV pinned), tokio, serde, blake3, existing origin crates (origin-provider for Haiku, origin-daemon's `run_loop`, origin-resume-token).

**Spec:** `docs/superpowers/specs/2026-05-28-goal-skill-design.md`

---

## File Structure

### New files
- `crates/origin-goal/Cargo.toml`
- `crates/origin-goal/src/lib.rs` — re-exports, crate-level docs
- `crates/origin-goal/src/state.rs` — `GoalState`, `GoalStatus`, `ClearReason`, `TagOutcome`
- `crates/origin-goal/src/tag.rs` — `<goal-status>` tag parser
- `crates/origin-goal/src/flags.rs` — `/goal --max-iter=N --budget=200k <cond>` parser
- `crates/origin-goal/src/verifier.rs` — Verifier trait + verdict parser only (no Anthropic dep — the trait is provider-agnostic so the goal crate stays cycle-free of `origin-provider`).
- `crates/origin-daemon/src/anthropic_verifier.rs` — `AnthropicHaikuVerifier` implementing `origin_goal::verifier::Verifier`. Lives in the daemon because it needs `origin-provider`.
- `crates/origin-goal/src/wire.rs` — `TagOutcomeWire`, `ClearReasonWire`, `GoalSnapshot` (used by both protocol and resume-token)
- `crates/origin-goal/tests/tag_parser.rs`
- `crates/origin-goal/tests/flag_parser.rs`
- `crates/origin-goal/tests/state_machine.rs`
- `crates/origin-goal/tests/verifier_mock.rs`
- `crates/origin-daemon/tests/goal_activates_with_inline_args.rs`
- `crates/origin-daemon/tests/goal_iterates_on_in_progress.rs`
- `crates/origin-daemon/tests/goal_max_iter_caps.rs`
- `crates/origin-daemon/tests/goal_budget_caps.rs`
- `crates/origin-daemon/tests/goal_user_interrupt_during_iteration.rs`
- `crates/origin-daemon/tests/goal_verifier_rejection_resumes.rs`
- `crates/origin-resume-token/tests/goal_snapshot_round_trip.rs`

### Modified files
- `Cargo.toml` (workspace member list — `origin-goal` is auto-included via `crates/*` glob, but verify)
- `crates/origin-cli/src/input.rs` — `parse_skill_command` returns args
- `crates/origin-daemon/src/protocol.rs` — `ActivateSkill { args }`; new `StreamEvent` variants
- `crates/origin-daemon/src/agent.rs` — `LoopOptions::goal`, system-prompt injection
- `crates/origin-daemon/src/main.rs` — driver wrapper around `run_loop`
- `crates/origin-daemon/Cargo.toml` — add `origin-goal` dep
- `crates/origin-cli/Cargo.toml` — none (CLI only sees `ClientMessage`, which lives in daemon)
- `crates/origin-resume-token/Cargo.toml` — add `origin-goal` dep
- `crates/origin-resume-token/src/lib.rs` — `goal: Option<GoalSnapshot>` field
- `crates/origin-skills/embedded/superpowers/goal/SKILL.md` — replace stub body with real spec

---

## Task 1: Scaffold `origin-goal` crate

**Files:**
- Create: `crates/origin-goal/Cargo.toml`
- Create: `crates/origin-goal/src/lib.rs`

- [ ] **Step 1: Create `crates/origin-goal/Cargo.toml`**

```toml
[package]
name = "origin-goal"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[lints]
workspace = true

[dependencies]
serde = { version = "1", features = ["derive"] }
thiserror = "1"

[dev-dependencies]
```

- [ ] **Step 2: Create `crates/origin-goal/src/lib.rs`**

```rust
//! Goal driver: persistent completion conditions with inline self-tag protocol.
//!
//! See `docs/superpowers/specs/2026-05-28-goal-skill-design.md`.

#![forbid(unsafe_code)]

pub mod flags;
pub mod state;
pub mod tag;
pub mod verifier;
pub mod wire;

pub use state::{ClearReason, GoalState, GoalStatus, TagOutcome};
pub use tag::parse_tag;
pub use flags::{parse_goal_args, GoalArgs, FlagParseError};
pub use wire::{ClearReasonWire, GoalSnapshot, GoalStatusWire, TagOutcomeWire};
```

- [ ] **Step 3: Verify scaffold builds**

Run: `cargo check -p origin-goal`
Expected: errors about missing modules (we'll add them next). This step confirms the workspace picked up the crate.

- [ ] **Step 4: Commit**

```bash
git add crates/origin-goal/
git commit -m "feat(goal): scaffold origin-goal crate"
```

---

## Task 2: Tag parser (`<goal-status>` → `TagOutcome`)

**Files:**
- Create: `crates/origin-goal/src/tag.rs`
- Create: `crates/origin-goal/src/state.rs` (stub for `TagOutcome` enum only)
- Test: `crates/origin-goal/tests/tag_parser.rs`

- [ ] **Step 1: Stub `state.rs` with `TagOutcome` only**

Create `crates/origin-goal/src/state.rs`:
```rust
//! Goal state machine types. Fully populated in Task 4.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TagOutcome {
    Met,
    InProgress { what_remains: String },
    Blocked { why: String },
    Missing,
}

// Placeholder symbols so lib.rs compiles. Replaced in Task 4.
#[derive(Debug, Clone)]
pub struct GoalState;
#[derive(Debug, Clone)]
pub enum GoalStatus { Active }
#[derive(Debug, Clone)]
pub enum ClearReason { UserSlash }
```

- [ ] **Step 2: Stub `flags.rs`, `verifier.rs`, `wire.rs` so the crate compiles**

`crates/origin-goal/src/flags.rs`:
```rust
//! Flag parser for `/goal --max-iter=N --budget=200k <cond>`. Implemented in Task 3.

use thiserror::Error;

#[derive(Debug)]
pub struct GoalArgs;

#[derive(Debug, Error)]
pub enum FlagParseError {
    #[error("placeholder")]
    Placeholder,
}

pub fn parse_goal_args(_raw: &str) -> Result<GoalArgs, FlagParseError> {
    Err(FlagParseError::Placeholder)
}
```

`crates/origin-goal/src/verifier.rs`:
```rust
//! Haiku verifier. Implemented in Task 8.
```

`crates/origin-goal/src/wire.rs`:
```rust
//! Wire-shape types shared with protocol + resume-token. Implemented in Task 5.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalSnapshot;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagOutcomeWire;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClearReasonWire;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalStatusWire;
```

- [ ] **Step 3: Write failing tag-parser tests**

Create `crates/origin-goal/tests/tag_parser.rs`:
```rust
use origin_goal::{parse_tag, TagOutcome};

#[test]
fn parses_met() {
    let s = "some assistant text\n<goal-status state=\"met\"><reason>tests green</reason></goal-status>";
    assert_eq!(parse_tag(s), TagOutcome::Met);
}

#[test]
fn parses_in_progress_with_reason() {
    let s = "<goal-status state=\"in_progress\"><reason>still 3 tests failing</reason></goal-status>";
    assert_eq!(
        parse_tag(s),
        TagOutcome::InProgress { what_remains: "still 3 tests failing".to_string() }
    );
}

#[test]
fn parses_blocked_with_reason() {
    let s = "<goal-status state=\"blocked\"><reason>need DB password</reason></goal-status>";
    assert_eq!(
        parse_tag(s),
        TagOutcome::Blocked { why: "need DB password".to_string() }
    );
}

#[test]
fn missing_tag_yields_missing() {
    assert_eq!(parse_tag("plain assistant reply with no tag"), TagOutcome::Missing);
}

#[test]
fn malformed_tag_yields_missing() {
    let s = "<goal-status state=\"banana\"><reason>x</reason></goal-status>";
    assert_eq!(parse_tag(s), TagOutcome::Missing);
}

#[test]
fn multiple_tags_last_wins() {
    let s = "<goal-status state=\"in_progress\"><reason>a</reason></goal-status> \
             midtext \
             <goal-status state=\"met\"><reason>done</reason></goal-status>";
    assert_eq!(parse_tag(s), TagOutcome::Met);
}

#[test]
fn state_attr_case_insensitive() {
    let s = "<goal-status state=\"MET\"></goal-status>";
    assert_eq!(parse_tag(s), TagOutcome::Met);
}

#[test]
fn whitespace_in_attributes_ok() {
    let s = "<goal-status   state = \"in_progress\" ><reason>x</reason></goal-status>";
    assert_eq!(
        parse_tag(s),
        TagOutcome::InProgress { what_remains: "x".to_string() }
    );
}

#[test]
fn empty_reason_ok() {
    let s = "<goal-status state=\"in_progress\"></goal-status>";
    assert_eq!(
        parse_tag(s),
        TagOutcome::InProgress { what_remains: String::new() }
    );
}

#[test]
fn extra_attributes_ignored() {
    let s = "<goal-status state=\"met\" extra=\"foo\"><reason>r</reason></goal-status>";
    assert_eq!(parse_tag(s), TagOutcome::Met);
}
```

- [ ] **Step 4: Run tests, confirm they fail**

Run: `cargo test -p origin-goal --test tag_parser`
Expected: 10 FAIL (function returns nothing useful yet).

- [ ] **Step 5: Implement the parser in `crates/origin-goal/src/tag.rs`**

```rust
//! Parse `<goal-status>` tags emitted by the main model.
//!
//! Tolerant by design: case-insensitive `state=`, whitespace allowed
//! in attributes, missing `<reason>` defaults to empty, multiple tags →
//! last wins. Anything we cannot make sense of returns `TagOutcome::Missing`
//! so a forgetful main model never accidentally ends the loop.

use crate::state::TagOutcome;

/// Parse the rightmost well-formed `<goal-status>` tag in `text`.
///
/// Returns [`TagOutcome::Missing`] if no tag is found or the rightmost one
/// has an unknown `state=` value.
#[must_use]
pub fn parse_tag(text: &str) -> TagOutcome {
    let mut last = TagOutcome::Missing;
    let mut cursor = 0;
    while let Some(open_rel) = text[cursor..].find("<goal-status") {
        let open = cursor + open_rel;
        let Some(tag_close_rel) = text[open..].find('>') else { break };
        let attrs_end = open + tag_close_rel;
        let attrs = &text[open + "<goal-status".len()..attrs_end];
        let Some(close_rel) = text[attrs_end..].find("</goal-status>") else { break };
        let close = attrs_end + close_rel;
        let inner = &text[attrs_end + 1..close];
        cursor = close + "</goal-status>".len();
        if let Some(outcome) = build_outcome(attrs, inner) {
            last = outcome;
        }
    }
    last
}

fn build_outcome(attrs: &str, inner: &str) -> Option<TagOutcome> {
    let state = extract_state(attrs)?.to_ascii_lowercase();
    let reason = extract_reason(inner);
    match state.as_str() {
        "met" => Some(TagOutcome::Met),
        "in_progress" => Some(TagOutcome::InProgress { what_remains: reason }),
        "blocked" => Some(TagOutcome::Blocked { why: reason }),
        _ => None,
    }
}

fn extract_state(attrs: &str) -> Option<&str> {
    // Hand-rolled to stay dependency-free. Looks for `state` (ws) `=` (ws) `"..."`.
    let bytes = attrs.as_bytes();
    let mut i = 0;
    while i + 5 <= bytes.len() {
        if &bytes[i..i + 5] == b"state"
            && (i == 0 || matches!(bytes[i - 1], b' ' | b'\t' | b'\r' | b'\n'))
        {
            let mut j = i + 5;
            while j < bytes.len() && matches!(bytes[j], b' ' | b'\t') { j += 1; }
            if j >= bytes.len() || bytes[j] != b'=' { i += 1; continue; }
            j += 1;
            while j < bytes.len() && matches!(bytes[j], b' ' | b'\t') { j += 1; }
            if j >= bytes.len() || bytes[j] != b'"' { i += 1; continue; }
            let val_start = j + 1;
            let val_end = val_start + attrs[val_start..].find('"')?;
            return Some(&attrs[val_start..val_end]);
        }
        i += 1;
    }
    None
}

fn extract_reason(inner: &str) -> String {
    let Some(open) = inner.find("<reason>") else { return String::new() };
    let after_open = open + "<reason>".len();
    let Some(close_rel) = inner[after_open..].find("</reason>") else { return String::new() };
    inner[after_open..after_open + close_rel].trim().to_string()
}
```

- [ ] **Step 6: Run tests, confirm all pass**

Run: `cargo test -p origin-goal --test tag_parser`
Expected: `test result: ok. 10 passed`.

- [ ] **Step 7: Commit**

```bash
git add crates/origin-goal/
git commit -m "feat(goal): tag parser for <goal-status>"
```

---

## Task 3: Flag parser (`/goal --max-iter=N --budget=200k <cond>`)

**Files:**
- Modify: `crates/origin-goal/src/flags.rs`
- Test: `crates/origin-goal/tests/flag_parser.rs`

- [ ] **Step 1: Write failing tests**

Create `crates/origin-goal/tests/flag_parser.rs`:
```rust
use origin_goal::{parse_goal_args, FlagParseError};

#[test]
fn condition_only() {
    let g = parse_goal_args("fix the failing tests").unwrap();
    assert_eq!(g.condition, "fix the failing tests");
    assert_eq!(g.max_iter, None);
    assert_eq!(g.token_budget, None);
}

#[test]
fn max_iter_then_cond() {
    let g = parse_goal_args("--max-iter=50 fix tests").unwrap();
    assert_eq!(g.condition, "fix tests");
    assert_eq!(g.max_iter, Some(50));
}

#[test]
fn budget_with_k_suffix() {
    let g = parse_goal_args("--budget=200k fix tests").unwrap();
    assert_eq!(g.token_budget, Some(200_000));
}

#[test]
fn budget_with_m_suffix() {
    let g = parse_goal_args("--budget=1m fix tests").unwrap();
    assert_eq!(g.token_budget, Some(1_000_000));
}

#[test]
fn budget_plain_number() {
    let g = parse_goal_args("--budget=12345 fix tests").unwrap();
    assert_eq!(g.token_budget, Some(12_345));
}

#[test]
fn both_flags() {
    let g = parse_goal_args("--max-iter=5 --budget=50k fix tests").unwrap();
    assert_eq!(g.max_iter, Some(5));
    assert_eq!(g.token_budget, Some(50_000));
    assert_eq!(g.condition, "fix tests");
}

#[test]
fn flags_after_condition_text_are_part_of_condition() {
    let g = parse_goal_args("fix tests --max-iter=5").unwrap();
    assert_eq!(g.condition, "fix tests --max-iter=5");
    assert_eq!(g.max_iter, None);
}

#[test]
fn unknown_flag_rejected() {
    let err = parse_goal_args("--bogus=1 fix tests").unwrap_err();
    matches!(err, FlagParseError::UnknownFlag(_));
}

#[test]
fn empty_condition_rejected() {
    let err = parse_goal_args("--max-iter=5").unwrap_err();
    matches!(err, FlagParseError::EmptyCondition);
}

#[test]
fn condition_with_embedded_double_dash_kept() {
    let g = parse_goal_args("rewrite the --foo flag handler").unwrap();
    assert_eq!(g.condition, "rewrite the --foo flag handler");
}

#[test]
fn max_iter_non_numeric_rejected() {
    let err = parse_goal_args("--max-iter=abc fix").unwrap_err();
    matches!(err, FlagParseError::InvalidValue { .. });
}

#[test]
fn condition_exceeding_4000_chars_rejected() {
    let big = "x".repeat(4001);
    let err = parse_goal_args(&big).unwrap_err();
    matches!(err, FlagParseError::ConditionTooLong);
}
```

- [ ] **Step 2: Run tests to confirm they fail**

Run: `cargo test -p origin-goal --test flag_parser`
Expected: compile errors (`GoalArgs` has no fields yet), then 12 FAIL.

- [ ] **Step 3: Implement the flag parser**

Replace `crates/origin-goal/src/flags.rs`:
```rust
//! Parse the argument string passed alongside `/goal`.
//!
//! Grammar: `(--key=value )* <condition...>`. The first token that doesn't
//! start with `--` begins the condition; everything from there to end-of-line
//! is the condition verbatim (so a condition like `rewrite the --foo flag`
//! is preserved).

use thiserror::Error;

const MAX_CONDITION_LEN: usize = 4_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalArgs {
    pub condition: String,
    pub max_iter: Option<u32>,
    pub token_budget: Option<u64>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum FlagParseError {
    #[error("unknown flag: {0}")]
    UnknownFlag(String),
    #[error("invalid value for {flag}: {value}")]
    InvalidValue { flag: String, value: String },
    #[error("--{0} requires =<value>")]
    MissingValue(String),
    #[error("goal condition is empty")]
    EmptyCondition,
    #[error("goal condition exceeds 4000 characters")]
    ConditionTooLong,
}

/// Parse the args portion of `/goal <args>`. The leading `/goal` token must
/// already be stripped by the caller.
///
/// # Errors
/// See [`FlagParseError`] variants.
pub fn parse_goal_args(raw: &str) -> Result<GoalArgs, FlagParseError> {
    let mut max_iter: Option<u32> = None;
    let mut token_budget: Option<u64> = None;

    let trimmed = raw.trim_start();
    let mut rest = trimmed;

    loop {
        let stripped = rest.trim_start();
        if !stripped.starts_with("--") {
            rest = stripped;
            break;
        }
        // Find end of this flag token (first whitespace).
        let token_end = stripped.find(char::is_whitespace).unwrap_or(stripped.len());
        let token = &stripped[..token_end];
        let after = &stripped[token_end..];
        let inner = &token[2..]; // strip leading "--"
        let (key, value) = inner
            .split_once('=')
            .ok_or_else(|| FlagParseError::MissingValue(inner.to_string()))?;
        match key {
            "max-iter" => {
                max_iter = Some(value.parse::<u32>().map_err(|_| FlagParseError::InvalidValue {
                    flag: key.to_string(),
                    value: value.to_string(),
                })?);
            }
            "budget" => {
                token_budget = Some(parse_budget(value).ok_or(FlagParseError::InvalidValue {
                    flag: key.to_string(),
                    value: value.to_string(),
                })?);
            }
            other => return Err(FlagParseError::UnknownFlag(other.to_string())),
        }
        rest = after;
    }

    let condition = rest.trim().to_string();
    if condition.is_empty() {
        return Err(FlagParseError::EmptyCondition);
    }
    if condition.len() > MAX_CONDITION_LEN {
        return Err(FlagParseError::ConditionTooLong);
    }
    Ok(GoalArgs { condition, max_iter, token_budget })
}

fn parse_budget(s: &str) -> Option<u64> {
    let (num_part, mult) = match s.as_bytes().last()? {
        b'k' | b'K' => (&s[..s.len() - 1], 1_000u64),
        b'm' | b'M' => (&s[..s.len() - 1], 1_000_000u64),
        _ => (s, 1u64),
    };
    let n: u64 = num_part.parse().ok()?;
    n.checked_mul(mult)
}
```

- [ ] **Step 4: Run tests, confirm all pass**

Run: `cargo test -p origin-goal --test flag_parser`
Expected: `test result: ok. 12 passed`.

- [ ] **Step 5: Commit**

```bash
git add crates/origin-goal/
git commit -m "feat(goal): flag parser for /goal args"
```

---

## Task 4: State machine + budget arithmetic

**Files:**
- Modify: `crates/origin-goal/src/state.rs`
- Test: `crates/origin-goal/tests/state_machine.rs`

- [ ] **Step 1: Replace `state.rs` with the real types**

```rust
//! Goal state machine.
//!
//! The driver lives in `origin-daemon`; this crate only carries the types
//! and the pure-function transitions so they can be unit-tested without
//! tokio or providers.

use serde::{Deserialize, Serialize};
use std::time::SystemTime;

pub const DEFAULT_MAX_ITER: u32 = 20;
pub const DEFAULT_TOKEN_BUDGET: u64 = 200_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TagOutcome {
    Met,
    InProgress { what_remains: String },
    Blocked { why: String },
    Missing,
}

#[derive(Debug, Clone)]
pub struct GoalState {
    pub condition: String,
    pub status: GoalStatus,
    pub iter: u32,
    pub max_iter: u32,
    pub tokens_spent: u64,
    pub token_budget: u64,
    pub started_at: SystemTime,
    pub last_status_tag: Option<TagOutcome>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoalStatus {
    Active,
    Verifying,
    Met { reason: String },
    Cleared { by: ClearReason },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClearReason {
    UserSlash,
    UserClearAll,
    MaxIter,
    BudgetExhausted,
    VerifierRejected(String),
    Met { reason: String },
    VerifierUnavailable,
}

impl GoalState {
    #[must_use]
    pub fn new(condition: String, max_iter: Option<u32>, token_budget: Option<u64>) -> Self {
        Self {
            condition,
            status: GoalStatus::Active,
            iter: 0,
            max_iter: max_iter.unwrap_or(DEFAULT_MAX_ITER),
            tokens_spent: 0,
            token_budget: token_budget.unwrap_or(DEFAULT_TOKEN_BUDGET),
            started_at: SystemTime::now(),
            last_status_tag: None,
        }
    }

    /// Top-of-iteration cap check. Returns `Some(reason)` if the iteration
    /// should not run; the caller emits `GoalCleared { reason }` and stops.
    #[must_use]
    pub fn cap_check(&self) -> Option<ClearReason> {
        if self.iter >= self.max_iter {
            Some(ClearReason::MaxIter)
        } else if self.tokens_spent >= self.token_budget {
            Some(ClearReason::BudgetExhausted)
        } else {
            None
        }
    }

    /// Called after `run_loop` returns. Adds tokens, increments iter, and
    /// records the parsed tag.
    pub fn record_iteration(&mut self, input_tokens: u64, output_tokens: u64, tag: TagOutcome) {
        self.tokens_spent = self
            .tokens_spent
            .saturating_add(input_tokens.saturating_add(output_tokens));
        self.iter = self.iter.saturating_add(1);
        self.last_status_tag = Some(tag);
    }

    /// Bookkeeping for the verifier's own token spend.
    pub fn record_verifier_tokens(&mut self, input_tokens: u64, output_tokens: u64) {
        self.tokens_spent = self
            .tokens_spent
            .saturating_add(input_tokens.saturating_add(output_tokens));
    }
}
```

- [ ] **Step 2: Write failing state-machine tests**

Create `crates/origin-goal/tests/state_machine.rs`:
```rust
use origin_goal::{ClearReason, GoalState, TagOutcome};

#[test]
fn defaults_when_args_omitted() {
    let g = GoalState::new("fix tests".into(), None, None);
    assert_eq!(g.condition, "fix tests");
    assert_eq!(g.iter, 0);
    assert_eq!(g.max_iter, 20);
    assert_eq!(g.token_budget, 200_000);
    assert_eq!(g.tokens_spent, 0);
    assert!(g.last_status_tag.is_none());
}

#[test]
fn cap_check_clean_on_fresh_state() {
    let g = GoalState::new("x".into(), None, None);
    assert_eq!(g.cap_check(), None);
}

#[test]
fn cap_check_fires_on_max_iter_equality() {
    let mut g = GoalState::new("x".into(), Some(3), None);
    g.iter = 3;
    assert_eq!(g.cap_check(), Some(ClearReason::MaxIter));
}

#[test]
fn cap_check_fires_on_budget_equality() {
    let mut g = GoalState::new("x".into(), None, Some(100));
    g.tokens_spent = 100;
    assert_eq!(g.cap_check(), Some(ClearReason::BudgetExhausted));
}

#[test]
fn record_iteration_accumulates_tokens_and_increments_iter() {
    let mut g = GoalState::new("x".into(), None, None);
    g.record_iteration(50, 25, TagOutcome::InProgress { what_remains: "a".into() });
    assert_eq!(g.tokens_spent, 75);
    assert_eq!(g.iter, 1);
    assert_eq!(
        g.last_status_tag,
        Some(TagOutcome::InProgress { what_remains: "a".into() })
    );
}

#[test]
fn record_verifier_tokens_charges_to_same_budget() {
    let mut g = GoalState::new("x".into(), None, Some(1_000));
    g.record_verifier_tokens(400, 100);
    assert_eq!(g.tokens_spent, 500);
    assert_eq!(g.iter, 0); // verifier doesn't count as an iteration
}

#[test]
fn budget_overshoot_one_iteration_then_caps() {
    let mut g = GoalState::new("x".into(), None, Some(100));
    assert_eq!(g.cap_check(), None);              // can run once
    g.record_iteration(80, 60, TagOutcome::InProgress { what_remains: String::new() });
    // tokens_spent = 140, over budget; next cap check fires
    assert_eq!(g.cap_check(), Some(ClearReason::BudgetExhausted));
}

#[test]
fn saturating_arithmetic_does_not_panic() {
    let mut g = GoalState::new("x".into(), Some(u32::MAX), Some(u64::MAX));
    g.record_iteration(u64::MAX, u64::MAX, TagOutcome::Met);
    assert_eq!(g.tokens_spent, u64::MAX);
}
```

- [ ] **Step 3: Run tests, confirm pass**

Run: `cargo test -p origin-goal --test state_machine`
Expected: `test result: ok. 8 passed`.

- [ ] **Step 4: Commit**

```bash
git add crates/origin-goal/
git commit -m "feat(goal): state machine + budget arithmetic"
```

---

## Task 5: Wire-shape types (`GoalSnapshot`, `*Wire` enums)

**Files:**
- Modify: `crates/origin-goal/src/wire.rs`

- [ ] **Step 1: Replace `wire.rs` with the real types**

```rust
//! Wire-shape types shared between the protocol (origin-daemon) and the
//! resume token (origin-resume-token). Kept in this crate so both consumers
//! depend on `origin-goal` rather than each other.

use crate::state::{ClearReason, GoalStatus, TagOutcome};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TagOutcomeWire {
    Met,
    InProgress { what_remains: String },
    Blocked { why: String },
    Missing,
}

impl From<TagOutcome> for TagOutcomeWire {
    fn from(t: TagOutcome) -> Self {
        match t {
            TagOutcome::Met => Self::Met,
            TagOutcome::InProgress { what_remains } => Self::InProgress { what_remains },
            TagOutcome::Blocked { why } => Self::Blocked { why },
            TagOutcome::Missing => Self::Missing,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClearReasonWire {
    UserSlash,
    UserClearAll,
    MaxIter,
    BudgetExhausted,
    VerifierRejected { why: String },
    Met { reason: String },
    VerifierUnavailable,
}

impl From<ClearReason> for ClearReasonWire {
    fn from(r: ClearReason) -> Self {
        match r {
            ClearReason::UserSlash => Self::UserSlash,
            ClearReason::UserClearAll => Self::UserClearAll,
            ClearReason::MaxIter => Self::MaxIter,
            ClearReason::BudgetExhausted => Self::BudgetExhausted,
            ClearReason::VerifierRejected(why) => Self::VerifierRejected { why },
            ClearReason::Met { reason } => Self::Met { reason },
            ClearReason::VerifierUnavailable => Self::VerifierUnavailable,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GoalStatusWire {
    Active,
    Verifying,
    Met { reason: String },
    Cleared { by: ClearReasonWire },
}

impl From<GoalStatus> for GoalStatusWire {
    fn from(s: GoalStatus) -> Self {
        match s {
            GoalStatus::Active => Self::Active,
            GoalStatus::Verifying => Self::Verifying,
            GoalStatus::Met { reason } => Self::Met { reason },
            GoalStatus::Cleared { by } => Self::Cleared { by: by.into() },
        }
    }
}

/// Snapshot persisted in the resume token. The `started_at_unix` field
/// avoids round-tripping `SystemTime` (whose serde shape is host-dependent).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GoalSnapshot {
    pub condition: String,
    pub iter: u32,
    pub max_iter: u32,
    pub tokens_spent: u64,
    pub token_budget: u64,
    pub started_at_unix: u64,
    pub status: GoalStatusWire,
}
```

- [ ] **Step 2: Confirm the crate compiles**

Run: `cargo check -p origin-goal --tests`
Expected: clean build.

- [ ] **Step 3: Commit**

```bash
git add crates/origin-goal/
git commit -m "feat(goal): wire-shape types + From conversions"
```

---

## Task 6: Generalize `ClientMessage::ActivateSkill` to carry args

**Files:**
- Modify: `crates/origin-daemon/src/protocol.rs`
- Modify: `crates/origin-cli/src/input.rs`
- Modify: `crates/origin-daemon/src/main.rs` (any callsite that constructs `ActivateSkill`)
- Modify: tests under `crates/origin-daemon/tests/` that construct `ActivateSkill`

- [ ] **Step 1: Add a regression test in `input.rs` for the new args field**

Append to the existing test module at the bottom of `crates/origin-cli/src/input.rs` (find the `mod tests` block; if none, create one):
```rust
#[cfg(test)]
mod tests_args {
    use super::*;
    use origin_daemon::protocol::ClientMessage;

    #[test]
    fn slash_with_args_returns_args_field() {
        let got = parse_skill_command("/goal fix the failing tests");
        assert!(matches!(
            got,
            Some(ClientMessage::ActivateSkill { ref name, args: Some(ref a) })
                if name == "goal" && a == "fix the failing tests"
        ));
    }

    #[test]
    fn slash_without_args_returns_none_args() {
        let got = parse_skill_command("/clear");
        assert!(matches!(
            got,
            Some(ClientMessage::ActivateSkill { ref name, args: None })
                if name == "clear"
        ));
    }

    #[test]
    fn deactivate_form_unaffected() {
        let got = parse_skill_command("/-goal");
        assert!(matches!(
            got,
            Some(ClientMessage::DeactivateSkill { ref name }) if name == "goal"
        ));
    }

    #[test]
    fn reserved_verb_still_rejected_with_args() {
        assert!(parse_skill_command("/mem accept").is_none());
    }
}
```

- [ ] **Step 2: Run tests, confirm they fail**

Run: `cargo test -p origin-cli --lib tests_args`
Expected: compile error (`args` field doesn't exist yet).

- [ ] **Step 3: Add `args: Option<String>` to `ActivateSkill` in `crates/origin-daemon/src/protocol.rs`**

Find the line (around 118):
```rust
ActivateSkill { name: String },
```
Replace with:
```rust
ActivateSkill { name: String, args: Option<String> },
```

- [ ] **Step 4: Update the parser in `crates/origin-cli/src/input.rs`**

Replace the body of `parse_skill_command` (currently at ~line 113):
```rust
#[must_use]
pub fn parse_skill_command(line: &str) -> Option<ClientMessage> {
    let trimmed = line.trim();
    let rest = trimmed.strip_prefix('/')?;
    if rest.is_empty() {
        return None;
    }

    // Split into `name_token` and `args` on the first whitespace.
    let (name_token, args_str) = match rest.find(char::is_whitespace) {
        Some(idx) => {
            let (n, a) = rest.split_at(idx);
            (n, a.trim_start())
        }
        None => (rest, ""),
    };
    if name_token.is_empty() {
        return None;
    }

    // Deactivate sigil: `-<name>`, no args allowed (would be ambiguous).
    if let Some(name) = name_token.strip_prefix('-') {
        if name.is_empty() || !args_str.is_empty() {
            return None;
        }
        if RESERVED_SLASH_VERBS.iter().any(|v| name == *v) {
            return None;
        }
        return Some(ClientMessage::DeactivateSkill { name: name.to_string() });
    }

    // Activate form. Reserved-verb guard applies to the first `:`-segment.
    let first_segment = name_token.split(':').next().unwrap_or(name_token);
    if RESERVED_SLASH_VERBS.iter().any(|v| first_segment == *v) {
        return None;
    }
    let args = if args_str.is_empty() { None } else { Some(args_str.to_string()) };
    Some(ClientMessage::ActivateSkill {
        name: name_token.to_string(),
        args,
    })
}
```

- [ ] **Step 5: Update all daemon callsites that construct `ActivateSkill`**

Find all of them:
```bash
grep -rn "ActivateSkill {" crates/origin-daemon/src/ crates/origin-daemon/tests/
```

For each construction site, add `args: None` (or the appropriate value). At the daemon-side handler in `main.rs` around line 839, the pattern match becomes:
```rust
ClientMessage::ActivateSkill { name, args } => {
    // `args` is consumed in Task 9 when goal routing lands; for now ignore it.
    let _ = &args;
    // ... existing body ...
}
```

- [ ] **Step 6: Run all CLI and daemon tests**

Run: `cargo test -p origin-cli --lib tests_args`
Expected: `test result: ok. 4 passed`.

Run: `cargo test -p origin-daemon`
Expected: clean (no new failures from the field addition).

- [ ] **Step 7: Commit**

```bash
git add crates/origin-cli/src/input.rs crates/origin-daemon/src/protocol.rs crates/origin-daemon/src/main.rs crates/origin-daemon/tests/
git commit -m "feat(protocol): ActivateSkill carries optional args"
```

---

## Task 7: Add `Goal*` `StreamEvent` variants

**Files:**
- Modify: `crates/origin-daemon/src/protocol.rs`
- Modify: `crates/origin-daemon/Cargo.toml`

- [ ] **Step 1: Add `origin-goal` as a daemon dep**

Edit `crates/origin-daemon/Cargo.toml`, add under `[dependencies]`:
```toml
origin-goal = { path = "../origin-goal" }
```

- [ ] **Step 2: Add the variants to `StreamEvent`**

Open `crates/origin-daemon/src/protocol.rs`. Find the `StreamEvent` enum (uses `#[serde(tag = "kind", rename_all = "snake_case")]`). Add at the end of the variant list:
```rust
/// Emitted when `/goal <cond>` activates a new goal.
GoalActive {
    condition: String,
    max_iter: u32,
    token_budget: u64,
},
/// Emitted after each `run_loop` tick while a goal is active.
GoalIteration {
    iter: u32,
    tokens_spent: u64,
    last_tag: origin_goal::TagOutcomeWire,
},
/// Emitted right before the Haiku verifier call.
GoalVerifying,
/// Terminal event for a goal.
GoalCleared {
    reason: origin_goal::ClearReasonWire,
    iter: u32,
    tokens_spent: u64,
},
```

- [ ] **Step 3: Ensure the daemon still compiles**

Run: `cargo check -p origin-daemon`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/origin-daemon/Cargo.toml crates/origin-daemon/src/protocol.rs
git commit -m "feat(protocol): Goal* StreamEvent variants"
```

---

## Task 8: Verifier trait + Haiku impl (with fail-open)

**Files:**
- Modify: `crates/origin-goal/src/verifier.rs`
- Modify: `crates/origin-goal/Cargo.toml`
- Test: `crates/origin-goal/tests/verifier_mock.rs`

- [ ] **Step 1: Add async + provider deps to origin-goal's Cargo.toml**

```toml
[dependencies]
serde = { version = "1", features = ["derive"] }
thiserror = "1"
async-trait = "0.1"

[dev-dependencies]
tokio = { version = "1", features = ["macros", "rt"] }
```

(Do NOT depend on `origin-provider` here — the verifier trait is provider-agnostic; the concrete Anthropic impl lives in the daemon to keep this crate cycle-free.)

- [ ] **Step 2: Define the verifier trait**

Replace `crates/origin-goal/src/verifier.rs`:
```rust
//! Verifier trait + plain-text verdict parser.
//!
//! The concrete Anthropic-Haiku implementation lives in the daemon to keep
//! this crate dependency-free. Tests use a `MockVerifier` defined inline.

use async_trait::async_trait;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Met,
    NotMet { reason: String },
}

#[derive(Debug, Error)]
pub enum VerifierError {
    #[error("verifier transport: {0}")]
    Transport(String),
    #[error("verifier rate-limited")]
    RateLimit,
    #[error("verifier returned malformed output: {0}")]
    Malformed(String),
}

#[async_trait]
pub trait Verifier: Send + Sync {
    /// Run one verification. `condition` is the goal text; `last_turn` is the
    /// final assistant message (truncated by the caller to ≤4k chars).
    ///
    /// Returns `(Verdict, input_tokens, output_tokens)` so the driver can
    /// charge the verifier's spend against the goal's token budget.
    async fn verify(
        &self,
        condition: &str,
        last_turn: &str,
    ) -> Result<(Verdict, u64, u64), VerifierError>;
}

/// Parse a verdict from a raw Haiku response.
///
/// Expected format:
/// ```text
/// VERDICT: met
/// ```
/// or
/// ```text
/// VERDICT: not_met — tests still failing
/// ```
///
/// Tolerant of leading/trailing whitespace and `:` / `—` / `-` separators.
///
/// # Errors
/// Returns [`VerifierError::Malformed`] if no `VERDICT:` line is found or the
/// verdict word is neither `met` nor `not_met`.
pub fn parse_verdict(raw: &str) -> Result<Verdict, VerifierError> {
    for line in raw.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("VERDICT:") else { continue };
        let rest = rest.trim();
        if let Some(reason) = rest.strip_prefix("not_met") {
            let reason = reason
                .trim_start_matches([' ', '\t', '-', '\u{2014}'])
                .trim()
                .to_string();
            return Ok(Verdict::NotMet { reason });
        }
        if rest == "met" || rest.starts_with("met ") || rest.starts_with("met\t") {
            return Ok(Verdict::Met);
        }
        return Err(VerifierError::Malformed(line.to_string()));
    }
    Err(VerifierError::Malformed(raw.to_string()))
}
```

- [ ] **Step 3: Write verifier tests (parse_verdict + a mock used by driver tests later)**

Create `crates/origin-goal/tests/verifier_mock.rs`:
```rust
use origin_goal::verifier::{parse_verdict, Verdict, VerifierError};

#[test]
fn parses_met() {
    assert_eq!(parse_verdict("VERDICT: met").unwrap(), Verdict::Met);
}

#[test]
fn parses_not_met_with_em_dash() {
    let v = parse_verdict("VERDICT: not_met — tests still failing").unwrap();
    assert_eq!(v, Verdict::NotMet { reason: "tests still failing".into() });
}

#[test]
fn parses_not_met_with_ascii_dash() {
    let v = parse_verdict("VERDICT: not_met - missing migration").unwrap();
    assert_eq!(v, Verdict::NotMet { reason: "missing migration".into() });
}

#[test]
fn parses_not_met_no_separator() {
    let v = parse_verdict("VERDICT: not_met tests red").unwrap();
    assert_eq!(v, Verdict::NotMet { reason: "tests red".into() });
}

#[test]
fn ignores_preamble_lines() {
    let raw = "Some preamble Haiku felt like emitting.\nVERDICT: met";
    assert_eq!(parse_verdict(raw).unwrap(), Verdict::Met);
}

#[test]
fn malformed_when_no_verdict_line() {
    matches!(parse_verdict("VERDICT_OF_THE_PEOPLE"), Err(VerifierError::Malformed(_)));
}

#[test]
fn malformed_when_unknown_verdict_word() {
    matches!(parse_verdict("VERDICT: maybe"), Err(VerifierError::Malformed(_)));
}
```

- [ ] **Step 4: Run tests, confirm pass**

Run: `cargo test -p origin-goal --test verifier_mock`
Expected: `test result: ok. 7 passed`.

- [ ] **Step 5: Commit**

```bash
git add crates/origin-goal/
git commit -m "feat(goal): verifier trait + verdict parser"
```

---

## Task 9: Daemon driver — system-prompt injection in `agent.rs`

**Files:**
- Modify: `crates/origin-daemon/src/agent.rs`

- [ ] **Step 1: Add `goal: Arc<Mutex<Option<GoalState>>>` to `LoopOptions`**

Open `crates/origin-daemon/src/agent.rs`. Find the `LoopOptions` struct (around line 51). Add a new field at the bottom of the struct:
```rust
    /// Per-connection goal slot. The driver in `main.rs` mutates this; `run_loop`
    /// reads it under the lock to render the `<origin-goal>` system-prompt block.
    /// `Arc<Mutex<Option<_>>>` (not `Option<Arc<Mutex<_>>>`) so the driver can
    /// install/remove the goal without rebuilding `LoopOptions`.
    pub goal: Arc<tokio::sync::Mutex<Option<origin_goal::GoalState>>>,
```

Find the `Default` impl (search for `impl Default for LoopOptions`) and add:
```rust
goal: Arc::new(tokio::sync::Mutex::new(None)),
```

- [ ] **Step 2: Render the `<origin-goal>` block in `run_loop`**

In `run_loop` (around line 348), after the existing `workflows_block` rendering and before the system-prompt is assembled, add:

```rust
    let goal_block = {
        let guard = opts.goal.lock().await;
        if let Some(g) = guard.as_ref().filter(|g| matches!(
            g.status,
            origin_goal::GoalStatus::Active | origin_goal::GoalStatus::Verifying
        )) {
            format!(
                "<origin-goal>\nACTIVE GOAL — iteration {iter}/{max}, tokens spent {tok}/{budget}.\n\
                 \n\
                 Condition: {cond}\n\
                 \n\
                 You MUST end every response with exactly one <goal-status> tag:\n  \
                 <goal-status state=\"met|in_progress|blocked\"><reason>...</reason></goal-status>\n\
                 \n\
                 - met:         only when the condition is fully satisfied AND visible in this conversation's output\n\
                 - in_progress: real work is happening; describe what still remains in <reason>\n\
                 - blocked:     you need user input or an irreversible action; describe the blocker in <reason>\n\
                 \n\
                 The driver will auto-continue on in_progress, run a verifier on met, and surface blocked to the user.\n\
                 </origin-goal>",
                iter = g.iter,
                max = g.max_iter,
                tok = g.tokens_spent,
                budget = g.token_budget,
                cond = g.condition,
            )
        } else {
            String::new()
        }
    };
```

Find where `catalog_block` and `workflows_block` are concatenated into the final system prompt, and append `goal_block` there (placed AFTER the cache breakpoint — i.e. it must be one of the last blocks added).

- [ ] **Step 3: Verify it compiles**

Run: `cargo check -p origin-daemon`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/origin-daemon/src/agent.rs
git commit -m "feat(daemon): inject <origin-goal> block when goal active"
```

---

## Task 10: Daemon driver — main.rs routing for `/goal`

**Files:**
- Modify: `crates/origin-daemon/src/main.rs`
- Modify: `crates/origin-daemon/Cargo.toml` (Anthropic verifier impl)
- Create: `crates/origin-daemon/src/goal_driver.rs`

- [ ] **Step 1: Create `crates/origin-daemon/src/goal_driver.rs`**

```rust
//! Driver: after every `run_loop` return, decide whether to verify, iterate,
//! or clear the active goal. Per-connection.

use crate::protocol::StreamEvent;
use origin_goal::verifier::{Verdict, Verifier, VerifierError};
use origin_goal::{ClearReason, ClearReasonWire, GoalState, GoalStatus, TagOutcome, TagOutcomeWire};

/// What the connection task should do after handling the driver's decision.
pub enum DriverDecision {
    /// Goal is over; emit `Cleared` event and drop the GoalState.
    Cleared {
        reason: ClearReasonWire,
        iter: u32,
        tokens_spent: u64,
    },
    /// Iterate again with this synthesized user prompt.
    Iterate {
        synthesized_prompt: String,
        iter_event: StreamEvent,
    },
}

/// Translate a `TagOutcome` + cap state into a [`DriverDecision`].
///
/// Caller responsibilities:
/// - Charge `LoopSummary` tokens to `state` via `record_iteration` BEFORE calling.
/// - On `Iterate`, send the `iter_event` to the client, then call `run_loop`
///   with `synthesized_prompt` as the user message.
/// - On `Cleared`, send a `GoalCleared` event and drop the state from the
///   connection.
///
/// The verifier is called by the driver only on `TagOutcome::Met`. We pass it
/// in as a trait object so tests can substitute a `MockVerifier`.
pub async fn drive(
    state: &mut GoalState,
    last_turn_text: &str,
    verifier: &dyn Verifier,
) -> DriverDecision {
    // Cap check first — never overshoot.
    if let Some(reason) = state.cap_check() {
        return cleared(state, reason);
    }
    let tag = state.last_status_tag.clone().unwrap_or(TagOutcome::Missing);
    match tag {
        TagOutcome::Met => {
            let truncated = truncate_for_verifier(last_turn_text);
            match verifier.verify(&state.condition, &truncated).await {
                Ok((Verdict::Met, in_tok, out_tok)) => {
                    state.record_verifier_tokens(in_tok, out_tok);
                    cleared(
                        state,
                        ClearReason::Met {
                            reason: "verifier confirmed".into(),
                        },
                    )
                }
                Ok((Verdict::NotMet { reason }, in_tok, out_tok)) => {
                    state.record_verifier_tokens(in_tok, out_tok);
                    iterate(
                        state,
                        format!(
                            "[goal-driver] You claimed the goal was met, but the verifier disagreed: {reason}. \
                             Address that specific gap and continue."
                        ),
                    )
                }
                Err(VerifierError::RateLimit | VerifierError::Transport(_) | VerifierError::Malformed(_)) => {
                    // Fail open.
                    cleared(state, ClearReason::VerifierUnavailable)
                }
            }
        }
        TagOutcome::InProgress { what_remains } => iterate(
            state,
            format!(
                "[goal-driver] Continue toward the active goal. What remains: {}",
                if what_remains.is_empty() {
                    "unspecified — keep going.".to_string()
                } else {
                    what_remains
                }
            ),
        ),
        TagOutcome::Missing => iterate(
            state,
            "[goal-driver] Continue toward the active goal. What remains: \
             unknown — main model did not emit a <goal-status> tag last turn; \
             emit one this turn."
                .to_string(),
        ),
        TagOutcome::Blocked { why } => iterate(
            state,
            format!(
                "[goal-driver] Last turn reported the goal blocked: {why}. \
                 Either resolve the blocker yourself, or if it truly requires the human, \
                 restate the blocker clearly and end the turn — the driver will then \
                 clear the goal so the user can respond."
            ),
        ),
    }
}

fn cleared(state: &GoalState, reason: ClearReason) -> DriverDecision {
    DriverDecision::Cleared {
        reason: reason.into(),
        iter: state.iter,
        tokens_spent: state.tokens_spent,
    }
}

fn iterate(state: &GoalState, prompt: String) -> DriverDecision {
    let iter_event = StreamEvent::GoalIteration {
        iter: state.iter,
        tokens_spent: state.tokens_spent,
        last_tag: TagOutcomeWire::from(state.last_status_tag.clone().unwrap_or(TagOutcome::Missing)),
    };
    DriverDecision::Iterate { synthesized_prompt: prompt, iter_event }
}

const VERIFIER_INPUT_MAX_CHARS: usize = 4_000;

fn truncate_for_verifier(s: &str) -> String {
    if s.len() <= VERIFIER_INPUT_MAX_CHARS {
        return s.to_string();
    }
    let start = s.len() - VERIFIER_INPUT_MAX_CHARS;
    // Don't split a UTF-8 codepoint.
    let mut i = start;
    while !s.is_char_boundary(i) { i += 1; }
    s[i..].to_string()
}
```

Add `pub mod goal_driver;` to `crates/origin-daemon/src/lib.rs` (so integration tests can import it).

- [ ] **Step 2: Wire `/goal` activation in `main.rs`**

Find the existing `ClientMessage::ActivateSkill { name, args }` handler (Task 6 made `args` available). Insert a special-case BEFORE the generic skill activation:

```rust
if name == "goal" {
    // Bare /goal → status query
    let Some(raw_args) = args.as_deref() else {
        if let Some(g) = active_goal.lock().await.as_ref() {
            let ev = StreamEvent::GoalActive {
                condition: g.condition.clone(),
                max_iter: g.max_iter,
                token_budget: g.token_budget,
            };
            let body = serde_json::to_vec(&ev).unwrap_or_default();
            let _ = conn_clone.lock().await.write_frame(FrameKind::Event, &body).await;
        } else {
            let ev = StreamEvent::SkillError { message: "no active goal".into() };
            let body = serde_json::to_vec(&ev).unwrap_or_default();
            let _ = conn_clone.lock().await.write_frame(FrameKind::Event, &body).await;
        }
        continue;
    };
    // Parse args; replace any existing goal.
    match origin_goal::parse_goal_args(raw_args) {
        Ok(parsed) => {
            // Clear prior goal if one existed.
            let mut slot = active_goal.lock().await;
            if let Some(prior) = slot.take() {
                let ev = StreamEvent::GoalCleared {
                    reason: origin_goal::ClearReasonWire::UserSlash,
                    iter: prior.iter,
                    tokens_spent: prior.tokens_spent,
                };
                let body = serde_json::to_vec(&ev).unwrap_or_default();
                let _ = conn_clone.lock().await.write_frame(FrameKind::Event, &body).await;
            }
            let new_goal = origin_goal::GoalState::new(
                parsed.condition.clone(),
                parsed.max_iter,
                parsed.token_budget,
            );
            let active = StreamEvent::GoalActive {
                condition: new_goal.condition.clone(),
                max_iter: new_goal.max_iter,
                token_budget: new_goal.token_budget,
            };
            *slot = Some(new_goal);
            drop(slot);
            let body = serde_json::to_vec(&active).unwrap_or_default();
            let _ = conn_clone.lock().await.write_frame(FrameKind::Event, &body).await;
        }
        Err(e) => {
            let ev = StreamEvent::SkillError { message: format!("/goal: {e}") };
            let body = serde_json::to_vec(&ev).unwrap_or_default();
            let _ = conn_clone.lock().await.write_frame(FrameKind::Event, &body).await;
        }
    }
    continue;
}
```

Add at the top of the per-connection task setup (search for where `active_skills` is initialized; goal lives alongside):
```rust
let active_goal: Arc<tokio::sync::Mutex<Option<origin_goal::GoalState>>> =
    Arc::new(tokio::sync::Mutex::new(None));
```

Also extend `DeactivateSkill { name }` to clear the goal when `name == "goal"`:
```rust
if name == "goal" {
    let mut slot = active_goal.lock().await;
    if let Some(prior) = slot.take() {
        let ev = StreamEvent::GoalCleared {
            reason: origin_goal::ClearReasonWire::UserSlash,
            iter: prior.iter,
            tokens_spent: prior.tokens_spent,
        };
        let body = serde_json::to_vec(&ev).unwrap_or_default();
        let _ = conn_clone.lock().await.write_frame(FrameKind::Event, &body).await;
    }
    continue;
}
```

- [ ] **Step 3: Wrap the `Prompt` handler in the driver loop**

Find the `ClientMessage::Prompt(req)` handler. Replace the single `run_loop` call with a loop that:
1. Calls `run_loop` once for the user's prompt.
2. After it returns, if `active_goal` is `Some(Active)`, calls `drive(...)`. If `Iterate`, calls `run_loop` again with the synthesized prompt (and emits the `iter_event` first). If `Cleared`, emits `GoalCleared` and drops the goal.
3. Between iterations, also drains any pending `ClientMessage` from the connection — if the user sent a `Prompt` or `Interrupt`, break out of the goal loop and handle it normally.

Sketch (insert into existing handler — exact placement depends on local code shape):
```rust
let mut next_text: String = req.text.clone();
loop {
    // Capture tokens via LoopSummary.
    let summary = run_loop(&mut session, &next_text, provider.as_ref(), prompter.as_ref(),
                           &opts.clone().with_goal(active_goal.clone())).await?;

    // Find the final assistant turn text for tag parsing + verifier input.
    let last_assistant_text = session
        .last_assistant_text()
        .unwrap_or_default();
    let tag = origin_goal::parse_tag(&last_assistant_text);

    let decision = {
        let mut slot = active_goal.lock().await;
        let Some(g) = slot.as_mut() else { break };  // goal cleared externally
        g.record_iteration(summary.input_tokens, summary.output_tokens, tag);
        crate::goal_driver::drive(g, &last_assistant_text, verifier.as_ref()).await
    };

    match decision {
        crate::goal_driver::DriverDecision::Iterate { synthesized_prompt, iter_event } => {
            let body = serde_json::to_vec(&iter_event).unwrap_or_default();
            let _ = conn_clone.lock().await.write_frame(FrameKind::Event, &body).await;
            next_text = synthesized_prompt;
            // Yield to allow concurrent ClientMessage handling.
            tokio::task::yield_now().await;
            // If the user sent a new message while we were running, break.
            if /* check pending_prompt */ false { break; }
        }
        crate::goal_driver::DriverDecision::Cleared { reason, iter, tokens_spent } => {
            let ev = StreamEvent::GoalCleared { reason, iter, tokens_spent };
            let body = serde_json::to_vec(&ev).unwrap_or_default();
            let _ = conn_clone.lock().await.write_frame(FrameKind::Event, &body).await;
            *active_goal.lock().await = None;
            break;
        }
    }
}
```

(Concrete pending-message detection: peek at the connection's incoming-message channel via `try_recv`; if non-empty, set a "interrupt requested" flag and break out of the loop. The next outer-loop iteration will handle that message as normal.)

Add `LoopOptions::with_goal(...)` builder in `agent.rs`:
```rust
#[must_use]
pub fn with_goal(mut self, goal: Arc<tokio::sync::Mutex<Option<origin_goal::GoalState>>>) -> Self {
    self.goal = goal;
    self
}
```

(Task 9 already declared `LoopOptions::goal` with shape `Arc<Mutex<Option<GoalState>>>`, matching the connection's `active_goal` slot — no reconciliation needed.)

- [ ] **Step 4: Implement a real `AnthropicHaikuVerifier`**

Create `crates/origin-daemon/src/anthropic_verifier.rs`:
```rust
use async_trait::async_trait;
use origin_goal::verifier::{parse_verdict, Verdict, Verifier, VerifierError};
use origin_provider::{ChatRequest, Provider};
use origin_core::types::{Block, Message, Role};
use std::sync::Arc;

pub struct AnthropicHaikuVerifier {
    pub provider: Arc<dyn Provider>,
    pub model: String,        // e.g. "claude-haiku-4-5"
}

#[async_trait]
impl Verifier for AnthropicHaikuVerifier {
    async fn verify(
        &self,
        condition: &str,
        last_turn: &str,
    ) -> Result<(Verdict, u64, u64), VerifierError> {
        let system = "You verify whether a stated goal has been met based ONLY on \
                      the assistant's final response. Answer with exactly one of:\n\
                      VERDICT: met\n\
                      VERDICT: not_met — <one-sentence reason>";
        let user_text = format!(
            "Goal: {condition}\nAssistant's claim of completion: {last_turn}"
        );
        let req = ChatRequest {
            model: self.model.clone(),
            system: Some(system.to_string()),
            messages: vec![Message::new(Role::User).with_block(Block::text(&user_text))],
            tools: vec![],
            // ... fill any other required fields with defaults; consult ChatRequest definition
        };
        let resp = self
            .provider
            .chat(&req)
            .await
            .map_err(|e| VerifierError::Transport(e.to_string()))?;
        let text = resp
            .blocks
            .iter()
            .filter_map(|b| b.as_text())
            .collect::<Vec<_>>()
            .join("\n");
        let verdict = parse_verdict(&text)?;
        Ok((verdict, resp.usage.input_tokens as u64, resp.usage.output_tokens as u64))
    }
}
```

(Field names depend on the local `ChatRequest`/`ChatResponse` shapes — adapt to whatever those types currently look like. Consult `crates/origin-provider/src/lib.rs`.)

Add the module to `crates/origin-daemon/src/lib.rs`:
```rust
pub mod anthropic_verifier;
pub mod goal_driver;
```

Wire it in `main.rs` where the connection task is set up:
```rust
let verifier: Arc<dyn Verifier> = Arc::new(anthropic_verifier::AnthropicHaikuVerifier {
    provider: provider.clone(),
    model: "claude-haiku-4-5".to_string(),
});
```

- [ ] **Step 5: Build, fix any reconciliation errors**

Run: `cargo build -p origin-daemon`
Expected: clean. Fix any type mismatches (the `LoopOptions::goal` ↔ `active_goal` shape note above is the most likely sticking point).

- [ ] **Step 6: Commit**

```bash
git add crates/origin-daemon/
git commit -m "feat(daemon): goal driver + Haiku verifier wiring"
```

---

## Task 11: Integration test — `/goal` activates with inline args

**Files:**
- Create: `crates/origin-daemon/tests/goal_activates_with_inline_args.rs`

- [ ] **Step 1: Write the test**

```rust
//! Integration: /goal <cond> round-trips and emits GoalActive.

#![allow(clippy::unwrap_used)]

use origin_daemon::protocol::{ClientMessage, StreamEvent};

mod common;
use common::TestDaemon;

#[tokio::test]
async fn goal_activate_emits_goal_active() {
    let daemon = TestDaemon::start_with_mock_provider().await;
    let mut conn = daemon.connect().await;

    conn.send(ClientMessage::ActivateSkill {
        name: "goal".into(),
        args: Some("fix the failing tests".into()),
    })
    .await;

    let ev = conn.next_event().await.unwrap();
    match ev {
        StreamEvent::GoalActive { condition, max_iter, token_budget } => {
            assert_eq!(condition, "fix the failing tests");
            assert_eq!(max_iter, 20);          // default
            assert_eq!(token_budget, 200_000); // default
        }
        other => panic!("expected GoalActive, got {other:?}"),
    }
}

#[tokio::test]
async fn goal_activate_with_flags_uses_overrides() {
    let daemon = TestDaemon::start_with_mock_provider().await;
    let mut conn = daemon.connect().await;

    conn.send(ClientMessage::ActivateSkill {
        name: "goal".into(),
        args: Some("--max-iter=5 --budget=50k fix tests".into()),
    })
    .await;

    let ev = conn.next_event().await.unwrap();
    match ev {
        StreamEvent::GoalActive { condition, max_iter, token_budget } => {
            assert_eq!(condition, "fix tests");
            assert_eq!(max_iter, 5);
            assert_eq!(token_budget, 50_000);
        }
        other => panic!("expected GoalActive, got {other:?}"),
    }
}
```

The `common` module is a shared test harness; if origin's daemon tests already have one (check `crates/origin-daemon/tests/common.rs` or similar), adapt the imports. If not, create it as part of this task with at minimum:
- `TestDaemon::start_with_mock_provider()` — spawns a daemon with a `MockProvider` returning canned responses.
- `TestDaemon::connect()` — opens an IPC connection.
- `.send(ClientMessage)` / `.next_event() -> Option<StreamEvent>` helpers.

(Search the existing tests for patterns: `ls crates/origin-daemon/tests/` and look for how other integration tests bootstrap.)

- [ ] **Step 2: Run, confirm pass**

Run: `cargo test -p origin-daemon --test goal_activates_with_inline_args`
Expected: 2 PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/origin-daemon/tests/goal_activates_with_inline_args.rs crates/origin-daemon/tests/common.rs
git commit -m "test(goal): /goal activation round-trip"
```

---

## Task 12: Integration test — iterate on `in_progress`, stop on `met` (verifier confirms)

**Files:**
- Create: `crates/origin-daemon/tests/goal_iterates_on_in_progress.rs`

- [ ] **Step 1: Write the test**

```rust
#![allow(clippy::unwrap_used)]

use origin_daemon::protocol::{ClientMessage, StreamEvent, PromptRequest};

mod common;
use common::{TestDaemon, MockProviderScript};

#[tokio::test]
async fn three_in_progress_then_met_with_verifier_confirm() {
    let script = MockProviderScript::new()
        // Initial /goal activation doesn't consume a turn.
        // Turn 1 (user prompt "begin")
        .reply("working on it\n<goal-status state=\"in_progress\"><reason>step 1 of 3</reason></goal-status>")
        // Turn 2 (driver synthesized)
        .reply("more progress\n<goal-status state=\"in_progress\"><reason>step 2 of 3</reason></goal-status>")
        // Turn 3
        .reply("more progress\n<goal-status state=\"in_progress\"><reason>step 3 of 3</reason></goal-status>")
        // Turn 4 — model claims done
        .reply("done!\n<goal-status state=\"met\"><reason>tests green</reason></goal-status>")
        // Verifier sees the last turn — returns "met"
        .verifier_reply("VERDICT: met");

    let daemon = TestDaemon::start_with_script(script).await;
    let mut conn = daemon.connect().await;

    conn.send(ClientMessage::ActivateSkill {
        name: "goal".into(),
        args: Some("get the tests green".into()),
    }).await;
    let _ = conn.next_event().await; // GoalActive

    conn.send(ClientMessage::Prompt(PromptRequest {
        text: "begin".into(),
        ..Default::default()
    })).await;

    // Drain events; assert ordering.
    let mut iter_count = 0;
    let mut verifying = false;
    loop {
        match conn.next_event().await.unwrap() {
            StreamEvent::GoalIteration { iter, .. } => {
                iter_count += 1;
                assert_eq!(iter, iter_count);
            }
            StreamEvent::GoalVerifying => verifying = true,
            StreamEvent::GoalCleared { reason, iter, .. } => {
                assert!(matches!(reason, origin_goal::ClearReasonWire::Met { .. }));
                assert_eq!(iter, 4);
                break;
            }
            // Token events are fine, just ignore.
            _ => {}
        }
    }
    assert_eq!(iter_count, 3);   // 3 in_progress events; the 4th turn went straight to verify
    assert!(verifying);
    assert_eq!(daemon.script_replies_consumed(), 4);
    assert_eq!(daemon.verifier_calls(), 1);
}
```

- [ ] **Step 2: Extend the test harness if needed**

`MockProviderScript` should have:
- `.reply(text)` — push a canned assistant response.
- `.verifier_reply(text)` — push a canned verifier response.
- Backing the harness's mock `Provider` AND mock `Verifier`.

If the existing harness doesn't have these methods, add them as part of this task.

- [ ] **Step 3: Run, confirm pass**

Run: `cargo test -p origin-daemon --test goal_iterates_on_in_progress`
Expected: 1 PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/origin-daemon/tests/
git commit -m "test(goal): iterate-then-verify happy path"
```

---

## Task 13: Integration test — max-iter cap

**Files:**
- Create: `crates/origin-daemon/tests/goal_max_iter_caps.rs`

- [ ] **Step 1: Write the test**

```rust
#![allow(clippy::unwrap_used)]

use origin_daemon::protocol::{ClientMessage, PromptRequest, StreamEvent};

mod common;
use common::{TestDaemon, MockProviderScript};

#[tokio::test]
async fn always_in_progress_caps_at_max_iter() {
    let mut script = MockProviderScript::new();
    // Always emit in_progress.
    for _ in 0..50 {
        script = script.reply(
            "still working\n<goal-status state=\"in_progress\"><reason>more</reason></goal-status>",
        );
    }
    let daemon = TestDaemon::start_with_script(script).await;
    let mut conn = daemon.connect().await;

    conn.send(ClientMessage::ActivateSkill {
        name: "goal".into(),
        args: Some("--max-iter=3 keep at it".into()),
    }).await;
    let _ = conn.next_event().await; // GoalActive

    conn.send(ClientMessage::Prompt(PromptRequest {
        text: "begin".into(),
        ..Default::default()
    })).await;

    let mut last = None;
    while let Some(ev) = conn.next_event().await {
        if let StreamEvent::GoalCleared { ref reason, iter, .. } = ev {
            assert!(matches!(reason, origin_goal::ClearReasonWire::MaxIter));
            assert_eq!(iter, 3);
            last = Some(ev);
            break;
        }
    }
    assert!(last.is_some());
    assert_eq!(daemon.script_replies_consumed(), 3); // exactly max_iter
}
```

- [ ] **Step 2: Run, confirm pass**

Run: `cargo test -p origin-daemon --test goal_max_iter_caps`
Expected: 1 PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/origin-daemon/tests/goal_max_iter_caps.rs
git commit -m "test(goal): max-iter cap"
```

---

## Task 14: Integration test — token budget cap

**Files:**
- Create: `crates/origin-daemon/tests/goal_budget_caps.rs`

- [ ] **Step 1: Write the test**

```rust
#![allow(clippy::unwrap_used)]

use origin_daemon::protocol::{ClientMessage, PromptRequest, StreamEvent};

mod common;
use common::{TestDaemon, MockProviderScript};

#[tokio::test]
async fn cumulative_tokens_trigger_budget_cap() {
    let script = MockProviderScript::new()
        .reply_with_usage(
            "<goal-status state=\"in_progress\"><reason>x</reason></goal-status>",
            40_000, 10_000,
        )
        .reply_with_usage(
            "<goal-status state=\"in_progress\"><reason>x</reason></goal-status>",
            40_000, 10_000,
        )
        .reply_with_usage(
            "<goal-status state=\"in_progress\"><reason>x</reason></goal-status>",
            40_000, 10_000,
        );
    let daemon = TestDaemon::start_with_script(script).await;
    let mut conn = daemon.connect().await;

    conn.send(ClientMessage::ActivateSkill {
        name: "goal".into(),
        args: Some("--budget=120k stuff".into()),
    }).await;
    let _ = conn.next_event().await; // GoalActive

    conn.send(ClientMessage::Prompt(PromptRequest {
        text: "begin".into(),
        ..Default::default()
    })).await;

    loop {
        if let StreamEvent::GoalCleared { reason, iter, tokens_spent } = conn.next_event().await.unwrap() {
            assert!(matches!(reason, origin_goal::ClearReasonWire::BudgetExhausted));
            // Cap-check fires AFTER the iteration that crosses, so iter is 3 and tokens_spent ≥ budget.
            assert_eq!(iter, 3);
            assert!(tokens_spent >= 120_000);
            break;
        }
    }
}
```

(`MockProviderScript::reply_with_usage(text, input_tokens, output_tokens)` is a new harness method; add it as part of this task.)

- [ ] **Step 2: Run, confirm pass**

Run: `cargo test -p origin-daemon --test goal_budget_caps`
Expected: 1 PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/origin-daemon/tests/
git commit -m "test(goal): token-budget cap"
```

---

## Task 15: Integration test — user interrupt during iteration

**Files:**
- Create: `crates/origin-daemon/tests/goal_user_interrupt_during_iteration.rs`

- [ ] **Step 1: Write the test**

```rust
#![allow(clippy::unwrap_used)]

use origin_daemon::protocol::{ClientMessage, PromptRequest, StreamEvent};
use std::time::Duration;

mod common;
use common::{TestDaemon, MockProviderScript};

#[tokio::test]
async fn interrupt_clears_goal_with_user_slash_reason() {
    let mut script = MockProviderScript::new();
    for _ in 0..50 {
        script = script
            .reply("<goal-status state=\"in_progress\"><reason>x</reason></goal-status>")
            .with_reply_delay(Duration::from_millis(50));
    }
    let daemon = TestDaemon::start_with_script(script).await;
    let mut conn = daemon.connect().await;

    conn.send(ClientMessage::ActivateSkill {
        name: "goal".into(),
        args: Some("keep going forever".into()),
    }).await;
    let _ = conn.next_event().await;

    conn.send(ClientMessage::Prompt(PromptRequest {
        text: "begin".into(),
        ..Default::default()
    })).await;

    // Let one iteration land, then interrupt.
    let _ = conn.next_event().await; // first GoalIteration
    conn.send(ClientMessage::Interrupt).await;

    // Expect GoalCleared { UserSlash }
    loop {
        if let StreamEvent::GoalCleared { reason, .. } = conn.next_event().await.unwrap() {
            assert!(matches!(reason, origin_goal::ClearReasonWire::UserSlash));
            break;
        }
    }
    // No more iterations should fire.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(daemon.script_replies_consumed() < 50);
}
```

- [ ] **Step 2: Run, confirm pass**

Run: `cargo test -p origin-daemon --test goal_user_interrupt_during_iteration`
Expected: 1 PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/origin-daemon/tests/
git commit -m "test(goal): user interrupt cancels iteration"
```

---

## Task 16: Integration test — verifier rejection resumes iteration

**Files:**
- Create: `crates/origin-daemon/tests/goal_verifier_rejection_resumes.rs`

- [ ] **Step 1: Write the test**

```rust
#![allow(clippy::unwrap_used)]

use origin_daemon::protocol::{ClientMessage, PromptRequest, StreamEvent};

mod common;
use common::{TestDaemon, MockProviderScript};

#[tokio::test]
async fn verifier_rejection_injects_reason_and_resumes() {
    let script = MockProviderScript::new()
        // Turn 1: model claims met prematurely
        .reply("<goal-status state=\"met\"><reason>looks done to me</reason></goal-status>")
        // Verifier rejects
        .verifier_reply("VERDICT: not_met — tests still failing")
        // Turn 2: model addresses the gap, claims met again
        .reply("fixed the failing tests\n<goal-status state=\"met\"><reason>green now</reason></goal-status>")
        .verifier_reply("VERDICT: met");

    let daemon = TestDaemon::start_with_script(script).await;
    let mut conn = daemon.connect().await;

    conn.send(ClientMessage::ActivateSkill {
        name: "goal".into(),
        args: Some("get tests green".into()),
    }).await;
    let _ = conn.next_event().await;

    conn.send(ClientMessage::Prompt(PromptRequest {
        text: "go".into(),
        ..Default::default()
    })).await;

    let mut verifying_count = 0;
    loop {
        match conn.next_event().await.unwrap() {
            StreamEvent::GoalVerifying => verifying_count += 1,
            StreamEvent::GoalCleared { reason, iter, .. } => {
                assert!(matches!(reason, origin_goal::ClearReasonWire::Met { .. }));
                assert_eq!(iter, 2);
                break;
            }
            _ => {}
        }
    }
    assert_eq!(verifying_count, 2);
    // Assert the second user-facing prompt sent to the provider contained the verifier's reason.
    let prompts = daemon.captured_synth_prompts();
    assert!(prompts.iter().any(|p| p.contains("tests still failing")));
}
```

(Add `TestDaemon::captured_synth_prompts() -> Vec<String>` to the harness — records every `[goal-driver]`-prefixed prompt sent to the mock provider.)

- [ ] **Step 2: Run, confirm pass**

Run: `cargo test -p origin-daemon --test goal_verifier_rejection_resumes`
Expected: 1 PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/origin-daemon/tests/
git commit -m "test(goal): verifier rejection resumes iteration"
```

---

## Task 17: Resume-token persistence

**Files:**
- Modify: `crates/origin-resume-token/Cargo.toml`
- Modify: `crates/origin-resume-token/src/lib.rs`
- Create: `crates/origin-resume-token/tests/goal_snapshot_round_trip.rs`

- [ ] **Step 1: Add dep**

In `crates/origin-resume-token/Cargo.toml`:
```toml
[dependencies]
origin-goal = { path = "../origin-goal" }
```

- [ ] **Step 2: Add the field**

In `crates/origin-resume-token/src/lib.rs`, modify `ResumeToken`:
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResumeToken {
    pub session_id: String,
    pub last_turn: u32,
    pub cas_handle_root: [u8; 32],
    pub pending_tool_calls: Vec<String>,
    pub plan_seq: u64,
    #[serde(default)]
    pub goal: Option<origin_goal::GoalSnapshot>,
}
```

- [ ] **Step 3: Write the round-trip test**

Create `crates/origin-resume-token/tests/goal_snapshot_round_trip.rs`:
```rust
#![allow(clippy::unwrap_used)]

use origin_goal::{ClearReasonWire, GoalSnapshot, GoalStatusWire};
use origin_resume_token::ResumeToken;

#[test]
fn token_round_trips_with_active_goal() {
    let token = ResumeToken {
        session_id: "s1".into(),
        last_turn: 7,
        cas_handle_root: [0; 32],
        pending_tool_calls: vec![],
        plan_seq: 3,
        goal: Some(GoalSnapshot {
            condition: "do the thing".into(),
            iter: 4,
            max_iter: 20,
            tokens_spent: 12_345,
            token_budget: 200_000,
            started_at_unix: 1_716_000_000,
            status: GoalStatusWire::Active,
        }),
    };
    let bytes = serde_json::to_vec(&token).unwrap();
    let back: ResumeToken = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(back.goal.as_ref().unwrap().condition, "do the thing");
    assert_eq!(back.goal.as_ref().unwrap().iter, 4);
    assert!(matches!(back.goal.unwrap().status, GoalStatusWire::Active));
}

#[test]
fn token_round_trips_without_goal_field_backward_compat() {
    // Old-format token bytes — no `goal` key.
    let raw = r#"{
        "session_id": "s1",
        "last_turn": 0,
        "cas_handle_root": [0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],
        "pending_tool_calls": [],
        "plan_seq": 0
    }"#;
    let token: ResumeToken = serde_json::from_str(raw).unwrap();
    assert!(token.goal.is_none());
}

#[test]
fn token_round_trips_with_terminal_status() {
    let token = ResumeToken {
        session_id: "s1".into(),
        last_turn: 7,
        cas_handle_root: [0; 32],
        pending_tool_calls: vec![],
        plan_seq: 3,
        goal: Some(GoalSnapshot {
            condition: "x".into(),
            iter: 5,
            max_iter: 20,
            tokens_spent: 1_000,
            token_budget: 200_000,
            started_at_unix: 1_716_000_000,
            status: GoalStatusWire::Cleared { by: ClearReasonWire::MaxIter },
        }),
    };
    let bytes = serde_json::to_vec(&token).unwrap();
    let back: ResumeToken = serde_json::from_slice(&bytes).unwrap();
    assert!(matches!(
        back.goal.unwrap().status,
        GoalStatusWire::Cleared { by: ClearReasonWire::MaxIter }
    ));
}
```

- [ ] **Step 4: Run, confirm pass**

Run: `cargo test -p origin-resume-token --test goal_snapshot_round_trip`
Expected: `test result: ok. 3 passed`.

- [ ] **Step 5: Wire snapshot creation + hydration in daemon**

In `crates/origin-daemon/src/main.rs`, wherever the session is checkpointed (search for `ResumeToken {`), populate `goal:` from `active_goal`:
```rust
goal: active_goal.lock().await.as_ref().map(|g| origin_goal::GoalSnapshot {
    condition: g.condition.clone(),
    iter: g.iter,
    max_iter: g.max_iter,
    tokens_spent: g.tokens_spent,
    token_budget: g.token_budget,
    started_at_unix: g.started_at.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
    status: g.status.clone().into(),
}),
```

In the `ResumeRequest` handler (~line 836), if `token.goal.is_some()` and the snapshot's status is `Active`, reconstruct `GoalState` and emit `GoalActive`. Do NOT auto-iterate.

- [ ] **Step 6: Commit**

```bash
git add crates/origin-resume-token/ crates/origin-daemon/src/main.rs
git commit -m "feat(goal): persist + restore goal in resume token"
```

---

## Task 18: Replace stub `goal/SKILL.md` with the real spec

**Files:**
- Modify: `crates/origin-skills/embedded/superpowers/goal/SKILL.md`

- [ ] **Step 1: Replace the file**

```markdown
---
name: goal
description: Invoked by /goal <condition>. Sets a persistent completion condition for the session — origin commits to keep working toward that condition across turns, with a Haiku-backed verifier deciding when it's met. Use when the user types /goal, says 'set a goal', 'keep going until X', 'work toward X until done', or otherwise wants sustained focus on a single objective without re-prompting at every turn.
---

# Goal

## Overview

`/goal <condition>` sets one completion condition. The daemon auto-iterates while you report `in_progress` or `blocked`, calls a Haiku verifier when you claim `met`, and clears on verifier confirmation, max-iter, budget, or user interrupt.

## Activation

```
/goal <condition>
/goal --max-iter=N <condition>
/goal --budget=200k <condition>
/goal --max-iter=N --budget=200k <condition>
/-goal                 # clear active goal
/clear                 # also clears active goal
```

`--max-iter` defaults to 20. `--budget` defaults to 200000 (suffixes `k`/`m` accepted). Activating `/goal` while one is active replaces the old condition.

## The tag protocol — YOU MUST FOLLOW THIS

When a goal is active, the system prompt will include an `<origin-goal>` block telling you the active condition. **You MUST end every response with exactly one `<goal-status>` tag:**

```
<goal-status state="met|in_progress|blocked"><reason>one-line summary</reason></goal-status>
```

- `met`         — only when the condition is fully satisfied AND visible in this conversation's output.
- `in_progress` — real work is happening; describe what still remains in `<reason>`.
- `blocked`     — you need user input or an irreversible action; describe the blocker in `<reason>`.

If you forget the tag, the driver treats it as `in_progress` with `Missing` outcome and synthesizes a "you forgot the tag, emit one this turn" prompt.

## How the driver responds

| You emit | Driver action |
|---|---|
| `in_progress` | Synthesizes `[goal-driver] Continue toward the active goal. What remains: <reason>` and re-invokes you. |
| `blocked` | Synthesizes a "resolve or restate the blocker" prompt; iterates one more turn. If you're still blocked, clears the goal so the user can respond. |
| `met` | Emits `GoalVerifying`, runs Haiku verifier. If verifier confirms → `GoalCleared { Met }`. If verifier rejects → synthesizes `[goal-driver] You claimed met but verifier disagreed: <reason>. Address that.` and iterates. |
| (no tag) | Treated as `Missing` → same as `in_progress` with an explicit "emit a tag" nudge. |

## Synthesized `[goal-driver]` prompts

When you receive a user message prefixed `[goal-driver]`, it's the driver, not the human. Don't address it as a user-facing reply — just continue the work.

## Safety guarantees

- Iteration count cannot exceed `--max-iter`.
- Token spend cannot exceed `--budget` by more than the iteration that crosses it.
- Verifier tokens count against the budget.
- Permission prompts still fire normally for every tool call inside an iteration.
- Subagents do NOT inherit the parent's goal state.
- If the verifier is rate-limited or errors, the driver fails open: trust your `met` claim and stop.

## When NOT to use

- For one-shot tasks that fit in a single turn — just do it.
- For tasks whose completion is invisible from the conversation (e.g., "until the prod metric drops") — verifier sees only the transcript.
- For open-ended exploration where the "done" condition is genuinely unknown — agree on a condition first or use brainstorming.

## Red flags

- You stopped emitting `<goal-status>` mid-conversation → resume next turn.
- You claimed `met` without evidence in the same response → restate the evidence; expect verifier rejection.
- You're answering a `[goal-driver]` message as if it were the user → re-read the protocol.
```

- [ ] **Step 2: Confirm the embedded-skills test still passes**

The count is still 16 (replacing the body, not adding a skill). Run:
```bash
cargo clean -p origin-skills && cargo test -p origin-skills --test embedded_skills
```
Expected: 2 PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/origin-skills/embedded/superpowers/goal/SKILL.md
git commit -m "feat(skill): replace goal skill stub with real protocol"
```

---

## Task 19: Cross-crate smoke build + workspace tests

- [ ] **Step 1: Full workspace build**

Run: `cargo build --workspace`
Expected: clean.

- [ ] **Step 2: Full workspace tests**

Run: `cargo test --workspace`
Expected: all green. If any pre-existing test fails (unrelated), note it but do not fix in this plan — it's outside scope.

- [ ] **Step 3: Run clippy at workspace lint level**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean. Fix any new lints in `origin-goal` or in the `agent.rs`/`main.rs` edits.

- [ ] **Step 4: Commit any lint fixes**

```bash
git add -p
git commit -m "chore(goal): clippy cleanup"
```

---

## Self-Review

### Spec coverage

| Spec section | Plan task(s) |
|---|---|
| §1 Architecture: new `origin-goal` crate | Task 1 |
| §1 CLI parser change | Task 6 |
| §1 Protocol changes | Tasks 6, 7 |
| §1 `agent.rs` system-prompt injection | Task 9 |
| §1 `main.rs` driver | Task 10 |
| §1 Resume token | Task 17 |
| §1 Skill body | Task 18 |
| §2 `GoalState` types | Task 4 |
| §2 Tag protocol + parser | Task 2 |
| §2 Verifier prompt | Task 8, 10 |
| §3 `ActivateSkill { args }` | Task 6 |
| §3 `Goal*` StreamEvents | Task 7 |
| §3 Flag parser | Task 3 |
| §3 Concurrent user input handling | Task 10 step 3 + Task 15 |
| §4 Driver lives in connection task | Task 10 |
| §4 Synthesized prompts | Task 10 (goal_driver.rs) |
| §4 `<origin-goal>` block | Task 9 |
| §4 Token & iteration accounting | Task 4 |
| §5 Resume token schema | Task 17 |
| §5 Safety invariants | Tasks 4, 10 |
| §5 Error handling matrix | Tasks 10, 8 |
| §6 Unit tests in origin-goal | Tasks 2, 3, 4, 8 |
| §6 Integration tests in origin-daemon | Tasks 11–16 |
| §6 Resume-token round-trip | Task 17 |
| §6 Skill catalog test (no count change) | Task 18 step 2 |
| §6 CLI parser test | Task 6 |

All spec sections covered.

### Placeholder scan

Scanned plan — three judgment-call sites flagged as "adapt to local code shape" (not placeholders, but execution-time decisions):

1. **Task 10 step 4**: `ChatRequest`/`ChatResponse` field shapes depend on whatever `origin-provider` currently exposes. The skeleton names the function calls but the executor will need to look up the actual signatures. This is unavoidable without reading the full provider source here.
2. **Task 10 step 3**: The pending-message detection inside the goal loop ("if user sent a new message while iterating, break") is sketched but not fully spelled out — the exact channel name depends on `main.rs`'s connection task structure. Acceptable: the executor reads the surrounding 50 lines and uses the existing pattern.
3. **Task 11+**: Test harness `TestDaemon` / `MockProviderScript` are referenced but may need to be created from scratch if origin doesn't have an integration-test harness yet. Task 11 step 2 explicitly says to check and adapt or create.

None of these are "fill in details" placeholders — each says exactly what to look at.

### Type consistency

- `GoalArgs` fields: `condition: String`, `max_iter: Option<u32>`, `token_budget: Option<u64>` — used consistently across Tasks 3, 4, 10.
- `TagOutcome` variants: `Met | InProgress { what_remains } | Blocked { why } | Missing` — match between Tasks 2, 4, 10 (driver), 18 (skill body).
- `ClearReason` variants match the spec §2 enum + §5 fix (`Met { reason }`, `VerifierUnavailable`).
- `LoopOptions::goal` shape: Task 9 and Task 10 both use `Arc<Mutex<Option<GoalState>>>` (corrected during self-review). Consistent.

