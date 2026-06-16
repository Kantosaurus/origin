# origin TUI Rework Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
>
> **Executor note:** the executors here are tool-equipped agents that read the live code. This plan therefore locks the **file decomposition + interface contracts + verification gates** (the contracts that make parallel work conflict-free) and references the spec (`docs/superpowers/specs/2026-06-16-tui-rework-design.md`) for visual detail, rather than transcribing every line. Where a signature, type, or token value is given, it is a **fixed contract** — implement against it exactly so parallel modules link.

**Goal:** Rework origin's bespoke-cell-grid TUI into a premium agentic coding terminal (signature copper spine + persistent chrome + tool-call blocks + deeper markdown/code + framed composer with palettes) and add inline interactive prompts (single + multi select) via a new `ask_user` tool — all on the existing SIMD renderer, no perf regression.

**Architecture:** Decompose `origin-cli/src/tui.rs::App::draw` (a ~1260-line monolith) into focused painter modules under `crates/origin-cli/src/tui/`, all reading colors/glyphs from one `tokens` source. A foundation pass (sequential) establishes the module skeleton + token contracts so the build-out modules (parallel) and the cross-crate `ask_user` wire never touch the same file. Integration (sequential) wires modules into the new layout and the interactive-prompt path end-to-end.

**Tech Stack:** Rust 2021 (workspace MSRV 1.96); bespoke `origin-tui` renderer (`Grid`/`Cell`/`Attr`, SIMD damage-diff, `Composer` 3-pane, `Scheduler`); `origin-cli` (crossterm 0.28 for terminal setup/input only); `origin-daemon` (protocol + agent loop); `origin-tools` (builtin tools). Build/verify via **git-bash** per-binary (`cargo build -p origin-cli|-p origin-daemon|-p origin-supervisor`), `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test -p <crate>`.

---

## Rendering contracts (read once; every painter uses these)

From `origin-tui` (re-exported in `tui.rs`):
- `Grid`: `.put(row: u16, col: u16, c: Cell)`, `.cols() -> u16`, `.rows() -> u16`.
- `Cell`: `Cell::new(ch: char, fg: u32, bg: u32, attr: Attr)`, `Cell::blank()`, `Cell::continuation(bg: u32)`. Colors are `0x00RRGGBB`; `fg/bg == 0` means "renderer default".
- `Attr`: `Attr::PLAIN`, `Attr::BOLD` (bitflags; dim/italic/strike/underline added in Task F1 if absent — see that task).
- Helpers already in `tui.rs`: `char_cell_width(ch) -> u16`, `find_closing(...)`, `wrap_segment_hanging(...)`.

Existing model types (keep names):
- `Style { fg: u32, bg: u32, bold: bool }`
- `ScrollLine { ... , literal: bool }` (a finalized transcript line; `literal` ⇒ render verbatim, skip markdown)
- `VisualLine<'a> { text: &'a str, fg: u32, bg: u32, bold: bool, literal: bool, indent: u16 }` (a post-wrap visual row)
- `theme::Palette` snapshot per frame via `App::palette()`.

Painters are pure where possible: `fn paint(grid: &mut Grid, region: Region, state: &…, tok: &Tokens)`. A `Region { top: u16, left: u16, width: u16, height: u16 }` is introduced in Task F1 (renderer-agnostic rectangle); painters clip to it.

---

## File structure (decomposition — locked in the foundation pass)

New directory `crates/origin-cli/src/tui/` (convert `tui.rs` → `tui/mod.rs`; the existing `App`, draw flow, tests stay in `mod.rs` initially and shrink as painters move out):

