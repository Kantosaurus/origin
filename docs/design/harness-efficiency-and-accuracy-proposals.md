# Making `origin` more accurate, more token-efficient, and cheaper

> A design review with concrete, implementable proposals, grounded in the current
> code (workspace 0.9.8) and in the published SWE-bench/agent-harness literature.
> Audience: maintainers. Each proposal lists **what**, **why it wins on
> {accuracy, tokens, cost}**, **where it plugs into the code**, and **effort**.

---

## 0. TL;DR — the ranked shortlist

`origin` is already in the top decile of harness engineering: SchemaCrush
columnar tool-output compaction, content-addressed dedup, hot/deferred tool
schemas, a stable-prefix/volatile-suffix prompt split for prompt-cache
stability, summary-backed compaction, the cheap self-tag goal verifier, and a
two-runtime daemon with perf-as-CI-gate. The biggest remaining wins are *not*
micro-optimizations of those; they are **whole mechanisms that the top
SWE-bench systems have and `origin` does not yet**.

Ranked by **accuracy-per-dollar uplift ÷ implementation effort**:

| # | Proposal | Acc | Tokens | Cost | Effort | Status |
|---|----------|:---:|:------:|:----:|:------:|:------:|
| 1 | **Reproduction-test gate** (generate failing test → confirm RED → require GREEN before "done") | ★★★ | ~ | ↓ (kills wasted retries) | M | ✅ **shipped** (`ORIGIN_REPRO_GATE`) |
| 2 | **Uncertainty-gated model cascade** (cheap model drives; escalate only when stuck) | ★★ | ~ | ↓↓↓ | M | ◑ partial (`choose_model_ref_struggling`; opt-in `ORIGIN_ROUTER`) |
| 3 | **Regression-test selection before final answer** (run the touched-area tests, feed failures back) | ★★★ | ↑ small | ↓ (fewer bad patches) | S–M | ✅ **shipped** (`ORIGIN_TEST_SELECT`) |
| 4 | **Localize-then-edit retrieval band** (repo-map + codegraph "suspect set" injected before the agent reads files) | ★★ | ↓↓ | ↓ | S | ◑ partial (repomap `focus` on by default) |
| 5 | **Best-of-N with execution-grounded selection** (only when a test oracle exists) | ★★★ | ↑↑ | ↑ but bounded | M | ✅ **shipped** (`ORIGIN_BESTOFN=N`) |
| 6 | **Sub-agent return-contract compression** (force structured `CompletionReport`, drop raw transcript) | ★ | ↓↓ | ↓ | S | ☐ |
| 7 | **Adaptive thinking-budget by phase/uncertainty** | ★ | ↓ | ↓ | S | ☐ |
| 8 | **"Anchored" file reads / edit-anchored re-reads** (never re-emit a whole file the model already saw) | ~ | ↓↓ | ↓ | S | ◑ partial (`ORIGIN_AGENTGREP_TRUNCATE` for Grep) |
| 9 | **Semantic + exact prompt-prefix cache across sessions** (warm the cache from a sibling session) | ~ | ↓ | ↓↓ | M | ☐ |
| 10 | **Speculative *next-tool* prefetch** (extend the existing speculative path to chains) | ↑ latency | ~ | ~ | M | ☐ |
| 11 | **Token-budgeted, model-tuned system prompt** (trim the always-on directive; make it conditional) | ~ | ↓ | ↓ | S | ☐ |
| 12 | **Diagnostic-driven repair loop already exists in `postedit` — wire it into the main loop** | ★★ | ↑ small | ↓ | S | ✅ **shipped** (`auto_lint`/`auto_test` + terminal enforcement) |

