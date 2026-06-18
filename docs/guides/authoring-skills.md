# Authoring Skills

A **skill** is a Markdown file with YAML frontmatter that teaches `origin` a
proven technique, pattern, or reference. Skills are reusable across projects and
sessions. `origin` ships **19 embedded skills** (the "superpowers" set —
test-driven-development, systematic-debugging, writing-plans, and more), and you
can add your own under `~/.origin/skills/`.

Crucially, skills are **not** all loaded into every conversation. They are
indexed into an embedding graph and only the top-matching ones are injected per
turn — so you can install hundreds of skills with no session-start scan cost and
no token bloat.

> For the subsystem internals (the HNSW index, materialization, the loader), see
> the [skills subsystem reference](../subsystems/skills.md). This guide is the
> task-oriented "how do I write one" companion.

---

## The `SKILL.md` format

A skill lives at `~/.origin/skills/<skill-name>/SKILL.md`. The file is split
into a YAML frontmatter block (between `---` delimiters) and a Markdown body:

```markdown
---
name: skill-name
description: Use when [specific triggering conditions and symptoms]
allowed-tools: [Read, Grep, Bash]
---

# Skill Name

## Overview
Core principle in 1–2 sentences.

## When to Use
Bullet list with SYMPTOMS and use cases. When NOT to use.

## ...
The rest of the skill body.
```

### Frontmatter fields

| Field | Required? | Type | Meaning |
| --- | --- | --- | --- |
| `name` | **yes** | string | Skill identifier. Use letters, numbers, and hyphens only. |
| `description` | **yes** | string | Third-person trigger text: describes *when* to use the skill, not what it does. Drives discovery. |
| `allowed-tools` | no | list of strings | The exact set of tools this skill may use. Omitted ⇒ no narrowing. |

The parser accepts both Unix (`\n`) and Windows (`\r\n`) line endings and strips
a leading UTF-8 BOM, so files saved by any editor round-trip cleanly. Missing
the opening or closing `---` delimiter is an error, as is invalid YAML.

> **Frontmatter budget.** Keep the whole frontmatter block under ~1024
> characters, and the `description` under ~500 characters. The frontmatter (not
> the body) is what gets embedded and matched, so it should be dense with
> trigger keywords.

---

## A complete worked example

Here is a small, self-contained technique skill. Save it to
`~/.origin/skills/condition-based-waiting/SKILL.md`:

```markdown
---
name: condition-based-waiting
description: Use when tests have race conditions, timing dependencies, flaky sleeps, or pass/fail inconsistently
allowed-tools: [Read, Grep, Edit]
---

# Condition-Based Waiting

## Overview
Replace fixed `sleep(n)` delays in tests with a poll-until-condition helper.
Fixed sleeps are either too short (flaky) or too long (slow). Wait for the
*condition you actually care about* instead of for the clock.

## When to Use
- A test "passes on my machine" but fails in CI
- You see `setTimeout` / `sleep` / `Thread.sleep` sprinkled through tests
- Tests get slower every time someone "bumps the timeout"

When NOT to use: genuinely time-based behavior (debounce windows, TTL expiry).

## Core Pattern

```ts
// ❌ Before: guess a duration
await sleep(2000);
expect(widget.loaded).toBe(true);

// ✅ After: poll the real condition
await waitFor(() => widget.loaded, { timeout: 5000, interval: 25 });
expect(widget.loaded).toBe(true);
```

## Common Mistakes
- Polling without a timeout (hangs forever on a real bug)
- Polling a mock instead of real state
```

Drop the directory in place and `origin` will index it on the next session.
There is no registration step — the loader scans `~/.origin/skills/` and any
discovered skill folders.

---

## The `allowed-tools` narrowing rule

This is the single most important authoring decision. `allowed-tools` declares
the *exact* set of tools a skill is permitted to use. The narrowing is
**enforced**, not advisory:

> A skill that omits `Bash` from its `allowed-tools` **cannot shell out** — even
> if the model tries. The sandbox (landlock + seccomp + namespaces on Linux,
> `sandbox-exec` on macOS, AppContainer on Windows) is wired to the tool grant.

Practical guidance:

- **Grant the minimum.** A read-only review skill should list only `Read`,
  `Grep`, `Glob`. Don't add `Write`/`Edit`/`Bash` "just in case."
- **Omitting the field means no narrowing** — the skill inherits the session's
  ambient toolset. Prefer an explicit list for anything that touches the
  filesystem or the network.
