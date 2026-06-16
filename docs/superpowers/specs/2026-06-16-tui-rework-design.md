# origin TUI rework — design spec

Date: 2026-06-16
Status: approved (brainstorming), pending implementation plan

## Goal

Make origin's terminal UI *feel* like a premium agentic coding terminal. The
rendering engine is already excellent (bespoke cell-grid + SIMD damage-diff +
truecolor + dirty-only repaint); the **design** is not keeping up. This is a
design rework and a set of new interactions on top of the existing engine — not
a rewrite of the renderer.

Direction (chosen): **Signature spine + chrome**. Scope: **comprehensive**.
Identity: **keep + elevate "Burnished Copper"**.

## Design principles (gpt-taste, adapted to the terminal)

The terminal is the medium, so the web-specific gpt-taste literals (GSAP, bento,
hero, picsum) do not apply; its *spirit* does:

- **Break the generic default.** No stock boxed look, no dated ASCII-art banner.
- **Strong hierarchy, ruthless restraint.** One copper accent + gold headings;
  weight/dim/color carry hierarchy. No cheap meta-labels.
- **Spacing & rhythm.** Turns breathe; sections read as distinct chapters.
- **Composition.** A persistent frame (chrome strip, spine, framed composer)
  rather than an undifferentiated scroll.
- **Motion.** Tasteful streaming reveal, micro-spinners, completion ticks — the
  terminal equivalent of motion.
- **Contrast & legibility.** Fix the near-invisible dim and the clashing colors;
  every token earns its contrast.

## Non-goals / constraints

- Keep the bespoke renderer and its performance (SIMD diff, dirty-only,
  ~166fps). No regressions to frame cost; the per-frame full re-wrap of
  scrollback may be optimized but is out of scope unless it blocks the redesign.
- No heavy dependencies. Syntax highlighting is a small **built-in lexical tint**
  (keyword/string/comment/number for rust/js/ts/py/json/bash/go), not syntect.
- Keep working: NO_COLOR (structure via attrs), the HighContrast theme + all
  `/theme` variants, wide-glyph/unicode correctness, the vim layer, prompt
  history + Ctrl+R, existing slash/mention semantics, mouse select/scroll/OSC52.
- Preserve all current daemon/protocol behavior; new protocol variants are
  additive and backward-compatible (a client/daemon that doesn't speak the new
  ChoiceAsk falls back to the existing text/permission path).

## Architecture / decomposition

`origin-cli/src/tui.rs::App::draw` is a ~1260-line monolith and the single
biggest maintainability risk. Decompose the *render* layer into focused,
independently-testable units (pure `(state, &mut Grid/Composer) -> ()` painters
where possible). Proposed modules (under `origin-cli/src/tui/` or new files):

- `theme` / `tokens` — the single source of truth for colors, and a single glyph
  set. Kills the duplicated raw hex in `panel.rs` / `goal_render.rs` so `/theme`
  governs everything. All other modules read tokens from here.
- `chrome` — top context strip + bottom status zone.
- `transcript` — the spine gutter + per-turn role headers + turn spacing.
- `toolblock` — the contained tool-call affordance (header + body + result/diff +
  collapse).
- `markdown` (+ `codeblock`, `syntax`) — block/inline markdown and the code-block
  renderer with the lexical tint.
- `composer` (+ `palette`) — the framed input field, slash palette (described),
  `@` picker, hint line, soft-wrap cues.
- `picker` — the interactive choice component (single + multi select), reused by
  both `ask_user` questions and permission asks.

Each unit answers: what does it render, what state does it read, what does it
depend on. Files that grow past ~400 lines are a smell to split further.

## Visual system (tokens)

Centralize in `theme`/`tokens`:

- Surfaces: `bg` near-black warm `0x0F0D0B`, `raised` `0x1A1714`, plus a new
  faint `band` tint for code blocks / selected rows that is actually distinct.
- Accent: copper `0xD4884E`, `accent_dim` retuned to remain legible (the current
  `DIM 0x44403C` at ~1.3:1 on raised is too low — target ~4.5:1 for any text).
- Text ramp: `muted` → `body` `0xC8C1B8` → `bright` `0xF0EBE3`.
- Headings: gold ramp (h1→h3), unchanged in spirit.
- Roles/semantics: `you` retoned off the clashing cornflower-blue into a warm
  cream/amber; `tool` purple kept but used consistently; ok/warn/error green/
  yellow/red kept; per-tool accent colors derived here.
- **One glyph family.** Retire the five competing status sets. Define:
  - roles: `◆ origin`, `you`
  - spine: `┃`
  - tools: `✎ edit · ⌘ bash · ⌕ grep · ◇ read · ⇲ write · ⚿ web · ⊕ task`
  - status: `▸ running → ✔ ok / ✘ fail`, `⚠ permission`
  - diff gutter: `+ −`, line numbers for reads
  - picker: `▸` cursor, `□/■` (multi toggles), `›` prompt glyph

## Layout / regions

```
 ◆ origin   <model> · <cwd> · ⎇ <branch>                  ◷ <elapsed> · ⛁ <ctx%>
────────────────────────────────────────────────────────────────────────────
┃  ... transcript (spine in gutter) ...
────────────────────────────────────────────────────────────────────────────
 <bottom status: spinner/phase/tokens/cost — its own quiet zone>
╭──────────────────────────────────────────────────────────────────────────╮
│ › <composer>                                                               │
╰──────────────────────────────────────────────────────────────────────────╯
  ⏎ send   ⇧⏎ newline   / skills   @ files   ^c interrupt
```

- **Top strip** is always visible (replaces the scroll-away banner): wordmark +
  model + cwd + branch (left), session clock + context-fill (right).