> **Implementation status (workspace 0.9.13+).** Proposals **1, 3, 5, 12** — the
> execution-feedback stack the TL;DR calls the highest-leverage change — are now
> implemented and wired into the **default headless path** (no `/goal` required).
> See [§9 "What shipped"](#9-what-shipped-implementation-notes) for the exact
> env switches, code seams, and the A/B protocol. Proposals 2, 4, 8 have partial
> substrate already in tree. The rest remain open.

`~` = roughly neutral. The single highest-leverage change is **#1 + #3 + #5
together**: an execution-grounded "propose → reproduce → verify → (rarely)
sample-and-rank" pipeline. That is exactly the trick that puts Agentless and the
test-time-scaling systems on the SWE-bench Pareto frontier, and `origin` has all
the substrate (Bash supervisor, postedit policy, codegraph, swarm) but never
assembles it into a closed loop.

---

## 1. Context from the literature (why these specific levers)

Holding the **model constant** (which is exactly `origin`'s benchmark
discipline), the published spread between harnesses on SWE-bench Verified comes
from a small number of mechanisms:

- **Execution feedback / reproduction tests.** The single largest non-model
  lever. **Agentless** (Xia et al., 2024) — a *fixed* localize → repair →
  validate pipeline with **no agent control loop** — matched or beat the agentic
  systems of its day at a fraction of the cost (low single-digit dollars per
  instance, far fewer LLM calls), precisely because it (a) localizes cheaply with
  structure, (b) samples a handful of candidate patches, and (c) **filters them
  with regression tests and a reproduction test**. The published method figure
  set literally centers on `reproduction_test`. Test-grounded *selection* is what
  converts "diverse guesses" into "a correct patch."
- **Test-time scaling with a selector.** Sampling N rollouts/patches and picking
  the best with an oracle (tests) or a learned value function (e.g.
  **SWE-search / Moatless Tools** running MCTS over trajectories) reliably adds
  several points of solve rate — but multiplies cost, so it must be **gated** to
  hard instances, not applied uniformly.
- **Cheap, structure-aware localization.** **AutoCodeRover** (AST/structure-aware
  navigation + spectrum-based fault localization) and Agentless both show that
  *narrowing the edit region before generation* both raises accuracy and slashes
  tokens (you stop dumping whole files into context). `origin` already has the
  two ingredients — `origin-repomap` (PageRank) and `origin-codegraph` — but
  doesn't inject a "suspect set" up front.
- **Minimal ACI beats baroque tooling.** **SWE-agent**'s thesis (the
  agent-computer interface matters more than raw model power) and the existence
  of **mini-swe-agent** (~100 lines, bash-only, still a strong fraction of the
  full system's score) argue that *every* token in the system prompt and tool
  schema must earn its place. `origin`'s hot/deferred split already embraces
  this; the always-on `DEFAULT_WORKFLOW` directive (~600–800 tokens, in the
  cached prefix every turn) is the one place that violates it.
- **Model cascades.** Routing the *easy* turns to a cheap model and escalating
  only on difficulty is the canonical cost-killer. `origin`'s router supports
  `PhaseAware`/`ArchitectEditor`/`QuotaFallback`/`Scored`, but the live policy is
  *phase-based* (turn 1 = plan, later = edit), not *uncertainty-based*.

Everything below maps one of these levers onto a concrete seam in the code.

---

## 2. Accuracy proposals

### 2.1 Reproduction-test gate (★ highest leverage)

**What.** Add an optional, opt-in pipeline stage: from a task/issue description,
the agent first writes a *failing* test that reproduces the bug, **runs it to
confirm RED**, does the fix, and the loop refuses to terminate `Ok` until that
same test goes **GREEN** and the pre-existing tests still pass.

**Why it wins.**
- *Accuracy:* turns "the model thinks it's done" into "the machine proved it's
  done." This is the mechanism behind Agentless's patch validation and is the
  largest single non-model contributor to SWE-bench solve rate.
- *Cost/tokens:* counter-intuitively **cheaper on net**, because today a wrong
  "success" wastes an entire downstream turn budget (and, under `/goal`, the
  Haiku verifier round-trips). A test oracle ends the loop deterministically.

**Where it plugs in.**
- The discipline already lives in the *prompt* (`default_workflow.rs` TDD step,
  `verification-before-completion` skill) but nothing **enforces** it. Add a
  programmatic gate in the terminal branch of `run_loop_inner`
  (`crates/origin-daemon/src/agent.rs:3827`, the "no tool_use ⇒ return
  `Ok(LoopSummary)`" site). Before returning, if a `ReproSpec` is configured for
  the session, run the recorded test command via the existing `proc_supervisor`
  (`origin-tools`/`bash.rs`) and only return `Ok` on GREEN; otherwise synthesize
  a continuation prompt with the failing output (mirroring
  `origin-postedit::repair_decision`, which already encodes
  `Stop`/`Retry{iter}`/`GiveUp` with `max_repair_iters`).
- Reuse `origin-postedit` (`crates/origin-postedit/src/lib.rs`) wholesale — it is
  *pure decision logic* that already models "run test → on failure feed
  diagnostics back up to N times." Today it is only wired for post-*edit*
  formatting; promote it to a per-prompt completion gate.

**Effort:** Medium. The executor, the repair-decision logic, and the prompt
discipline all exist; you are wiring them into the terminal branch and threading
a `test_command`/`repro` config through `LoopOptions`.

**Risk control.** Default-off (env or per-session flag) so non-test workflows
(docs, refactors with no runnable oracle) are byte-identical. Cap repair
iterations with the existing `max_repair_iters`.

---

### 2.2 Regression-test selection before final answer

**What.** When the turn mutated files, *before* declaring success, run a
**narrow, selected** set of tests covering the touched files (not the whole
suite), and feed any failures back into the loop.

**Why it wins.**
- *Accuracy:* catches "fixed the symptom, broke a neighbor" — the
  `PASS_TO_PASS` half of SWE-bench scoring. Pure upside on correctness.
- *Cost:* selection (vs full suite) keeps the extra spend small; and again, a
  caught regression is far cheaper than shipping a wrong patch.

**Where it plugs in.**
- Selection set = the files the turn mutated (already tracked as
  `lsp_edited_paths` in `run_loop_inner`, `agent.rs:3407`) ∪ their reverse
  dependencies from `origin-codegraph` (`graph_query` `Neighbors`/`Path` on the
  edited entities). This is a *natural* use of the code graph that nothing
  currently exercises in the hot loop.
- Run via `proc_supervisor`; format failures through the same repair-decision
  path as 2.1.

**Effort:** Small–Medium (largely subsumed by 2.1; the new part is the
codegraph-driven *selection* of which tests to run).

---

### 2.3 Best-of-N with execution-grounded selection (gated)

**What.** For instances where a test oracle exists *and* a difficulty signal
fires, generate **N candidate patches** (via N swarm sub-agents, which already
run concurrently and in isolated context), run each against the
reproduction+regression tests, and keep the first that passes (or rank by
tests-passed, tie-broken by a cheap LLM-as-judge / `origin-review` confidence
score).

**Why it wins.**
- *Accuracy:* test-time scaling is the most reliable way to add points once the
  single-shot pipeline is solid. With an execution oracle the selector is
  *free of judge error* — you keep a patch only if it actually passes.
- *Cost:* this is the one proposal that *raises* cost, so it is **strictly
  gated** (see §3.1 difficulty signal) and bounded (small N, e.g. 3). Net
  accuracy-per-dollar is positive because it is applied only to the ~20–30% of
  instances that single-shot fails.

**Where it plugs in.**
- The swarm is purpose-built for this: `Task` → `origin_swarm::Coordinator` →
  `swarm_worker::run_one_worker` already spins **isolated `Session`s** with their
  own context (`crates/origin-daemon/src/swarm_worker.rs`). Fan out N workers
  with the same goal + repro spec; collect via the existing background-results
  machinery.
- Selection/ranking: `origin-review` (`crates/origin-review/src/lib.rs`) already
  implements confidence-scored, adversarial-vote aggregation of findings — reuse
  its `vote`/`filter`/`Strictness` machinery as the tie-breaker when multiple
  candidates pass.
- Each candidate should land on an **isolated git worktree** so a bad candidate
  never touches the user's tree — `origin-vcs::Worktree` ("lanes") already exists
  for exactly this; the parent merges only the winning diff.

**Effort:** Medium. Orchestration + worktree-per-candidate + selection wiring;
all sub-components exist.

---

### 2.4 Localize-then-edit retrieval band

**What.** Before the agent starts reading files, inject a compact, token-budgeted
**"suspect set"**: the top-K files/symbols most likely relevant, from
`origin-repomap` (personalized PageRank, `focus`ed on terms in the prompt) plus
a `origin-codegraph` neighborhood of the matched symbols.

**Why it wins.**
- *Accuracy:* localization-first is the Agentless/AutoCodeRover insight — the
  model edits the right place more often and wastes fewer turns flailing with
  `Grep`.
- *Tokens:* **strongly negative token delta** — the model reads *targeted* files
  instead of exploring, and you replace a dozen speculative `Read`/`Grep` calls
  with one budgeted map.

**Where it plugs in.**
- `repo_map_block` already exists (`agent.rs:3227`, gated by `ORIGIN_REPOMAP=1`)
  and `origin-repomap::build_map(files, focus, token_budget)` already accepts a
  `focus` set. Today `focus` is empty. Populate it from (a) salient identifiers
  in the user prompt and (b) `mem_search`/`graph_query` hits, and render it into
  the **stable cached prefix** (it's already in the `recalled_system` parts
  array, so it stays cache-stable across the run).
- Add a `<suspect-set>` companion block from a one-shot `graph_query`
  `Neighbors` around the focus symbols.

**Effort:** Small. The map, the focus parameter, the codegraph queries, and the
prompt-assembly slot all exist; you wire the focus extraction and flip it on.

---

### 2.5 Wire the existing post-edit repair loop into the main loop

**What.** `origin-postedit` is fully implemented (formatter table, lint/test
config, `repair_decision` with `max_repair_iters`) but, per the tools doc, the
*caller* must execute the chosen commands — and the **main agent loop doesn't**.
Wire `auto_lint`/`auto_test` into the per-turn post-edit probe.

**Why it wins.** Accuracy (compile/lint/test errors get fixed before "done"
instead of surfacing to the user) at a small, bounded token cost. It's the
generalization of 2.1/2.2 to lint+format, and the code is *already written and
tested* — it's just not invoked from `run_loop_inner`.

**Where it plugs in.** The post-edit site already exists in the loop ("post-edit
LSP probe" after tool dispatch, `agent.rs:~4713`). Extend it from "LSP
diagnostics only" to "formatter → optional lint → optional test → repair
decision," all default-off.

**Effort:** Small.

---

## 3. Cost proposals

### 3.1 Uncertainty-gated model cascade (★ highest cost lever)

**What.** Drive the loop with a **cheap** model by default and **escalate to a
strong model only when a difficulty/uncertainty signal fires** — instead of the
current *phase-based* split (turn 1 = plan/strong, later = edit/cheap).

Difficulty signals (all cheaply computable in-loop):
- repeated tool failure / `edit.no_match` / `edit.ambiguous` on the same file;
- the reproduction test still RED after K cheap-model repair iterations;
- the model emitted `<goal-status state="blocked">` or kept emitting `Missing`
  tags (already parsed by `origin-goal`);
- transcript crossed a size/turn threshold without progress (no new files
  touched in M turns).

**Why it wins.**
- *Cost:* this is the canonical cascade saving — most turns are easy and a cheap
  model nails them; you pay for the frontier model only on the hard residual.
  On a SWE-bench-style mix this is a large $ reduction at near-flat solve rate.
- *Accuracy:* *non-negative* — hard instances still get the strong model, and
  the escalation trigger ("cheap model is stuck") is exactly when the strong
  model helps most.

**Where it plugs in.**
- The router already exists and is per-turn: `LiveRouter::choose_model_ref(turn)`
  (`routing.rs:88`) drives `turn_model` in `run_loop_inner` (the per-turn routing
  block at `agent.rs:~3408`). Add a `Strategy::EscalateOnUncertainty { cheap,
  strong }` to `origin-router`, and feed it the difficulty signal via a new
  `Phase`-like input or a side-channel the loop already computes
  (`total_tool_calls`, repeated-`ToolError`, goal tag, `lsp_edited_paths`
  emptiness).
- Cost accounting to *prove* the win is already there: `estimate_spend_usd`
  (`agent.rs:660`) + `origin-cost`'s cache-aware pricing.

**Effort:** Medium. New strategy enum arm + threading the (already-computed)
signals into the router call. No provider changes — cross-provider per-turn
rebuild (`build_provider_for`) already exists.

---

### 3.2 Adaptive thinking-budget by phase & uncertainty

**What.** Scale `ChatRequest.thinking_tokens` / `ReasoningEffort` down for
mechanical turns (applying a known edit, running a command) and up only for
planning/stuck turns.

**Why it wins.** Extended-thinking tokens are billed output tokens. Most turns
in a coding loop are mechanical and need little. Spending the thinking budget
only where it changes the answer is a direct token+$ cut at flat accuracy.

**Where it plugs in.** `effort`/`thinking_tokens` are already first-class on
`ChatRequest` and already wired per-model in the Anthropic driver (adaptive vs
fixed budget). Set them per-turn from the same phase/uncertainty signal as 3.1
(plan/stuck ⇒ high; edit/execute ⇒ low). The Anthropic driver already does the
right thing with `None`/low values (byte-identical when unset).

**Effort:** Small.

---

### 3.3 Sub-agent return-contract compression

**What.** Sub-agents currently return `CompletionReport` with `plan_updates`,
`files_touched`, `decisions`, `follow_ups`, and `transcript_handle` **all
empty**, plus the full final `assistant_text` and token counts
(`swarm_worker.rs:376`). Force workers to emit a *structured, bounded* report
(files touched, the diff, a 1–2 line decision summary, and a CAS `transcript_handle`
for the full body) and have the parent consume the **structured fields**, not the
raw prose.

**Why it wins.**
- *Tokens:* sub-agent context isolation is one of the best-known token wins
  (Cognition's "don't dump sub-agent transcripts into the parent" point). Today
  the parent re-ingests the worker's free-form final answer; a structured report
  is a fraction of the tokens.
- *Cost/accuracy:* a tighter contract also reduces the parent's confusion and
  keeps its own cache prefix stable.

**Where it plugs in.** Populate the already-defined `CompletionReport` fields in
`run_one_worker`; CAS-put the full transcript and return only its handle
(`transcript_handle` is already a `[u8;32]` field — it's just hard-coded to
zero). Parent-side, format the structured fields into the `<background-results>`
block instead of the raw text.

**Effort:** Small.

---

### 3.4 Cross-session prompt-prefix cache warming

**What.** When a swarm of sibling workers (or sequential sessions on the same
repo) share an identical system+tools prefix, ensure they hit the *provider's*
prompt cache by issuing them so the first request warms it and the rest land
inside the 5-minute TTL — and surface a "cache is about to go cold" nudge to
re-warm proactively.

**Why it wins.** Pure cost: Anthropic cache-read is ~0.1× input. `origin` already
detects cold cache (`origin-cost::is_cache_cold`, `PROMPT_CACHE_TTL_MS`) and
already keeps a byte-stable prefix (`recalled_system`). The missing piece is
*scheduling* sibling work to share the warm window and re-warming before expiry.

**Where it plugs in.** Coordinator dispatch order (`origin-swarm`) + the existing
cache-cold signal in `origin-cost`. The prefix is already stable by construction
(volatile context rides as a trailing block), so this is purely about timing.

**Effort:** Medium.

---

## 4. Token-efficiency proposals

### 4.1 Trim / conditionalize the always-on `DEFAULT_WORKFLOW` directive

**What.** `default_workflow::DEFAULT_WORKFLOW` is a ~600–800-token prose block
injected into the **cached prefix of every prompt** (`agent.rs:3169`,
`directive_block`). Make it (a) shorter and (b) **conditional** — full version
only when the request is non-trivial / a `/goal` is active; a one-line version
otherwise.

**Why it wins.**
- *Tokens/cost:* it's in the cached prefix, so the *steady-state* cost is small
  (cache-read), but the **cache-write** (first turn, and every time the cache
  goes cold within the 5-min TTL) pays full freight for those tokens on every
  session and every cold-cache turn. For short/trivial requests it's pure
  overhead the harness's own philosophy (mini-swe-agent minimalism) argues
  against.
- *Accuracy:* a tighter, less prescriptive directive can *help* strong models
  that already know how to work and were being over-constrained.

**Where it plugs in.** `default_workflow::directive()` already has an env switch;
add a *length tier* keyed on a triviality heuristic (the same "trivial request"
notion the directive itself describes) computed once in `run_loop_inner`.

**Effort:** Small. **Caveat:** measure on the SWE-bench A/B harness before/after
— the directive encodes the brainstorm→plan→TDD→verify flow that may itself be
carrying solve rate. Trim empirically, don't guess.

---

### 4.2 Anchored file reads — never re-emit bytes the model already saw

**What.** Track `(path, byte-range, content-hash)` of every region the model has
already read this run; on a subsequent `Read`/`Grep`/post-edit re-display of the
same unchanged region, return a compact "unchanged since turn N — see handle"
reference instead of the bytes.

**Why it wins.** *Tokens:* re-reading the same file (very common after an edit)
is a major silent sink. This generalizes the existing per-session output-CAS
dedup (which only fires on **byte-identical whole results**) to **sub-file
regions and post-edit re-reads**.

**Where it plugs in.** There is already scaffolding: the
`ORIGIN_AGENTGREP_TRUNCATE` path keeps `grep_exposure` windows of already-seen
`(file,line)` regions and elides them on re-grep (`agent.rs:3352`). Generalize
that idea to `Read` and to post-edit file re-display, backed by the global CAS
`Store` + `Recall` (so the full bytes are always retrievable). The
`origin-tools` envelope + `result_cas` already provide the hash/handle
substrate.

**Effort:** Small–Medium. Mostly extending an existing exposure-tracking pattern
to more tools.

---

### 4.3 Push SchemaCrush thresholds and add a "diff" representation for edits

**What.** Two small wins on the already-excellent SchemaCrush:
1. The lossy tail-offload `budget_tokens` (default 6000) and `head_rows` (12) are
   conservative; expose them per-tool via `ToolMeta.token_budget` (already a
   field!) so chatty tools (`Grep` content mode, big `graph_query` result sets)
   crush harder.
2. For file mutations, prefer returning a **unified diff** of what changed rather
   than echoing post-edit file content — the `origin-editfmt` machinery already
   produces/normalizes diffs.

**Why it wins.** Tokens/cost, fully reversible (Recall), no accuracy cost.

**Where it plugs in.** `array_crush::CrushConfig` is per-call already; thread
`ToolMeta.token_budget` into the crush config at the dispatch site. The diff
representation rides on `origin-editfmt`.

**Effort:** Small.

---

### 4.4 Compaction: summarize *semantically*, not just oldest-N

**What.** Today compaction folds the **oldest** summarizable turns into
`[compacted turn N] <summary>` (`compactor.rs`, `COMPACT_OLDEST_N_TURNS=4`). Add
a relevance signal so it preferentially compacts turns **least related to the
current objective** (e.g. exploration dead-ends) and preserves the turns that
touched files still in play.

**Why it wins.** *Accuracy + tokens:* keeps the *useful* history at the same byte
budget; dropping a dead-end exploration is lossless for the task but frees tokens
the oldest-N rule would have spent preserving recent-but-irrelevant chatter.

**Where it plugs in.** `compact()` already takes a `summaries` array and closes
selection under tool-pairing; add an optional per-turn relevance score (cosine of
the turn summary's embedding vs the current goal/prompt embedding — `origin-mem`
already has the ONNX embedder + cosine machinery) and select lowest-relevance
first, still respecting tool-pair closure and the rewind snapshots.

**Effort:** Medium. Keep it behind a flag; the current deterministic behavior is
a safe default and the regression tests
(`compaction_never_orphans_a_tool_result_*`) must keep passing.

---

## 5. Speculation & latency (cost-neutral, UX/throughput wins)

### 5.1 Extend speculative execution from single tools to short chains

`origin` already speculatively dispatches **pure** tools (`Read`/`Glob`/`Grep`)
the instant their args parse from the stream (`try_speculative_spawn`,
`agent.rs:7328`). Two extensions:
- **Pre-warm the LSP/codegraph** for a file the moment a speculative `Read` of it
  fires (so a follow-up `Diagnostics`/`graph_query` is hot).
- **Speculative localization:** on the first user prompt, kick off the
  repo-map/`graph_query` "suspect set" (proposal 2.4) concurrently with the first
  model call, so it's ready to inject without adding wall-clock.

These don't change token/$ but improve the perf KPIs the project already gates
on, and they make 2.4 free in latency terms.

**Effort:** Medium.

---

## 6. What to measure (so the claims are provable)

`origin` already has the right harness: `bench/swe/` runs SWE-bench Verified A/B
with the **model held constant** and reports **pass@1 (95% bootstrap CI), mean
tokens, $, wall-clock** (`bench/README.md`). Recommended protocol for each
proposal:

1. Land it **default-off** (the project's standard "byte-identical when
   disabled" discipline).
2. Run `bench/swe/run.py -n 50 --seeds 3` with the feature off vs on, **same
   model**, same task slice.
3. Accept only if it moves the intended axis without regressing the others:
   - cost/token proposals (3.x, 4.x): tokens/$ ↓ with pass@1 CI overlapping (no
     accuracy loss);
   - accuracy proposals (2.x): pass@1 CI separates upward;
   - and report the **accuracy-per-dollar** Pareto point, which is `origin`'s
     stated competitive claim.
4. Keep the perf-gate green (`bench/perf`, ≤80 ms read-only) — none of these
   should touch the cold-start path.

---

## 7. Suggested sequencing

1. **Wire the existing repair loop into the main loop (2.5)** and
   **sub-agent report compression (3.3)** — both are "code already exists, just
   not invoked," lowest risk, immediate token/accuracy wins.
2. **Reproduction-test gate (2.1) + regression-test selection (2.2)** — the core
   execution-feedback loop; the biggest accuracy lever.
3. **Uncertainty-gated cascade (3.1) + adaptive thinking budget (3.2)** — the
   biggest cost lever; reuses the router/cost crates.
4. **Localize-then-edit band (2.4)** + **anchored reads (4.2)** — the biggest
   token lever; reuses repomap/codegraph/CAS.
5. **Best-of-N execution-grounded selection (2.3)** — apply *after* 2.1/3.1 exist
   (it depends on the test oracle and the difficulty gate), gated to hard
   instances.
6. Polish: 3.4 cache warming, 4.1 directive trim (measure!), 4.3/4.4 compaction
   refinements, 5.x speculation.

---

## 8. One-paragraph rationale for the headline claim

`origin`'s honest, provable edge is **"same-or-better solve rate at fewer
tokens / lower $ / lower latency."** The proposals above are chosen to widen
exactly that gap: the **execution-feedback** stack (2.1–2.3, 2.5) raises solve
rate the way the SWE-bench Pareto leaders do; the **cascade + adaptive
thinking** stack (3.1–3.2) and **localization + anchored reads** stack (2.4,
4.x) cut tokens and dollars *without* touching solve rate; and every one of them
reuses substrate `origin` already ships (proc-supervisor, postedit, swarm,
worktrees, router, cost meter, repomap, codegraph, CAS, the embedder). The work
is mostly **assembly and wiring**, not new subsystems — which is the cheapest
possible way to move all three KPIs at once.

---

## 9. What shipped (implementation notes)

The execution-feedback stack (proposals **1, 3, 5, 12**) is implemented and
default-off. Every switch below is byte-identical to the prior behaviour when
unset. The design constraint that drove this work: these mechanisms already
existed at the prompt/decision layer but were **dormant on the benchmarked
path** — `bench/swe/adapters/origin.sh` runs a plain `origin run <issue>` with
no `/goal`, no `governance.toml`, and no test command, so the gate never fired
and `LoopSummary.gate_signals` was computed and dropped. The fix makes the gate
**armable by env and enforced without a `/goal`**.

### 9.1 Env switches (operator surface)

| Env | Effect | Default |
|-----|--------|---------|
| `ORIGIN_REPRO_GATE=1` | Arm the reproduction/regression gate on the current run: inject the `<repro-gate>` contract, run the test command at turn-end, and **refuse to return a tool-free "done" while tests are RED** — even with no `/goal`. Auto-derives a `test_command` from the repo's ecosystem markers. | off |
| `ORIGIN_REPRO_GATE=<command>` | As above, but use `<command>` verbatim as the test oracle (escape hatch for non-standard runners). | — |
| `ORIGIN_TEST_SELECT=1` | Regression-test **selection**: narrow the gate's test command to `{edited ∪ code-graph reverse-deps}` (pytest/go) instead of the whole suite. Falls back to the full command whenever narrowing could drop coverage. | off |
| `ORIGIN_BESTOFN=N` (N≥2, capped at 6) | For a **hard** instance (gate still RED), run N candidate attempts in isolated `git worktree` lanes, score each against the test oracle, and apply the verified winner's diff. Requires an armed oracle (`ORIGIN_REPRO_GATE`). | off |

`[post_edit]` in `governance.toml` also gained a `repro_gate` key (previously the
field existed on `PostEditConfig` but had **no config surface** — it was
unreachable).

### 9.2 Code seams (for maintainers)

- **Gate on-switch + auto-derived command:** `config::resolve_post_edit_with_repro_gate`
  + `config::probe_repo_markers` (daemon) over the pure
  `origin_postedit::{detect_test_command, RepoMarkers}`. Wired at the
  `LoopOptions.post_edit` build site in `main.rs`.
- **No-goal enforcement (the key change):** the terminal branch of
  `run_loop_inner` (`agent.rs`, the "no tool_use ⇒ return `Ok`" site) now runs
  the oracle *there* — fixing the ordering gap where the post-turn test block
  never executed on the final tool-free turn and the terminal branch reused a
  *stale* prior-turn result — and on RED pushes a continuation user turn and
  `continue`s instead of returning, bounded by `PostEditConfig::max_repair_iters`.
  The predicate is `origin_postedit::PostEditConfig::test_gate_armed`.
- **Test selection:** `origin_codegraph::query::reverse_dep_files` (new inbound
  `edges_to` / `nodes_in_file` primitives) → `origin_postedit::select_tests` →
  `agent::effective_test_command`, called at every gate execution site.
- **Sub-agent enforcement:** `swarm_worker::worker_post_edit` arms `auto_test`
  (turn-end regression run + terminal enforcement) — but **not** `repro_gate`
  (the "write a failing test first" contract is a top-level bug-report
  discipline, not a delegated sub-task's).
- **Best-of-N:** pure policy in `origin_swarm::bestofn`
  (`select_best`/`has_verified_winner`/`DifficultySignals`, fully unit-tested);
  orchestration in `origin_daemon::bestofn_runner` (`WorktreeArena` +
  `run_best_of_n`), invoked from `main::maybe_run_best_of_n`. The batch runs on a
  blocking thread and serializes on a process-wide cwd lock (candidates edit
  cwd-relative paths, so each runs with cwd pointed at its worktree).

### 9.3 A/B protocol (unchanged from §6, restated per feature)

Each feature is default-off; measure it on `bench/swe/` with the **model held
constant** before turning it on in the published run:

```bash
# baseline (all off) vs. the execution-feedback stack:
python bench/swe/run.py --harness origin --adapter bench/swe/adapters/origin.sh -n 50 --seeds 3
ORIGIN_REPRO_GATE=1 python bench/swe/run.py --harness origin-gate --adapter bench/swe/adapters/origin.sh -n 50 --seeds 3
ORIGIN_REPRO_GATE=1 ORIGIN_TEST_SELECT=1 ORIGIN_BESTOFN=3 \
  python bench/swe/run.py --harness origin-full --adapter bench/swe/adapters/origin.sh -n 50 --seeds 3
python bench/swe/evaluate.py --out bench/swe/out --max-workers 8
```

Accept a knob only if it moves pass@1's CI upward (accuracy features) or holds
pass@1 while cutting tokens/$ (efficiency features). To make the *published*
number reflect the gate, export the switches in the adapter (or a wrapper) — note
that **SWE-bench hides the `FAIL_TO_PASS`/`PASS_TO_PASS` tests**, so the
auto-derived whole-suite command (e.g. `pytest`) is what the gate runs; it
verifies the model's *own* reproduction test and any repo tests it can see, not
the hidden grader.