- **Tool names are canonical** (`Read`, `Grep`, `Glob`, `Edit`, `Write`,
  `Bash`, `WebFetch`, `WebSearch`, `Browser`, …). A name not in the builtin
  Toolbox is most likely an MCP-served tool; the first-run discovery sweep will
  flag any such tool a skill declares.

---

## Embedding-indexed injection

You don't choose which skills load — `origin` does, per turn, by relevance:

1. Each installed skill's frontmatter is embedded and indexed into an HNSW
   (approximate nearest-neighbor) graph at load time.
2. On each turn, `origin` materializes the **top-K** skills whose `description`
   best matches the current context, and injects only those into the prompt.
3. Hundreds of installed skills therefore cost **zero** session-start scan time
   and don't bloat the context window.

This is why the `description` field matters so much: it's the query target.
Write it as **triggering conditions only** ("Use when…"), in the third person,
packed with the words a model would search for (error messages, symptoms,
synonyms, tool names). **Do not** summarize the skill's workflow in the
description — testing showed that a description that summarizes the process
causes the model to follow the *description* and skip the body.

```yaml
# ❌ BAD: summarizes the workflow — the model may follow this and skip the skill
description: Use when executing plans - dispatches a subagent per task with review between tasks

# ✅ GOOD: triggering conditions only, no workflow summary
description: Use when executing implementation plans with independent tasks in the current session
```

---

## Token-budget tips

Frequently-matched skills can load into many conversations, so every token
counts. Target word counts (from the embedded `writing-skills` skill):

| Skill kind | Target |
| --- | --- |
| getting-started / always-on workflows | < 150 words each |
| Frequently-loaded skills | < 200 words total |
| Other skills | < 500 words |

Techniques to stay under budget:

- **Move detail to `--help`.** Reference a command's own help instead of
  documenting every flag.
- **Cross-reference, don't repeat.** Point to another skill by name with a
  requirement marker (`**REQUIRED BACKGROUND:** Use superpowers:test-driven-development`)
  rather than restating it. Never use `@path` links — that force-loads the file
  and burns context immediately.
- **Split heavy reference into sibling files.** Keep `SKILL.md` as the overview;
  put 100+ line API references or reusable scripts in adjacent files that are
  loaded only when needed.
- **One excellent example beats five mediocre ones.** Don't port the same
  example to five languages.

Verify with a quick word count:

```sh
wc -w ~/.origin/skills/your-skill/SKILL.md
```

---

## The writing-skills discipline

`origin`'s own `writing-skills` skill states the rule plainly:

> **Writing skills IS Test-Driven Development applied to process documentation.**

The Iron Law:

```text
NO SKILL WITHOUT A FAILING TEST FIRST
```

The RED → GREEN → REFACTOR cycle maps onto skill authoring:

| TDD concept | Skill creation |
| --- | --- |
| Write test first | Run a baseline pressure scenario **before** writing the skill |
| Watch it fail (RED) | Document the exact rationalizations an agent uses *without* the skill |
| Minimal code (GREEN) | Write a skill addressing *those specific* failures — no speculative content |
| Watch it pass | Re-run the scenario with the skill present; the agent should comply |
| Refactor | Find new rationalizations, add explicit counters, re-test until bulletproof |

The point: if you didn't watch an agent fail without the skill, you don't know
whether the skill teaches the right thing. This applies to **edits** as much as
to new skills.

### Naming and structure conventions

- **Verb-first, active names:** `creating-skills`, `condition-based-waiting`,
  `root-cause-tracing` — not `skill-creation` or `async-test-helpers`.
- **Flat namespace:** one searchable folder per skill under `skills/`.
- **Discipline skills** (rules) need a *rationalization table* and a *red-flags
  list* to close loopholes; **technique/pattern/reference skills** need a clear
  example and a quick-reference table.

---

## Inspecting and discovering skills

```sh
origin plugin ls                 # list discovered .claude / .agents skills
origin plugin info <manifest>    # parse a plugin manifest + report its context cost
origin plugin install <source>   # install a bundle into ~/.origin/plugins/
```

When you migrate from another harness, `origin import` brings skills across with
content-hash dedupe — see **[Migration](migration.md)**.

---

See the [skills subsystem reference](../subsystems/skills.md) for how the index,
materialization, and injection are implemented.

_Last reviewed against workspace version 0.9.8._
