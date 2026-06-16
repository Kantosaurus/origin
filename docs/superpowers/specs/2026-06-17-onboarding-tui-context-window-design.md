# Onboarding TUI + accurate context window — design spec

Date: 2026-06-17
Status: approved (brainstorming), pending implementation plan
Related: [[2026-06-16-tui-rework-design]] (shares the `Grid`/`Tokens`/`glyph` renderer primitives)

## Goal

Two cohesive improvements to origin's model/provider experience:

- **Part A — Onboarding picker:** replace the plain line-based `origin init`
  wizard with an interactive terminal screen: a wordmark banner, a search bar,
  a cyclable provider list, an OAuth-vs-API-key step, then a model list (with
  each model's context size shown). Same look-and-feel as the reworked TUI.
- **Part B — Accurate context window:** the input-bar `ctx %` meter currently
  shows the wrong fill because it uses a crude heuristic (every Claude model →
  200K), when e.g. `claude-opus-4-8` actually has a 1M window. Replace both
  crude tables with one shared resolver and wire it everywhere.

These reuse origin's existing provider catalog, credential capture, `/models`
probe, and config persistence — only the *interaction* and the *context
resolution* change.

## Part A — Onboarding interactive picker

### What exists (reused, not rewritten)
`crates/origin-cli/src/init.rs` already has the full onboarding *logic*:
- `Catalog::builtin()` — static `ProviderEntry { id, display_name, wire, auth }`
  list. A brand with multiple sign-in methods appears as multiple entries —
  notably `anthropic` (`AuthScheme::ApiKey`) and `anthropic-oauth`
  (`AuthScheme::OAuth`). **This pair is the "OAuth vs API key" choice.**
- `capture_credentials(...)` — per-`AuthScheme` (ApiKey paste / OAuth via
  `keyring_login::run` / SigV4 / None), persists to the `KeyVault`.
- `run_probe(...)` — GETs the provider's `/models` endpoint; `ProbeResult.models:
  Vec<String>` is the live model list (already used by `pick_model`).
- `OriginConfig` + `config::save_to` — persistence (unchanged).

### Flow (per role slot: primary, and the optional backup / subagent)
1. **Provider** — banner + search bar + cyclable list of provider *brands*.
2. **Auth type** — only when the chosen brand has >1 auth scheme (e.g.
   anthropic → `OAuth` / `API key`); single-scheme brands skip this step. The
   (brand, auth) pair resolves back to a concrete catalog entry id.
3. **Credential capture** — reuse `capture_credentials` for the resolved entry
   (OAuth launches the existing browser flow; API key is a masked in-screen
   text field; SigV4/None unchanged), then `run_probe`.
4. **Model** — banner + search bar + cyclable list of the probe's live models,
   each annotated with its context size (Part B resolver). Default-first
   ordering preserved from `pick_model`. A "type your own id" escape hatch stays.

`esc` steps back one stage; `⏎` selects; typing filters; `↑↓` (and `j/k`) cycle
within the filtered set. Layout (matches the reworked TUI's tokens/glyphs):

```
 ◆ origin
 ───────────────────────────────────────────────
 Choose your provider                    primary

 ⌕ anthr▌

 ▸ anthropic      Claude    · OAuth / API key
   openai         GPT       · API key
   google         Gemini    · API key
 ↑↓ cycle · ⏎ select · type to search · esc back
```
```
 ◆ origin · anthropic · oauth
 ───────────────────────────────────────────────
 Choose a model

 ⌕ opus▌

 ▸ claude-opus-4-8      1M ctx
   claude-sonnet-4-6    200K ctx
   claude-haiku-4-5     200K ctx
 ↑↓ cycle · ⏎ select · esc back
```

### Architecture / components (new `crates/origin-cli/src/onboarding/` module)
- `onboarding/picker.rs` — a **pure search+cursor reducer** for a filtered,
  cyclable list. State `{ items: Vec<Row>, query: String, cursor: usize }` where
  `Row { value: String, label: String, note: Option<String> }`; a `filtered()`
  view (case-insensitive substring/subsequence match over `label`);
  `enum PickKey { Char(char), Backspace, Up, Down, Enter, Esc }`;
  `fn reduce(&mut State, PickKey) -> Option<PickResult>` where
  `PickResult { Selected(String) | Back }`. Cursor clamps to the filtered set;
  typing resets the cursor to 0. Pure ⇒ unit-tested without a terminal.
- `onboarding/screen.rs` — a **raw-mode runner**: enters crossterm raw mode,
  renders banner + search + list into a `Grid` via `origin-tui` +
  `crate::tui::tokens` (`Tokens`, `glyph`, `blit_row`), reads `KeyEvent`s,
  drives `picker::reduce`, returns the result. Thin shell (smoke-tested). A
  small masked text-field helper here handles the API-key paste step.
- `onboarding/flow.rs` — **orchestration**: brand grouping (build brand →
  Vec<entry>, keyed by the entry id with a known auth suffix like `-oauth`
  stripped), provider step → auth-type step (only when >1 scheme) → resolve
  entry → reuse `capture_credentials` + `run_probe` → model step (annotated via
  Part B). Returns a `RoleConfig`. Shared catalog/credential/probe helpers are
  factored out of `init.rs` so both the interactive and line-based paths call
  them.

### Non-TTY fallback (hard requirement)
When stdin is not a TTY (CI, pipes, the existing scripted tests), `origin init`
keeps the **current line-based wizard** (`init::run_with<R: BufRead, W: Write>`)
unchanged. The interactive screen is the TTY path only; `init::run` detects a
TTY and dispatches. This preserves every existing onboarding test and CI.

## Part B — Accurate context window

### Single shared resolver
`fn model_context_window(model: &str) -> u32` — canonical, always returns a
value (no `Option`):
1. **Explicit marker:** parse a trailing `[<n>m]` / `[<n>k]` (case-insensitive,
   e.g. `claude-opus-4-8[1m]` → 1_000_000, `foo[200k]` → 200_000) and use it.
2. **Accurate table** (by model id, then family substring), known values:
   - `claude-opus-4-8` family → 1_000_000
   - other claude (sonnet/haiku/opus ≤4-7/fable) → 200_000
   - `gemini-2.x` / gemini → 1_000_000
   - `gpt-4*` / `gpt-5*` / o-series → 128_000 (or the model's known larger
     window where applicable — values verified at implementation time)
3. **Fallback:** 200_000 for unrecognized models (was 128K; raised so an
   unknown model under-reports its fill rather than over-reports).

Exact table values are verified against current provider docs during
implementation; the structure (marker → table → fallback) is fixed.

### Placement + wiring
Canonical resolver lives in `crates/origin-daemon` (e.g. promote the existing
`agent.rs::model_context_window` into a small `model_window.rs`, made `pub` and
returning `u32`). `origin-cli` already depends on `origin-daemon`, so:
- `crates/origin-cli/src/tui/mod.rs::ctx_pct` (line ~735) calls the shared
  resolver; the local `context_window_for` (line ~2548) is **deleted**.
- `crates/origin-daemon/src/agent.rs` compaction soft-cap (line ~158) uses the
  same resolver (it already calls `model_context_window` — now non-`Option`,
  adjust the `map_or`).
- `onboarding/flow.rs` annotates each model row via the same resolver, formatted
  as `1M ctx` / `200K ctx` (reuse/extend the `format_tokens`-style abbreviation).

This makes the meter, the compaction cap, and the onboarding picker all agree.

## Non-goals / constraints
- No new dependencies; build the picker on the existing `Grid`/`Tokens`/`glyph`
  primitives + crossterm (already a dependency), not a select crate.
- Do not change credential capture, the `/models` probe, vault, or
  `config.toml` persistence — only the interaction layer and context resolution.
- Keep the non-TTY line-based wizard and all existing onboarding tests green.
- Keep NO_COLOR / theme behavior (the onboarding screen reads from `Tokens`).
- Live model-discovery of `context_window` is explicitly out of scope (the
  static table + marker covers the need; revisit later if a model is missed).

## Testing
- **Part B:** unit tests for `model_context_window`: marker `[1m]`/`[200k]`
  parse, `claude-opus-4-8` → 1M, sonnet → 200K, gemini → 1M, gpt-4o → 128K,
  unknown → 200K fallback, version-suffix tolerance.
- **Part A:** unit tests for `picker::reduce` (filter narrows + resets cursor,
  up/down cycle within filtered, enter selects the highlighted value, esc → Back,
  backspace edits the query, empty-list guard). Brand-grouping unit test
  (anthropic → {oauth, apikey}; single-scheme brand → no auth step). A
  render-to-`Grid` snapshot for one screen (banner + search + a highlighted row).
  The non-TTY path keeps the existing scripted `init` tests unchanged.
- Per-crate `build` + `clippy --all-targets -D warnings` + `test`, then run
  `origin init` in a real terminal to confirm the interactive flow.

## Implementation phases
1. **Part B — context-window resolver** (shared `model_context_window` + wire to
   `ctx_pct`, compaction, and delete `context_window_for`; tests). Small,
   high-value, independently shippable — do first.
2. **Part A — onboarding picker:** `picker.rs` reducer (+ tests) → `screen.rs`
   raw-mode runner + render → `flow.rs` orchestration (brand grouping, auth-type
   step, reuse credentials/probe, model step with ctx labels) → TTY dispatch in
   `init::run` with the line-based non-TTY fallback preserved.

Each phase builds + clippies + tests green before the next.