- **Spine** `┃` (copper) runs the transcript gutter.
- **Bottom status** moves out of the composer into its own zone above a rule.
- **Composer** is a real rounded frame with a `›` prompt glyph.

## Roles & turns

Each turn is headed on the spine: `you` (warm) and `◆ origin` (copper). Generous
inter-turn spacing. The assistant finally has a clear affordance (fixes the
"undifferentiated wall"). Continuation lines hang-indent under the header.

## Tool-call blocks

Each call is a contained, scannable unit tied to the spine with a `┌ │`
connector: per-tool icon + color, target, right-aligned metrics (`+N −N`,
elapsed, `✔/✘`), the result/diff nested beneath, and **collapse by line count**
(not bytes) for long output with an expand affordance. Reads show line numbers;
edits show a real `+ −` diff gutter with hunk awareness.

## Markdown & code

Real rendering: ordered/unordered lists (bullet + indent normalization),
*italic* / `code` / ~~strike~~ / links, a `▎` blockquote bar, full-width
thematic rules, ATX headings 1–4. **Code blocks**: language label, left rule,
legible contrast (no dim-on-dim), the dep-free lexical tint, and the literal
` ``` ` fences replaced by clean framing. Block styling is applied **live while
streaming** (incremental parse) so headings/code no longer "snap" into style at
turn end. A streaming caret `▌` rides the live token.

## Composer & palettes

- Framed field, `›` prompt glyph, coding-flavored placeholder.
- **Slash palette shows descriptions** (already computed, currently discarded).
- **`@` file/agent picker**: a popup that completes files/paths from the repo
  and known agents, with live feedback as you type `@`.
- Soft-wrap gutter cue; "more above" indicator when the field scrolls past its
  max rows; evenly-spaced, de-noised hint line.
- `Esc` dismisses an open popup/picker *before* it ever quits.

## Interactive prompts (new capability)

One reusable `picker` component, two sources:

### A) Agent asks the user (`ask_user` tool)

- New builtin tool `ask_user` with input
  `{ question: string, options: [{ label, description? }], multi_select?: bool,
     allow_custom?: bool (default true) }`. Mirrors how an agent asks a human a
  structured question.
- New protocol: `StreamEvent::ChoiceAsk { id, question, options, multi_select }`
  and `ClientMessage::ChoiceDecision { id, selected: Vec<usize>, custom: Option<String> }`.
- Daemon: the `ask_user` dispatch **pauses the tool** awaiting `ChoiceDecision`
  (exactly the pattern `permission_ask` already uses), then returns the chosen
  label(s) / custom text as the tool result to the model.
- Backward-compat: if a turn/client did not opt into interactive choice, the
  tool degrades to returning a text instruction (the model asks in prose).

### B) Permission asks become selectable

- Reuse `PermissionAsk` / `PermissionDecision`; extend the decision to carry
  `always: bool` (Allow once / Deny / Always-allow `<tool>`). Renders through the
  same `picker` instead of the cramped yellow `y/n` line.

### Picker behavior (TUI)

- Single-select: `↑↓`/`j`/`k` move, `⏎` selects, number keys jump.
- Multi-select: `↑↓` move, `space` toggles `□/■`, `⏎` confirms the set.
- Always offers `✎ type your own…` (when `allow_custom`) → drops into free-text.
- `esc` skips/denies (sends an empty/deny decision; the model handles "skipped").
- The picker renders inline in the transcript, tied to the spine where the
  question was asked; on resolve it collapses to a compact "you chose: …" line.

## Motion

- Streaming caret `▌` on the live token; smooth incremental reveal (no snap).
- Per-running-tool micro-spinner on its block (not just one global spinner).
- One-frame completion tick `▸→✔`.
- Eased composer grow/shrink. All motion respects the dirty-only scheduler (no
  idle redraws beyond the existing watchdog cadence).

## Identity / wordmark

Retire the ASCII block-art. First-run shows a compact one-line wordmark
(`◆ origin`) + a short tip; thereafter the persistent top strip carries identity.

## Testing

- Pure render/layout helpers (wrapping, markdown→spans, diff-gutter, picker state
  reducer, tool-block layout) are unit-tested as pure functions over `state`.
- The `ask_user` wire gets a daemon round-trip test (tool → ChoiceAsk →
  ChoiceDecision → tool result) and a backward-compat (no-opt-in) test.
- Snapshot-style tests render representative frames to a `Grid` and assert key
  rows/glyphs, mirroring existing tui tests.
- Per-crate `build` + `clippy --all-targets -D warnings` + `test`, then run the
  app to visually confirm (see /run).

## Implementation phases (high level; detailed steps come from the plan)

1. **Tokens + glyph unification** (theme/tokens module; remove duplicated hex;
   contrast fixes). Low-risk, unblocks everything.
2. **Chrome** (top strip + bottom status zone) + retire banner/wordmark.
3. **Transcript spine + role headers + turn rhythm.**
4. **Tool-call blocks** (the biggest visible win) + diff gutter + collapse.
5. **Markdown depth + code blocks + lexical tint + live block styling + caret.**
6. **Composer frame + described slash palette + `@` picker + hint cleanup.**
7. **Picker component** (single + multi) — TUI only first, wired to permission
   (upgrade) for an end-to-end interactive path.
8. **`ask_user` tool + ChoiceAsk/ChoiceDecision protocol + daemon pause-await**,
   rendered through the picker.
9. **Motion pass** (per-tool spinners, completion tick, composer ease).
10. **Decompose `App::draw`** into the modules above as the work lands; final
    verification + visual run.

Each phase builds + clippies + tests green before the next.