| File | Responsibility | Public surface (contract) |
| --- | --- | --- |
| `tui/tokens.rs` | Single source of colors + one glyph family; contrast-correct. Derives from `theme::Palette`. | `struct Tokens{…}`, `impl Tokens{ fn from_palette(p: theme::Palette) -> Self }`, `mod glyph { … }`, `fn tool_token(name:&str)->ToolToken`, `struct Region{…}` |
| `tui/chrome.rs` | Top context strip + bottom status zone; retire banner. | `fn draw_top(grid,&mut, region, ctx:&ChromeCtx, tok)`, `fn draw_status(grid, region, st:&StatusCtx, tok)` |
| `tui/transcript.rs` | Copper spine gutter, per-turn role headers, turn rhythm, hang-indent. | `fn layout_turn(lines:&[ScrollLine], width, tok) -> Vec<RenderRow>`, role-header helpers |
| `tui/toolblock.rs` | Contained tool-call block: header (icon+target+metrics), nested body/diff, line-count collapse. | `fn layout_tool(call:&ToolView, width, tok) -> Vec<RenderRow>`, `fn diff_gutter(...)` |
| `tui/markdown.rs` | Block + inline markdown → styled rows (lists, italic, code, strike, links, blockquote, rules, ATX h1–4). Pure. | `fn block_style(line:&str, tok)->BlockStyle`, `fn render_inline(grid,row,text,max_cols,style,tok,start_col)` |
| `tui/codeblock.rs` | Fenced code block framing + language label + band bg, calls `syntax`. | `fn layout_code(lines:&[&str], lang:&str, width, tok) -> Vec<RenderRow>` |
| `tui/syntax.rs` | Dep-free lexical tint. Pure, no I/O. | `fn tint(lang: Lang, line: &str) -> Vec<Span>` where `Span{start:usize,len:usize,kind:Tok}`; `fn lang_from_label(s:&str)->Option<Lang>` |
| `tui/composer.rs` | Framed input field, `›` glyph, soft-wrap cues, hint line. | `fn draw_field(grid, region, ed:&EditorView, tok)`, `fn draw_hint(grid, region, in_flight, tok)` |
| `tui/palette.rs` | Slash palette (described) + `@` file/agent picker popup. | `fn draw_slash(grid, region, items:&[SlashItem], sel, tok)`, `fn draw_mentions(grid, region, items:&[MentionItem], sel, tok)` |
| `tui/picker.rs` | Interactive choice component (single + multi); pure reducer + painter. | `struct PickerState{…}`, `enum PickerKey`, `enum PickerOutcome`, `fn reduce(&mut PickerState, PickerKey)->Option<PickerOutcome>`, `fn layout_picker(&PickerState, width, tok)->Vec<RenderRow>` |
| `tui/mod.rs` | `App`, draw orchestration calling the painters, input routing, existing tests. | unchanged external surface |

`RenderRow` (shared, defined in `tokens.rs` or a tiny `tui/row.rs`): an owned styled row the orchestrator blits into the grid — `struct RenderRow{ spans: Vec<RowSpan>, indent: u16 }`, `struct RowSpan{ text: String, fg: u32, bg: u32, attr: Attr }`. This decouples painters from absolute grid coordinates (they emit rows; `mod.rs` places them), which is what lets them be unit-tested and built in parallel.

Cross-crate (interactive prompts — separate files, parallel-safe):
- `crates/origin-tools/src/builtins/ask_user.rs` — new `ask_user` builtin tool.
- `crates/origin-daemon/src/protocol.rs` — add `StreamEvent::ChoiceAsk`, `ClientMessage::ChoiceDecision`, extend `PermissionDecision` with `always`.
- `crates/origin-daemon/src/…` (agent loop / dispatch) — pause-await-decision for `ask_user`.

Token de-dup targets: `crates/origin-cli/src/panel.rs`, `crates/origin-cli/src/goal_render.rs` (replace raw hex with `Tokens`).

---

## Parallelization strategy (how this maps to workflows)

