# `/goal` — Persistent completion conditions with self-tagged auto-iteration

**Date:** 2026-05-28
**Status:** Approved (sections §1–§6)
**Owner:** Ainsley Woo

## Summary

`/goal <condition>` sets a persistent completion condition for the current connection. The main model emits an inline `<goal-status>` tag at the end of every response. The daemon parses the tag, auto-iterates while `in_progress` or `blocked`, and runs a single Haiku verification pass only when the main model claims `met`. The loop is bounded by max-iteration and token-budget caps and survives daemon restart via the resume token.

The novel mechanism vs. Claude Code's `/goal` (which runs a full-transcript Haiku evaluator after **every** assistant turn): origin shifts evaluation onto the main model via a self-tag, runs the verifier **at most once per goal**, and the verifier sees only the goal + last turn — not the transcript. Expected token cost per goal of N iterations: roughly `~80 × N` system-prompt tokens for the `<origin-goal>` block plus one verifier call (~1k input tokens), vs. baseline `~50k × N` for transcript-walking evaluation. Wins on tokens for any goal that takes more than one iteration.

## §1 Architecture

A new `origin-goal` crate owns the goal state machine. The daemon's per-connection task holds an `Option<GoalState>`; when active, after each `run_loop` returns the daemon inspects the final assistant turn for an inline `<goal-status>` tag, decides whether to verify-and-stop or auto-iterate, and (if iterating) calls `run_loop` again with a synthesized continuation prompt. The CLI consumes a stream of `Goal*` events and renders `◎ goal · iter 4/20 · 18.2k tok` in the status line.

### Crate touch list

- `origin-goal` — new crate. State machine, tag parser, verifier client, budget accounting.
- `origin-cli/src/input.rs` — parser change: `/name args...` now valid; `ClientMessage::ActivateSkill { args: Option<String> }`.
- `origin-daemon/src/protocol.rs` — extend `ActivateSkill`, add `StreamEvent::{GoalActive, GoalIteration, GoalVerifying, GoalCleared}`.
- `origin-daemon/src/main.rs` — wire `/goal` activation to instantiate `GoalState`; after each `run_loop`, run the goal driver.
- `origin-daemon/src/agent.rs` — `LoopOptions` gains `goal: Option<Arc<Mutex<GoalState>>>`; system prompt injects the `<origin-goal>` block.
- `origin-resume-token` — extend token schema with optional `goal: Option<GoalSnapshot>`.
- `origin-skills/embedded/superpowers/goal/SKILL.md` — replace today's model-driven stub with the real protocol spec.

## §2 Goal state machine (`origin-goal`)

```rust
pub struct GoalState {
    pub condition: String,                     // up to 4_000 chars; validated at activation
    pub status: GoalStatus,
    pub iter: u32,                             // 0-indexed; incremented after each driver tick
    pub max_iter: u32,                         // default 20
    pub tokens_spent: u64,                     // input + output across iterations
    pub token_budget: u64,                     // default 200_000
    pub started_at: SystemTime,
    pub last_status_tag: Option<TagOutcome>,
}

pub enum GoalStatus {
    Active,
    Verifying,                                 // Haiku call in flight
    Met { reason: String },                    // terminal — verifier confirmed
    Cleared { by: ClearReason },               // terminal — user/cap/etc.
}

pub enum ClearReason {
    UserSlash,
    UserClearAll,
    MaxIter,
    BudgetExhausted,
    VerifierRejected(String),
    Met { reason: String },                   // terminal success — verifier confirmed,
                                              // or fail-open when verifier unavailable
    VerifierUnavailable,                      // network/rate-limit; fail open → treated as Met by clients
}

pub enum TagOutcome {
    Met,
    InProgress { what_remains: String },
    Blocked { why: String },
    Missing,                                   // tag absent — treated as InProgress
}
```

### Driver lifecycle (after each `run_loop` returns)

1. Parse the `<goal-status>` tag from the final assistant turn.
2. If `iter + 1 > max_iter` or `tokens_spent >= token_budget` → emit `GoalCleared` (cap reason); stop.
3. Otherwise dispatch on tag outcome:
   - `Met` → emit `GoalVerifying`, call Haiku verifier. If `met` → emit `GoalCleared { Met }` and stop. If `not_met` → inject the verifier's reason as next-turn nudge and resume iteration.
   - `InProgress { what_remains }` or `Missing` → synthesize continuation prompt, call `run_loop` again.
   - `Blocked { why }` → synthesize a "resolve or restate the blocker" prompt; iterate one more turn. If the next tag is also `Blocked`, clear with the blocker surfaced as the reason (lets the user respond).

