# Onboarding TUI + Accurate Context Window — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
>
> **Executor note:** executors are tool-equipped agents that read the live code. This plan locks the **file decomposition + interface contracts + verification gates** and references the spec (`docs/superpowers/specs/2026-06-17-onboarding-tui-context-window-design.md`) for detail. Build/verify via **git-bash**: `cargo build -p origin-cli` (the cross-crate gate), `cargo build -p origin-daemon`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test -p <crate>`. NEVER `cargo build --workspace`.

**Goal:** Replace the line-based `origin init` wizard with an interactive TUI picker (banner → search → cyclable providers → OAuth/API-key → model-with-context), and resolve each model's real context window so the input-bar `ctx %` meter is accurate.

**Architecture:** One shared `model_context_window(model)->u32` resolver (marker `[Nm]/[Nk]` → accurate table → 200K fallback) used by the meter, the compaction cap, and the picker (Part B). A new `origin-cli/src/onboarding/` module — a pure search+cursor reducer + a thin crossterm raw-mode shell + flow orchestration — reusing the existing `origin_provider::catalog`, credential capture, `/models` probe, and config persistence (Part A). The line-based wizard stays as the non-TTY fallback.

**Tech Stack:** Rust 2021; `origin-tui` `Grid`/`Cell`/`Attr`; `crate::tui::tokens` (`Tokens`/`glyph`/`blit_row`); crossterm 0.28 (raw mode + key events; already a dep); `origin_provider::catalog` (`ProviderEntry`/`AuthScheme`/`Catalog`); `crate::init_probe` (`ConnectivityProbe`/`ProbeResult`).

---

## Existing interface contracts (read once; build against these)

- `origin_provider::catalog`: `struct ProviderEntry { id: Cow<str>, display_name: Cow<str>, wire: WireFormat, auth: AuthScheme, base_url, chat_path, default_model: Cow<str>, capabilities }`; `enum AuthScheme { None, ApiKey{header,prefix}, OAuth(OAuthSpec), SigV4{service}, Custom }`; `Catalog::builtin()`, `.entries() -> &[ProviderEntry]`, `.lookup(id) -> Option<&ProviderEntry>`.
- `crate::init_probe`: `struct ProbeResult { outcome: ProbeOutcome, models: Vec<String> }`; `enum ProbeOutcome { Ok, AuthFailed{status,detail}, Unreachable{detail}, Skipped{reason} }` + `.is_passing()`; `trait ConnectivityProbe { async fn probe(&self, &ProviderEntry, &KeyVault, account) -> ProbeResult }`; `LiveProbe`, `MockProbe`.
- `crate::init`: `run()`, `run_with<R: BufRead, W: Write>(...)`, `configure_role(...)`, `capture_credentials(...)`, `run_probe(...)`, `pick_model(...)`, `Role` (`Primary`/`Backup`/`Subagent`), `RoleConfig { provider, account, model }` (from `origin_cli::config`), `config::save_to`.
- Anthropic appears twice in the catalog: `anthropic` (`AuthScheme::ApiKey`) and `anthropic-oauth` (`AuthScheme::OAuth`). **Brand = entry id with a trailing `-oauth` removed.**
- Renderer: `Grid::put/cols/rows`, `Cell::new/blank`, `crate::tui::tokens::{Tokens, glyph, blit_row, Region, RenderRow, RowSpan, char_cell_width}`, `Tokens::from_palette(theme::Palette)`. crossterm raw mode + `event::read()` is set up in `origin-cli/src/main.rs` (reference its enter/leave-raw + alt-screen pattern).

---

## File structure

| File | Responsibility | Public surface (contract) |
| --- | --- | --- |
| `crates/origin-daemon/src/model_window.rs` (new) | The one context-window resolver. | `pub fn model_context_window(model: &str) -> u32` |
| `crates/origin-daemon/src/agent.rs` (modify) | Remove the private `model_context_window`; call `crate::model_window::`. | compaction soft-cap uses the shared fn |
| `crates/origin-daemon/src/lib.rs` (modify) | `pub mod model_window;` | — |
| `crates/origin-cli/src/tui/mod.rs` (modify) | `ctx_pct` uses the shared resolver; delete local `context_window_for`. | unchanged external surface |
| `crates/origin-cli/src/onboarding/mod.rs` (new) | Module root + `is_tty()` dispatch helper. | `pub async fn run_interactive(...) -> Result<()>` |
| `crates/origin-cli/src/onboarding/picker.rs` (new) | Pure search+cursor reducer. | `Row`, `PickerState`, `PickKey`, `PickResult`, `reduce`, `filtered` |
| `crates/origin-cli/src/onboarding/screen.rs` (new) | Crossterm raw-mode runner + render + masked text field. | `run_picker(...)`, `run_text_field(...)` |
| `crates/origin-cli/src/onboarding/flow.rs` (new) | Orchestration: brand-group → provider → auth → creds → probe → model. | `configure_role_interactive(...)`, `group_by_brand(...)` |
| `crates/origin-cli/src/init.rs` (modify) | TTY → `onboarding::run_interactive`; non-TTY → existing `run_with`. Expose shared helpers `pub(crate)`. | `run()` dispatches |

`format_ctx(window: u32) -> String` (e.g. `1M` / `200K`) lives in `onboarding/picker.rs` (or reuse `tui::chrome`/`format_tokens` style); used for the model `note`.

---

## Phase B — Accurate context window (do first; independently shippable)

### Task B1: shared `model_context_window` resolver
**Files:** Create `crates/origin-daemon/src/model_window.rs`; Modify `crates/origin-daemon/src/lib.rs`.
- [ ] **Failing tests** in `model_window.rs`: `model_context_window("claude-opus-4-8[1m]")==1_000_000`; `("claude-opus-4-8")==1_000_000`; `("claude-opus-4-8-20250101")==1_000_000` (version suffix); `("claude-sonnet-4-6")==200_000`; `("claude-haiku-4-5")==200_000`; `("gemini-2.5-pro")==1_000_000`; `("gpt-4o")==128_000`; `("foo[200k]")==200_000`; `("totally-unknown")==200_000` (fallback).
- [ ] **Implement** `pub fn model_context_window(model: &str) -> u32`: (1) regex-free parse of a trailing `[<digits><k|m>]` (case-insensitive) → multiply (k=1e3, m=1e6); (2) else lowercase + match: `claude-opus-4-8`/`opus-4-8` → 1_000_000; contains `gemini` → 1_000_000; contains `claude`/`opus`/`sonnet`/`haiku`/`fable` → 200_000; contains `gpt-4`/`gpt-5`/`o1`/`o3` → 128_000; (3) else 200_000. (Verify exact values against current provider docs; the structure marker→table→fallback is fixed.) `pub mod model_window;` in lib.rs.
- [ ] Run `cargo test -p origin-daemon --lib model_window` → green. Commit `feat(daemon): shared model_context_window resolver (marker + accurate table)`.

### Task B2: wire the resolver into compaction + the input-bar meter; delete the crude heuristic
**Files:** Modify `crates/origin-daemon/src/agent.rs:~158,~173`; `crates/origin-cli/src/tui/mod.rs:~735,~2548`.
- [ ] In `agent.rs`: delete the private `fn model_context_window` (and its `compaction_cap_tests` that assert the OLD `Option` API — port those asserts to B1's tests if not already covered) and replace its caller at ~158 with `let window = crate::model_window::model_context_window(model);` then the existing bytes math (drop the `map_or`).
- [ ] In `tui/mod.rs::ctx_pct` (~735): `let window = origin_daemon::model_window::model_context_window(&self.usage.model);`. Delete the local `fn context_window_for` (~2548) and update the doc-ref in `tui/chrome.rs` comment if it names it.
- [ ] Update the existing `ctx_meter_*` test(s) in `tui/mod.rs` if they assumed 200K for a claude model that is now 1M (set `last_ctx_tokens` + `usage.model` and assert the new pct). Run `cargo build -p origin-cli`, `cargo test -p origin-daemon --lib`, `cargo test -p origin-cli --lib tui::`, `cargo clippy --workspace --all-targets -- -D warnings`. Commit `fix(tui): input-bar ctx% uses the real per-model context window`.

---

## Phase A — Interactive onboarding picker

### Task A1: the pure search+cursor reducer
**Files:** Create `crates/origin-cli/src/onboarding/picker.rs` (+ `mod.rs` registering `mod picker;`); Modify `crates/origin-cli/src/main.rs`/`lib.rs` to add `mod onboarding;`.
- [ ] **Types:** `pub struct Row { pub value: String, pub label: String, pub note: Option<String> }`; `pub struct PickerState { pub items: Vec<Row>, pub query: String, pub cursor: usize }`; `pub enum PickKey { Char(char), Backspace, Up, Down, Enter, Esc }`; `pub enum PickResult { Selected(String), Back }`.
- [ ] **Functions:** `pub fn filtered(s: &PickerState) -> Vec<usize>` — indices of items whose `label` (lowercased) contains the query (lowercased); empty query → all. `pub fn reduce(s: &mut PickerState, k: PickKey) -> Option<PickResult>` — `Char`→push to query + `cursor=0`; `Backspace`→pop query + `cursor=0`; `Up`/`Down`→move cursor within `0..filtered.len()` (saturating, no wrap); `Enter`→`Some(Selected(items[filtered[cursor]].value.clone()))` (None if filtered empty); `Esc`→`Some(Back)`. `pub fn format_ctx(window: u32) -> String` (`1M`/`200K`).
- [ ] **Tests:** filter narrows + resets cursor to 0; up/down clamp within the filtered set; enter returns the highlighted `value` (not label); enter on empty-filter set returns None; esc→Back; backspace edits; `format_ctx(1_000_000)=="1M"`, `(200_000)=="200K"`. Run `cargo test -p origin-cli --lib onboarding::picker`. Commit `feat(onboarding): pure search+cursor picker reducer`.

### Task A2: brand grouping + flow orchestration (logic, testable)
**Files:** Create `crates/origin-cli/src/onboarding/flow.rs`; Modify `init.rs` to `pub(crate)` the shared helpers (`capture_credentials`, `run_probe`, the `pick_model` ordering) so `flow.rs` reuses them.
- [ ] `pub struct Brand { pub label: String, pub entries: Vec<ProviderEntry> }`; `pub fn group_by_brand(entries: &[ProviderEntry]) -> Vec<Brand>` — key by `id.trim_end_matches("-oauth")`, preserve catalog order, `label` = the brand's first entry `display_name` shortened (or the brand id). `pub fn auth_label(scheme: &AuthScheme) -> &'static str` (`OAuth`/`API key`/`AWS SigV4`/`none`).
- [ ] `pub fn provider_rows(brands: &[Brand]) -> Vec<Row>` (value = brand key, label = brand label, note = the auth options joined e.g. `OAuth / API key`); `pub fn model_rows(models: &[String], default: &str) -> Vec<Row>` — default-first ordering (port `pick_model`'s logic), note = `format_ctx(model_context_window(id))` + ` ctx`.
- [ ] **Tests:** `group_by_brand` collapses `anthropic`+`anthropic-oauth` into one brand with 2 entries; a single-scheme provider → 1 entry; `provider_rows`/`model_rows` shapes; `model_rows` annotates `claude-opus-4-8` with `1M ctx` and puts the default first. Run `cargo test -p origin-cli --lib onboarding::flow`. Commit `feat(onboarding): brand grouping + provider/model row builders`.

### Task A3: the crossterm raw-mode screen runner
**Files:** Create `crates/origin-cli/src/onboarding/screen.rs`.
- [ ] `pub fn run_picker(banner: &str, breadcrumb: &str, title: &str, mut state: PickerState, tok: &Tokens) -> std::io::Result<PickResult>` — enter raw mode (+ a saved-restore guard so a panic/`?` restores the terminal), loop: build a `Grid` sized to the terminal, paint the `◆ origin` wordmark + breadcrumb + a `───` rule + `title` + `⌕ {query}▌` search + the `filtered` rows (`▸` cursor in `tok.accent`, label in `tok.bright`/`body`, `note` in `tok.muted`, `sel_bg` on the cursor row) + a hint line, flush, then map `crossterm::event::read()?` keys → `PickKey` → `reduce`; return on `Some(result)`. Reuse `blit_row`/`RowSpan` to render rows. Leave raw mode on exit.
- [ ] `pub fn run_text_field(prompt: &str, masked: bool) -> std::io::Result<Option<String>>` — a one-line raw-mode input (echo `•` when masked) for the API-key paste; `Enter`→`Some(text)` (None if empty), `Esc`→`None`.
- [ ] **Test:** a render helper that paints one frame into a `Grid` for a fixed `PickerState` and asserts the `◆` wordmark at the top, the `⌕` search glyph, and the `▸` cursor on the selected filtered row (mirror the `tui` snapshot-test style; the event loop itself is smoke-tested manually). Run `cargo build -p origin-cli` + `cargo test -p origin-cli --lib onboarding::screen`. Commit `feat(onboarding): crossterm raw-mode picker screen + masked field`.

### Task A4: wire the interactive flow per role + TTY dispatch
**Files:** Modify `crates/origin-cli/src/onboarding/flow.rs` (add the async driver) and `crates/origin-cli/src/init.rs` (`run()` dispatch).
- [ ] `pub async fn configure_role_interactive(cat: &Catalog, vault: &KeyVault, probe: &dyn ConnectivityProbe, role: Role, tok: &Tokens) -> Result<RoleConfig>`: provider step (`run_picker` over `provider_rows`) → if the brand has >1 entry, auth step (`run_picker` over the brand's `auth_label`s) else take the single entry → resolve `ProviderEntry` → `capture_credentials` (OAuth → existing `keyring_login::run`; ApiKey → `run_text_field(masked=true)` then vault set; SigV4/None as today) → `run_probe` (retry loop preserved) → model step (`run_picker` over `model_rows`, with a trailing "type your own id" Row whose value sentinel drops to `run_text_field`). Returns `RoleConfig`. `Esc`/`Back` at a step returns to the previous step.
- [ ] `onboarding/mod.rs`: `pub async fn run_interactive(vault, cfg_path, probe, tok) -> Result<()>` running primary + the optional backup/subagent (a yes/no `run_picker` with `[Yes,No]` rows or a confirm helper), then `config::save_to`. In `init::run()`: `if std::io::stdin().is_terminal() { onboarding::run_interactive(...).await } else { run_with(stdin, stdout, ...).await }` (use `std::io::IsTerminal`). Build `tok` via `Tokens::from_palette(theme::Palette::default())` (or the active theme).
- [ ] Run `cargo build -p origin-cli`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test -p origin-cli` (the existing scripted `init` tests must still pass via the non-TTY path). Commit `feat(onboarding): interactive origin init for TTYs; line-based wizard as non-TTY fallback`.

### Task A5: verify end-to-end
- [ ] Central: build the 3 binaries; `clippy --workspace --all-targets -- -D warnings`; `cargo test -p origin-cli -p origin-daemon`. Run `origin init` in a real terminal: confirm banner, provider search/cycle, anthropic→OAuth/API-key step, model list with `1M/200K ctx`, esc-back, and that a piped/non-TTY `origin init` still runs the line-based wizard. Confirm the input-bar `ctx %` now reflects 1M for a 1M model. Commit any fixups.

---

## Self-Review

**Spec coverage:** banner+search+cyclable providers (A1/A3), OAuth-vs-API-key step (A4 + `group_by_brand` A2), model list with ctx labels (A2 `model_rows` + B1), reuse of catalog/credentials/probe/config (A2/A4), non-TTY fallback (A4), shared resolver wired to meter+compaction+picker (B1/B2/A2), no new deps / bespoke renderer (A3 uses `Grid`/`Tokens`+crossterm). All spec sections map to a task.

**Placeholder scan:** no TBD/handle-edge-cases/write-tests-for-above; every task names concrete types, fns, and tests. The "verify exact table values" note (B1) is a correctness check on fixed-structure data, not a placeholder.

**Type consistency:** `model_context_window(&str)->u32` (B1) is the single name used by agent.rs (B2), tui/mod.rs (B2), and `model_rows` (A2). `Row{value,label,note}`, `PickerState{items,query,cursor}`, `PickKey`, `PickResult{Selected,Back}`, `reduce`, `filtered`, `format_ctx` (A1) are consumed unchanged by `flow.rs` (A2/A4) and `screen.rs` (A3). `Brand{label,entries}` + `group_by_brand` (A2) feed `provider_rows` (A2) and the auth step (A4). `RoleConfig{provider,account,model}` and `Catalog`/`AuthScheme`/`ProbeResult` match the existing crates.
