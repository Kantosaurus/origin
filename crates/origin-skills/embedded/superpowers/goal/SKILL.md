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