- **Wave 0 — Foundation (sequential, 1 agent, gating):** Task F1–F3. Creates `tui/` dir, `tokens.rs` (+ `Region`, `RenderRow`), `glyph`, converts `tui.rs`→`tui/mod.rs`, adds empty module files with the exact public signatures above (compiling stubs returning empty/placeholder). Behavior-preserving: the app still renders as before. **Gate: 3 binaries build + clippy -D + existing tui tests green.** Commit. Everything else branches from this commit.
- **Wave 1 — Parallel build-out (Workflow, worktree-isolated agents — each owns ONE file/crate, branched from the Wave-0 commit):**
  - A: `tui/syntax.rs` (pure lexer + tests)
  - B: `tui/markdown.rs` + `tui/codeblock.rs` (consumes syntax via the locked `Span`/`Lang` contract)
  - C: `tui/picker.rs` (pure reducer + painter + tests)
  - D: `tui/chrome.rs`
  - E: `tui/composer.rs` + `tui/palette.rs`
  - F: `ask_user` wire — `origin-tools/.../ask_user.rs` + `origin-daemon/protocol.rs` additions + daemon pause-await (separate crates; independent of TUI files)
  - G: `tui/toolblock.rs`
  Each agent: implement file to its contract, add unit tests, `cargo test -p <crate> <module>` in its worktree, commit on its branch. Agents do NOT edit `tui/mod.rs`.
- **Wave 2 — Integration (sequential, me):** merge Wave-1 branches (distinct files ⇒ clean), then rewrite `tui/mod.rs` draw orchestration to call the painters in the new layout (chrome → spine/transcript → toolblocks → composer/palette/picker), wire picker↔permission and picker↔ChoiceAsk end-to-end, token de-dup in `panel.rs`/`goal_render.rs`, motion pass. **Gate: central build (3 binaries) + clippy -D + full touched-crate tests, then run the app.**

---

## Phase → Task index

- Phase 1 (tokens/glyphs) → **F1, F2, F3** (foundation) + token de-dup folded into Wave 2.
- Phase 2 (chrome) → **D**.
- Phase 3 (transcript spine) → **Wave 2 integration** (needs `mod.rs`) + role-header helpers.
- Phase 4 (tool blocks) → **G**.
- Phase 5 (markdown/code/syntax/caret) → **A + B** (+ caret in Wave 2).
- Phase 6 (composer/palette/@picker) → **E**.
- Phase 7 (picker component + permission upgrade) → **C** (+ permission wire in Wave 2).
- Phase 8 (`ask_user` + protocol + daemon) → **F** (+ TUI render via C in Wave 2).
- Phase 9 (motion) → **Wave 2**.
- Phase 10 (decompose `App::draw`) → **F1–F3 + Wave 2** (decomposition is the through-line, not a final big-bang).

---

## Wave 0 — Foundation (sequential, gating)

### Task F1: Convert `tui.rs` → `tui/mod.rs`; add `Region`, `RenderRow`, `Attr` extensions

**Files:**
- Move: `crates/origin-cli/src/tui.rs` → `crates/origin-cli/src/tui/mod.rs` (git mv; keep all content + tests)
- Create: `crates/origin-cli/src/tui/tokens.rs`
- Modify: `crates/origin-tui/src/grid.rs` (or wherever `Attr` lives) — ensure `Attr::DIM`, `Attr::ITALIC`, `Attr::STRIKE`, `Attr::UNDERLINE` exist (add missing bitflags + their handling in the ANSI emit path)