### Tag protocol the main model emits

```
<goal-status state="met|in_progress|blocked">
<reason>one-line summary of progress, blocker, or evidence of completion</reason>
</goal-status>
```

Parser is tolerant: whitespace/case-insensitive on `state=`, missing `<reason>` defaults to empty, extra attributes ignored. **Missing tag → `Missing` outcome (treated as InProgress)** so a forgetful model never accidentally ends the loop.

### Verifier prompt

```
System (cacheable): You verify whether a stated goal has been met based ONLY on
the assistant's final response. Answer with exactly one of:
  VERDICT: met
  VERDICT: not_met — <one-sentence reason>

User: Goal: <condition>
Assistant's claim of completion: <final turn text, truncated to last 4k chars>
```

The verifier sees only goal + last turn — not the full transcript. This is the token-efficiency win.

## §3 Wire protocol & event stream

### `ClientMessage` change

```rust
ActivateSkill { name: String, args: Option<String> },
```

No new `ClientMessage` for goals — `/goal <cond>` rides the generalized `ActivateSkill`. The daemon recognizes `name == "goal"` and routes through the goal driver instead of plain skill activation. `/-goal` and `/clear` both clear via the existing `DeactivateSkill` / context-reset paths.

### New `StreamEvent`s (server → client)

```rust
GoalActive   { condition: String, max_iter: u32, token_budget: u64 },
GoalIteration{ iter: u32, tokens_spent: u64, last_tag: TagOutcomeWire },
GoalVerifying,
GoalCleared  { reason: ClearReasonWire, iter: u32, tokens_spent: u64 },
```

`ClearReasonWire` variants: `user_slash | user_clear_all | max_iter | budget_exhausted | verifier_rejected { why } | met { reason } | verifier_unavailable`.

`verifier_unavailable` is emitted when the Haiku verifier is rate-limited, network-down, or returns a malformed response — i.e., when we cannot prove the goal was met but trust the main model's `met` claim. (Earlier drafts of this spec called this case `met { reason: "verifier unavailable" }`; the implementation chose a distinct variant so clients can render it differently from a verified completion.)

### CLI argument parsing (`origin-cli/src/input.rs`)

```
/goal <condition>
/goal --max-iter=N --budget=200k <condition>
/goal --max-iter=N <condition>
/goal --budget=500k <condition>
/-goal           → clears (existing DeactivateSkill path)
/goal            → bare form: show status if active, error otherwise
```

Flag parser is a hand-written split on `--key=val` tokens before the first non-flag token; everything after is the condition. Suffixes `k`/`m` accepted on `--budget`. Unknown flags → activation error event; no partial-state activation.

### Concurrent user input

While the driver is auto-iterating, if the user sends a `ClientMessage::Prompt`:
1. Cancel the in-flight `run_loop` for the current iteration.
2. Append the user message to the session.
3. Restart `run_loop` with the user message as the latest turn.
4. After that turn, the driver tick fires normally — goal continues unless the user's message implicitly cleared it.

`Ctrl+C` (existing `ClientMessage::Interrupt`) is treated as a clear with `ClearReason::UserSlash`.

## §4 Auto-iteration loop & system-prompt injection

### Where the driver lives

`origin-daemon/src/main.rs` already has a per-connection task that consumes `ClientMessage`s and calls `run_loop`. The goal driver is a small wrapper around that loop: after each `run_loop` returns success, check `goal_state` — if `Active`, decide next action (verify, iterate, or clear). No new task spawned; the existing connection task drives everything sequentially, so cancellation, session locking, and event ordering all work without new primitives.

### Synthesized continuation prompts

For `TagOutcome::InProgress { what_remains }` or `Missing`:
```
[goal-driver] Continue toward the active goal. What remains: <what_remains or
"unknown — main model did not emit a <goal-status> tag last turn; emit one this turn">.
```

For `TagOutcome::Blocked { why }`:
```
[goal-driver] Last turn reported the goal blocked: <why>. Either resolve the
blocker yourself, or if it truly requires the human, restate the blocker
clearly and end the turn — the driver will then clear the goal so the user can respond.
```