- [ ] **Step 1:** `git mv crates/origin-cli/src/tui.rs crates/origin-cli/src/tui/mod.rs`. Add `mod tokens;` etc. to `mod.rs`. Build `-p origin-cli` — expect green (pure move).
- [ ] **Step 2:** In `origin-tui`, confirm/add `Attr` flags DIM/ITALIC/STRIKE/UNDERLINE and their SGR codes in the terminal emit path (grep the diff-emit for where BOLD's `1;` is written; add `2;`(dim) `3;`(italic) `4;`(underline) `9;`(strike)). Add a unit test asserting each flag emits its SGR. If the renderer cannot express a flag, fall back to a color/weight change (document in the test). Run `cargo test -p origin-tui`.
- [ ] **Step 3:** In `tokens.rs` define `struct Region{ pub top:u16, pub left:u16, pub width:u16, pub height:u16 }` and `struct RenderRow{ pub spans: Vec<RowSpan>, pub indent: u16 }`, `struct RowSpan{ pub text: String, pub fg:u32, pub bg:u32, pub attr: Attr }`, plus `fn blit_row(grid:&mut Grid, row:u16, base_col:u16, max_cols:u16, r:&RenderRow)` (clips by `char_cell_width`, writes continuation cells for wide glyphs, fills bg to `max_cols` when `bg!=0`). Unit-test `blit_row` for a wide-glyph + clip case.
- [ ] **Step 4:** Build `-p origin-cli`, `-p origin-daemon`, `-p origin-supervisor`; `clippy --workspace --all-targets --locked -- -D warnings`; `cargo test -p origin-cli -p origin-tui`. All green.
- [ ] **Step 5:** Commit: `refactor(tui): split tui.rs into tui/ module, add Region/RenderRow + Attr flags`.

### Task F2: `Tokens` color system + glyph family

**Files:** `crates/origin-cli/src/tui/tokens.rs`; reference `crates/origin-cli/src/theme.rs`.

- [ ] **Step 1:** Define `struct Tokens` with the spec's named roles: surfaces `bg/raised/band`, `accent/accent_dim`, text `muted/body/bright`, headings `h1/h2/h3`, roles `you/origin`, `tool` + per-tool accents, `ok/warn/err`, `code_fg/code_bg`, `spine`, `sel_bg`. Values per spec §"Visual system": `bg 0x0F0D0B`, `raised 0x1A1714`, `accent 0xD4884E`, `body 0xC8C1B8`, `bright 0xF0EBE3`; retune `accent_dim`/`muted` so any text token is ≥4.5:1 on `raised` (add `fn contrast_ratio(fg:u32,bg:u32)->f32` + a test asserting `muted`,`body`,`bright`,`accent_dim` all clear 4.5:1 on both `bg` and `raised`); retone `you` off cornflower-blue to warm cream/amber.
- [ ] **Step 2:** `impl Tokens { fn from_palette(p: theme::Palette) -> Self }` so `/theme` and HighContrast/NO_COLOR map through (NO_COLOR ⇒ all colors 0, hierarchy via `Attr`). Add `fn high_contrast()` parity check test.
- [ ] **Step 3:** `mod glyph` with the one family (consts): `ORIGIN="◆"`, `SPINE='┃'`, tool icons (`EDIT='✎'` `BASH='⌘'` `GREP='⌕'` `READ='◇'` `WRITE='⇲'` `WEB='⚿'` `TASK='⊕'`), `RUN='▸'` `OK='✔'` `FAIL='✘'` `PERM='⚠'`, `PROMPT='›'`, `CURSOR='▸'`, `BOX_UNCHECKED='□'` `BOX_CHECKED='■'`, `CARET='▌'`, `QUOTE_BAR='▎'`. Add `fn tool_token(name:&str)->ToolToken{ icon:char, fg:u32 }` mapping tool names (`edit`,`bash`,`grep`,`read`,`write`,`web_fetch`,`task`,… default `◆`).
- [ ] **Step 4:** `cargo test -p origin-cli tokens`; clippy. Green.
- [ ] **Step 5:** Commit: `feat(tui): centralized Tokens color system + unified glyph family`.

### Task F3: Stub all painter modules with their locked signatures

**Files:** create `tui/chrome.rs`, `transcript.rs`, `toolblock.rs`, `markdown.rs`, `codeblock.rs`, `syntax.rs`, `composer.rs`, `palette.rs`, `picker.rs`; register in `mod.rs`.

- [ ] **Step 1:** Create each file with the exact public signatures from the File-structure table + the supporting input structs (`ChromeCtx`, `StatusCtx`, `ToolView`, `SlashItem`, `MentionItem`, `Lang`, `Tok`, `Span`, `BlockStyle`, `EditorView`, `PickerState/PickerKey/PickerOutcome`). Bodies are minimal compiling stubs (`Vec::new()` / no-op paint / `BlockStyle::default()`). Each input struct's fields are derived from the live `App` state the painter needs (read `mod.rs` to populate them).
- [ ] **Step 2:** Build `-p origin-cli`; clippy -D (allow `dead_code` on stubs via `#[allow(dead_code)]` with a `// Wave 1 fills this` note). Green.
- [ ] **Step 3:** Commit: `feat(tui): painter module skeleton with locked interfaces (Wave-1 surface)`. **This is the Wave-0 gate commit — Wave 1 branches from here.**

---

## Wave 1 — Parallel build-out (one agent per item, worktree-isolated)

Each task: implement to contract + unit tests; verify in the agent's worktree with `cargo test -p <crate>` (+ `clippy -p <crate> --all-targets -- -D warnings`); commit on the agent branch; do NOT touch `tui/mod.rs` or another task's file.

### Task A: `tui/syntax.rs` — lexical tint
- [ ] Implement `enum Lang{Rust,Js,Ts,Py,Json,Bash,Go}`, `fn lang_from_label(&str)->Option<Lang>`, `enum Tok{Keyword,Str,Comment,Num,Ident,Punct}`, `struct Span{start:usize,len:usize,kind:Tok}`, `fn tint(lang:Lang, line:&str)->Vec<Span>`. Per-language keyword sets + string/line-comment/number scanning, byte-range spans, no panics on partial/streaming lines, UTF-8 safe (operate on char indices→byte ranges).
- [ ] Tests: rust `fn`/`let`/`//`/string/number; json keys/strings/numbers; bash `#` comment + `$VAR`; python `def`/`#`/f-string; unknown lang ⇒ empty. Assert spans are non-overlapping and in-range.
- [ ] Commit: `feat(tui): dependency-free lexical syntax tint`.

### Task B: `tui/markdown.rs` + `tui/codeblock.rs`
- [ ] `markdown.rs`: `enum BlockKind{Para,H(u8),Bullet(u8),Ordered(u8),Quote,Rule,CodeFence{lang:String}}`, `fn block_style(line:&str, tok:&Tokens)->BlockStyle{ kind, fg, bg, attr, marker:Option<String>, indent:u16 }` (ATX h1–4, `-`/`*`/`+` bullets with nesting by indent, `N.` ordered, `>` quote, `---`/`***` rule, ``` ``` ``` fence open/close + lang). `fn render_inline(grid:&mut Grid,row:u16,text:&str,max_cols:u16,style:Style,tok:&Tokens,start_col:u16)` extending the existing `render_md_line` to add `*italic*`(Attr::ITALIC), `~~strike~~`(Attr::STRIKE), `[txt](url)`(txt in `accent` underline, url hidden), `` `code` `` (kept), `**bold**`(kept). Reuse `char_cell_width`/`find_closing`.
- [ ] `codeblock.rs`: `fn layout_code(lines:&[&str], lang:&str, width:u16, tok:&Tokens)->Vec<RenderRow>` — left rule (`▎` in `accent_dim`) + `band` bg + a dim language label row; each line tinted via `syntax::tint(lang_from_label(lang)…)` mapping `Tok`→token colors; replaces literal ``` fences with framing.
- [ ] Tests: `block_style` for each kind; `render_inline` italic/strike/link into a `Grid` asserting glyphs+attrs; `layout_code` row count + label + a tinted keyword cell.
- [ ] Commit: `feat(tui): deep markdown + framed code blocks with syntax tint`.

### Task C: `tui/picker.rs` — interactive choice (single + multi)
- [ ] `struct PickerOption{label:String, description:Option<String>}`, `struct PickerState{ question:String, options:Vec<PickerOption>, multi:bool, allow_custom:bool, cursor:usize, checked:Vec<bool>, custom:Option<String>, typing_custom:bool }`, `enum PickerKey{Up,Down,Toggle,Confirm,Digit(u8),Custom,Cancel,Char(char),Backspace}`, `enum PickerOutcome{Selected{indices:Vec<usize>, custom:Option<String>}, Cancelled}`, `fn reduce(&mut PickerState, PickerKey)->Option<PickerOutcome>`, `fn layout_picker(&PickerState, width:u16, tok:&Tokens)->Vec<RenderRow>`.
- [ ] Behavior per spec §"Picker behavior": single ⇒ Up/Down move, Confirm/Digit select-and-emit; multi ⇒ Toggle flips `checked[cursor]` (`□/■`), Confirm emits all checked; `Custom` (only if `allow_custom`) ⇒ `typing_custom=true`, Char/Backspace edit `custom`, Confirm emits custom; Cancel ⇒ `Cancelled`. `layout_picker` renders question (bold), each option row with `▸` cursor / `□■` boxes / number hints / description in `muted`, and a `✎ type your own…` row when allowed.
- [ ] Tests (pure, no grid needed for reducer): single-select digit jump; multi toggle+confirm yields sorted indices; custom flow; cancel; `layout_picker` row count for single vs multi.
- [ ] Commit: `feat(tui): reusable single/multi-select picker component`.

### Task D: `tui/chrome.rs` — top strip + status zone
- [ ] `struct ChromeCtx{ model:String, cwd:String, branch:Option<String>, elapsed:String, ctx_pct:u8 }`, `struct StatusCtx{ spinner:Option<String>, phase:Option<String>, tokens:u32, cost:Option<f64>, in_flight:bool }`. `draw_top` paints `◆ origin` + model·cwd·`⎇ branch` left, `◷ elapsed · ⛁ ctx%` right (truncate cwd middle on narrow widths; ctx% colorized warn/err past thresholds). `draw_status` paints the bottom quiet zone above a full-width rule. Reuse `format_tokens`/`context_window_for` (move them to `tokens.rs` or `pub(crate)` them from `mod.rs`; here assume `pub(crate)` re-export).
- [ ] Tests: `draw_top` into a `Grid`, assert wordmark glyph at col 0 + branch glyph present + right-aligned clock; narrow-width truncation doesn't overflow `cols`.
- [ ] Commit: `feat(tui): persistent top context strip + bottom status zone`.

### Task E: `tui/composer.rs` + `tui/palette.rs`
- [ ] `struct EditorView{ lines:Vec<String>, cursor_row:usize, cursor_col:usize, placeholder:String, scroll_top:usize, max_rows:u16 }`. `draw_field` paints the rounded frame (`╭╮╰╯│─`) with `›` prompt glyph on the first row, soft-wrap continuation cue in the gutter, "▴ more above" when `scroll_top>0`. `draw_hint` paints the de-noised, evenly-spaced keybind hint (`⏎ send · ⇧⏎ newline · / skills · @ files · ^c interrupt`), dimming the in-flight variant.
- [ ] `palette.rs`: `struct SlashItem{name:String, desc:String}`, `fn draw_slash` shows name in `accent` + **description in `muted`** (description is currently computed then discarded — surface it). `struct MentionItem{display:String, kind:MentionKind}` (`File`/`Dir`/`Agent`), `fn draw_mentions` renders the `@` popup with a per-kind glyph. Selection row uses `sel_bg`.
- [ ] Tests: `draw_field` frame corners present + prompt glyph; `draw_slash` shows a description substring; `draw_mentions` shows a kind glyph.
- [ ] Commit: `feat(tui): framed composer + described slash palette + @ mention picker`.

### Task F: `ask_user` wire (cross-crate)
- [ ] `origin-daemon/src/protocol.rs`: add `StreamEvent::ChoiceAsk{ id:String, question:String, options:Vec<ChoiceOption>, multi_select:bool, allow_custom:bool }`, `struct ChoiceOption{label:String, description:Option<String>}`, `ClientMessage::ChoiceDecision{ id:String, selected:Vec<usize>, custom:Option<String> }`; extend `PermissionDecision` with `#[serde(default)] always: bool`. Keep all serde additive (existing messages still parse).
- [ ] `origin-tools/src/builtins/ask_user.rs`: register builtin `ask_user` with input schema `{question:string, options:[{label,description?}], multi_select?:bool, allow_custom?:bool=true}`. On a channel that supports interactive choice, it emits a `ChoiceAsk` and awaits the matching `ChoiceDecision` (mirror `permission_ask`'s pause-await mechanism — find it in the daemon and reuse the same oneshot/registry); returns the chosen label(s)/custom text as the tool result. With no interactive channel (backward-compat), returns a text instruction telling the model to ask in prose.
- [ ] Daemon dispatch: route `ask_user` through the pause-await path; map `ChoiceDecision.selected` → labels, append `custom` if present; on empty/cancel return a "user skipped" result.
- [ ] Tests: daemon round-trip (dispatch `ask_user` → assert `ChoiceAsk` emitted → feed `ChoiceDecision` → assert tool result = selected labels); backward-compat (no interactive channel ⇒ text-instruction result); serde test that an old `PermissionDecision` without `always` still deserializes.
- [ ] Commit: `feat(daemon): ask_user tool + ChoiceAsk/ChoiceDecision protocol (pause-await)`.

### Task G: `tui/toolblock.rs`
- [ ] `struct ToolView{ name:String, target:String, status:ToolStatus, added:u32, removed:u32, elapsed_ms:u64, body:ToolBody }`, `enum ToolStatus{Running,Ok,Fail}`, `enum ToolBody{ Text(Vec<String>), Diff(Vec<DiffLine>), Read{path:String, start:u32, lines:Vec<String>} }`, `struct DiffLine{ kind:DiffKind, text:String }`, `enum DiffKind{Add,Del,Ctx}`. `fn layout_tool(&ToolView, width:u16, tok:&Tokens)->Vec<RenderRow>` — header row: `tool_token` icon+color, target, right-aligned `+N −N · elapsed · ✔/✘`; body nested with `│` connector; `diff_gutter` paints `+`(ok) `−`(err) `·`(ctx) gutter; reads show right-aligned line numbers; **collapse by line count** (default cap e.g. 12 rows) with a `… +N more lines` row via the existing `diff_elision_summary` style.
- [ ] Tests: header metrics formatting+right-alignment within `width`; diff gutter glyph/color per kind; collapse cap emits the elision row at the right count; read line-number alignment.
- [ ] Commit: `feat(tui): contained tool-call blocks with diff gutter + collapse`.

---

## Wave 2 — Integration (sequential)

### Task INT-1: Merge Wave-1 branches
- [ ] Merge A–G branches into `feat/tui-rework` (distinct files ⇒ no conflict; resolve any `mod.rs` registration trivially). Central build (3 binaries, git-bash) + `clippy --workspace --all-targets --locked -- -D warnings` + `cargo test` for origin-cli/origin-tui/origin-tools/origin-daemon. Green before proceeding. Commit the merge.

### Task INT-2: New draw orchestration (transcript spine + role headers + rhythm)
- [ ] Rewrite `App::draw` in `tui/mod.rs` to compute regions (top chrome / transcript / status / composer) and call painters: `chrome::draw_top` → transcript loop (per-turn role header `you`/`◆ origin`, copper `┃` spine in gutter via `glyph::SPINE`, blank-line rhythm between turns, hang-indent continuations) using `transcript::layout_turn` + `markdown`/`codeblock`/`toolblock` for each line's kind → `chrome::draw_status` → `composer::draw_field`/`draw_hint`, with `palette`/`picker` popups on top. Replace the old `draw_notices`/`draw_status_line`/`draw_input_card_bg`/`draw_keybind_hint` bodies with delegations (or delete once superseded). Keep scroll/selection/mouse behavior.
- [ ] Keep existing tui tests green (wrap/finalize/heading tests); update only assertions that intentionally changed (e.g., heading hash stripping now lives in `markdown::block_style`). Build + clippy + test. Commit.

### Task INT-3: Wire picker ↔ permission + ↔ ChoiceAsk (interactive prompts e2e)
- [ ] In `mod.rs` input routing (where `pending_permission`/`permission_answer` live in `main.rs` ~948 and `tui.rs`): when a `PermissionAsk` arrives, build a `PickerState` (options: Allow once / Deny / Always allow `<tool>`) and route keys through `picker::reduce`; on `Selected` send `PermissionDecision{ id, allow, always }`. When `StreamEvent::ChoiceAsk` arrives, build a `PickerState` from its options/`multi_select`/`allow_custom`, render inline via `picker::layout_picker` tied to the spine; on `Selected` send `ClientMessage::ChoiceDecision`; on `Cancelled` send the skip/deny. `Esc` dismisses an open picker/popup before it can quit the app. On resolve, collapse to a compact `you chose: …` transcript line.
- [ ] `main.rs`: read the new `StreamEvent::ChoiceAsk` and forward to the App; send `ChoiceDecision` over the client channel. Add a focused test or a scripted-input simulation for single + multi select producing the right decision.
- [ ] Build (3 binaries) + clippy + tests. Commit.

### Task INT-4: Token de-dup + NO_COLOR/theme parity
- [ ] Replace raw hex in `crates/origin-cli/src/panel.rs` and `crates/origin-cli/src/goal_render.rs` with `Tokens` (thread a `&Tokens` or `theme::Palette`-derived value in). Verify `/theme` switches re-theme panel + goal render; NO_COLOR still renders structure via `Attr`; HighContrast unaffected. Build + clippy + tests. Commit.

### Task INT-5: Motion pass
- [ ] Streaming caret `▌` on the live assistant token (append in the active row during streaming, removed on finalize). Per-running-tool micro-spinner in its `toolblock` header (drive from the existing `Spinner`). One-frame completion tick (`▸`→`✔`) on tool finish. Eased composer grow/shrink (interpolate target rows over a couple frames; respect the dirty-only `Scheduler` — no idle redraw beyond the existing watchdog). Retire the ASCII banner; first-run shows the compact `◆ origin` wordmark + one tip. Build + clippy + tests. Commit.

### Task INT-6: Final verification + visual run
- [ ] Central: build `-p origin-cli -p origin-daemon -p origin-supervisor`; `clippy --workspace --all-targets --locked -- -D warnings`; `cargo test` across touched crates. Run the app (`/run` or the built `origin` binary) and confirm: chrome strip, spine, a tool call rendering as a block with diff, a streamed markdown+code answer, the slash palette with descriptions, the `@` picker, a permission picker, and an `ask_user` single + multi select round-trip. Note anything deferred. Commit any fixups.

---

## Self-Review

**Spec coverage:** chrome (D/INT-2), spine+roles (INT-2), tool blocks+diff+collapse (G), markdown+code+syntax+live styling (B/INT-2) + caret (INT-5), composer+slash-desc+@picker+esc (E/INT-3), one-glyph/tokens+contrast (F2/INT-4), interactive single+multi via ask_user (C/F/INT-3) + permission upgrade (protocol F + INT-3), motion (INT-5), decompose App::draw (F1–F3/INT-2), NO_COLOR/theme/a11y preserved (F2/INT-4), no heavy deps / bespoke renderer kept (A is dep-free; all painters use existing Grid). All spec sections map to a task.

**Placeholder scan:** stub bodies in F3 are intentional compiling scaffolds (explicitly Wave-1-filled), not plan placeholders; every Wave-1/2 task states concrete types, signatures, and tests. No "TBD/handle-edge-cases/write-tests-for-above" left.

**Type consistency:** `Tokens`, `Region`, `RenderRow/RowSpan`, `Span{start,len,kind}`, `Lang`, `Tok`, `PickerState/PickerKey/PickerOutcome`, `ToolView/ToolStatus/ToolBody/DiffLine/DiffKind`, `ChoiceAsk/ChoiceOption/ChoiceDecision`, `BlockKind/BlockStyle`, `EditorView`, `SlashItem/MentionItem` — each defined once and consumed with the same field/variant names across tasks. `tint(Lang,&str)->Vec<Span>` produced by A is consumed by B with the same `Span`. `PickerOutcome::Selected{indices,custom}` (C) maps to `ChoiceDecision{selected,custom}` (F) in INT-3.