For verifier-rejection resume:
```
[goal-driver] You claimed the goal was met, but the verifier disagreed:
<verifier reason>. Address that specific gap and continue.
```

These are marked `[goal-driver]` so the model can distinguish them from real user turns; the skill body teaches the model not to address `[goal-driver]` prompts as user-facing replies.

### System-prompt injection (`origin-daemon/src/agent.rs`)

When a goal is active, `run_loop` prepends a block to the system prompt:

```
<origin-goal>
ACTIVE GOAL — iteration <iter>/<max_iter>, tokens spent <tokens>/<budget>.

Condition: <condition>

You MUST end every response with exactly one <goal-status> tag:
  <goal-status state="met|in_progress|blocked"><reason>...</reason></goal-status>

- met:         only when the condition is fully satisfied AND visible in this conversation's output
- in_progress: real work is happening; describe what still remains in <reason>
- blocked:     you need user input or an irreversible action; describe the blocker in <reason>

The driver will auto-continue on in_progress, run a verifier on met, and surface blocked to the user.
</origin-goal>
```

Sits adjacent to the existing `<origin-skills>` / `<origin-workflows>` blocks. **Cache placement:** the block changes every iteration (iter counter, token spend), so it lives *after* the cache breakpoint on the cached system prompt — only ~80 tokens re-tokenize per iteration.

### Token & iteration accounting

Per-iteration sequence:
1. **Cap check** (top of loop): if `iter >= max_iter` or `tokens_spent >= token_budget`, emit `GoalCleared` with the appropriate cap reason and stop **without** calling `run_loop`. This guarantees the cap is never overshot.
2. Call `run_loop`.
3. Add `summary.input_tokens + summary.output_tokens` to `tokens_spent`.
4. Increment `iter`.
5. Parse tag, dispatch (verify / iterate / clear). If dispatch decides to iterate, return to step 1.

`LoopSummary` already exposes token counts (used by sidecar billing), so no new instrumentation is needed.

## §5 Resume token, safety, error handling

### Resume token schema (`origin-resume-token`)

```rust
pub struct ResumeToken {
    // ... existing fields ...
    #[serde(default)]
    pub goal: Option<GoalSnapshot>,
}

pub struct GoalSnapshot {
    pub condition: String,
    pub iter: u32,
    pub max_iter: u32,
    pub tokens_spent: u64,
    pub token_budget: u64,
    pub started_at_unix: u64,
    pub status: GoalStatusWire,    // Active | Verifying | Met | Cleared (terminals included for transcript replay)
}
```

`#[serde(default)]` → old tokens deserialize fine (no goal). The existing MAC covers the new field automatically since the serializer hashes the full serialized bytes.

On `ResumeRequest` with `goal.is_some() && goal.status == Active`, the daemon reconstructs `GoalState` and emits `GoalActive` so the user sees the goal is back, but does **not** auto-iterate immediately — it waits for the user's next `Prompt` (or `Interrupt`) before resuming the driver. The next `Prompt` triggers a normal `run_loop`; the driver then takes over again from its post-`run_loop` tick. (Rationale: blindly resuming auto-iteration on a process restart is too surprising; the user may not even know they have a daemon running.) No new verb needed.

### Safety invariants

1. **Iteration & budget caps are checked at the top of every iteration**, before any `run_loop` call (per §4 accounting sequence). A goal can never exceed `max_iter` model invocations or overshoot `token_budget` by more than one iteration's worth (the iteration in flight when the cap was crossed).
2. **Token budget is also checked before the verifier call.** Verifier tokens count against the budget.
3. **Permission system is unchanged** — every tool call inside a goal iteration still hits `check_with_skills`. A goal does not bypass user permission prompts.
4. **Per-connection goal limit: 1.** Activating `/goal` while one is already active replaces the old condition (after emitting `GoalCleared { UserSlash }` for the prior one). Documented in the skill body.
5. **Verifier rate-limit / error → fail open.** If Haiku returns rate-limit, network error, or a malformed response, the driver logs at warn level, emits `GoalCleared { Met { reason: "verifier unavailable; trusting main model" } }`, and stops. Fails *toward* the user's stated completion, never toward burning more budget.
6. **Subagent isolation**: subagents spawned via the Task tool do **not** inherit the parent's goal state. Goal lives at the connection level, not the `run_loop` level.

### Error handling matrix

| Failure | Driver behavior | Event emitted |
|---|---|---|
| `run_loop` returns `LoopError::Provider` | Clear goal | `GoalCleared { BudgetExhausted }` (provider error surfaces via existing path) |
| Tag parse fails | Treat as `TagOutcome::Missing` → iterate | `GoalIteration { last_tag: Missing }` |
| Verifier rate-limited | Fail open (trust main-model claim) | `GoalCleared { VerifierUnavailable }` |
| Verifier transport error (network) | Fail open | `GoalCleared { VerifierUnavailable }` |
| Verifier returns unparseable text | Treat as `NotMet`, resume iteration | `GoalIteration` next tick |
| Verifier says `not_met` | Resume iteration, inject verifier reason | `GoalIteration { last_tag: ... }` next tick |
| Max iter reached | Stop | `GoalCleared { MaxIter }` |
| Token budget exhausted | Stop | `GoalCleared { BudgetExhausted }` |
| User sends `Prompt` mid-iteration | Cancel current iter; user msg replaces synth prompt | (no goal event; normal `Token` events flow) |
| User sends `Interrupt` | Stop | `GoalCleared { UserSlash }` |
| Daemon shutdown mid-iter | State persisted via session checkpoint | On resume: `GoalActive` re-emit, await user |

## §6 Testing strategy

### Unit tests in `origin-goal` (~12 tests, no I/O, deterministic)

- Tag parser: well-formed `met`/`in_progress`/`blocked`, missing tag → `Missing`, malformed → `Missing`, multiple tags → last wins, case-insensitive `state=`, whitespace in attributes, empty `<reason>`.
- State machine transitions: every legal edge in the driver flowchart, plus the rejection-resume edge.
- Flag parser: `--max-iter=N`, `--budget=200k`/`1m`, unknown flag rejected, condition extraction with multiple flags, condition with embedded `--` (treated as condition text).
- Budget arithmetic: cap-on-equality, cap-on-overshoot, verifier tokens included.

### Integration tests in `origin-daemon/tests/` (~6 tests, mock provider + mock verifier)

- `goal_activates_with_inline_args.rs` — `/goal fix tests` round-trips through the parser and emits `GoalActive` with the right condition.
- `goal_iterates_on_in_progress.rs` — mock provider emits `in_progress` tag for 3 turns then `met`; verifier confirms; assert exactly 4 `run_loop` calls and a `GoalCleared { Met }` event.
- `goal_max_iter_caps.rs` — mock provider always emits `in_progress`; assert exactly `max_iter` calls and `GoalCleared { MaxIter }`.
- `goal_budget_caps.rs` — mock provider emits high-token responses; assert clear when cumulative tokens cross budget.
- `goal_user_interrupt_during_iteration.rs` — start iteration, inject `ClientMessage::Interrupt`, assert `GoalCleared { UserSlash }` and no further `run_loop` calls.
- `goal_verifier_rejection_resumes.rs` — mock provider claims `met`; mock verifier returns `not_met — tests still failing`; assert one more iteration with the verifier's reason injected, then final `met`.

### Resume-token round-trip (`origin-resume-token/tests/`)

- `goal_snapshot_round_trip.rs` — `GoalSnapshot` serializes, MAC-wraps, unwraps, deserializes; missing field tolerated; status-Active token resumes as Active.

### Skill catalog test (`origin-skills/tests/embedded_skills.rs`)

- Skill count stays at 16 (we're replacing the existing `goal/SKILL.md` body, not adding a new skill). New assertion: the body contains the `<goal-status>` tag protocol example string.

### CLI parser test (`origin-cli/src/input.rs`)

- `parse_skill_command` returns `ActivateSkill { name, args: Some("...") }` for `/goal fix tests`.
- Existing skill activations without args still return `args: None` (backward compat).

## Out of scope (deferred)

- Multiple concurrent goals per connection.
- Sharing goal state across connections to the same session.
- A `/goal pause` verb (today: user just interrupts; tomorrow: maybe).
- Configurable verifier model via flag (use Haiku for now; reconsider if there's demand).
- Telemetry hooks for goal completion rates / verifier disagreement rates.
