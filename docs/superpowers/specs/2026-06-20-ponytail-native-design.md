# Ponytail-native for origin — design spec

- **Date:** 2026-06-20
- **Status:** Approved (brainstorming) → ready for implementation plan
- **Author:** origin team
- **Topic:** Port ponytail (the "lazy senior dev" ruleset) into origin as a native Rust subsystem so every code-write "goes through ponytail."

---

## 1. Goal

Make laziness-as-discipline a native, always-on property of origin. Two mechanisms working together:

1. **Ruleset injection** — the ponytail ladder + guards conditions the model on every turn (cached system-prompt block, mode-aware). This makes the model write lean code *upfront*.
2. **Deterministic dependency gate** — a pure Rust check at tool-dispatch time that intercepts every code-write and shell command, detects *added dependencies*, and (in `full`/`ultra`) blocks the ones that don't earn their place. This is the deterministic backstop for the one thing a prompt can't guarantee, and the novel "signature subsystem."

Non-goal restatement up front: the gate does **not** call an LLM and does **not** try to detect "speculative abstraction" or reinvented-stdlib *code patterns* in v1. It detects **dependency additions only** — the highest-leverage, lowest-false-positive target, and the one ponytail already ships a lookup table for.

## 2. Locked decisions (record of the brainstorming forks)

| Decision | Choice |
|---|---|
| Mechanism | Hard pre-write gate **plus** ruleset injection (paired) |
| Detection scope | Dependency additions at **all entry points** (manifest edits + package-manager Bash commands), table-driven |
| Interactive override | Reuse origin's prompter; offer `[Allow once · Allow & remember · Deny]`; "remember" → allowlist |
| No-user fallback (subagent/swarm/headless/scheduled) | **Allow + log** a warning to the debt ledger |
| Build scope | **Full port** — gate + injection + `/ponytail` toggle + all 5 commands + statusline badge |
| Modes | `off / lite / full / ultra`; **default `full`** |
| `ultra` distinction | Mechanically stricter (option B): challenges **every** new dependency (rung 4), not only replaceable ones |

## 3. New crate: `origin-ponytail`

A pure library crate (no `origin-daemon` dependency), mirroring the `origin-conseca` / `origin-policy` / `origin-review` subsystem convention. Edition 2021 (per origin crate norm). The daemon and CLI depend on it; it depends on nothing origin-specific beyond `serde`/`serde_json`/`toml` and small parsing helpers.

### 3.1 Modules

| module | responsibility | key public interface |
|---|---|---|
| `mode.rs` | the 4 modes + parsing (mirror of `origin-cli/src/effort.rs`) | `enum PonytailMode { Off, Lite, Full, Ultra }`; `as_str(&self)->&'static str`; `parse_level(&str)->Option<PonytailMode>`; `parse_ponytail_command(line:&str)->Option<Option<PonytailMode>>` |
| `ruleset.rs` | embedded ruleset text, single source of truth | `system_block(mode:PonytailMode)->String` → `<origin-ponytail level="…">…</origin-ponytail>`, filtered to the active intensity; empty string for `Off` |
| `native_table.rs` | ported platform-native **dependency** rows | `lookup(eco:Ecosystem, pkg:&str)->Option<&'static NativeReplacement>`; `enum Ecosystem { Npm, PyPI, Cargo, Go, RubyGems }`; `struct NativeReplacement { rung:u8, native:&'static str, note:&'static str }` |
| `detect.rs` | find *added* deps from a write or a shell command | `manifest_deps_added(file:&str, before:Option<&str>, after:&str)->Vec<Dep>`; `bash_installs(cmd:&str)->Vec<Dep>`; `struct Dep { eco:Ecosystem, name:String }` |
| `gate.rs` | pure verdict | `evaluate(input:&GateInput)->Vec<Flagged>`; `struct GateInput { tool, args, mode, allowlist }`; `struct Flagged { dep:Dep, kind:FlagKind }`; `enum FlagKind { Replaceable(&'static NativeReplacement), Unjustified }` |
| `config.rs` | resolve mode + allowlist from `~/.origin/ponytail.toml` / env | `resolve_mode(request:Option<PonytailMode>)->PonytailMode`; `allowlist()->BTreeSet<String>`; `remember(dep:&Dep)` |
| `debt.rs` | append-only ledger | `log(event:&DebtEvent)` → `~/.origin/ponytail-debt.jsonl`; `read()->Vec<DebtEvent>` |
| `commands.rs` | embedded prompts + static text + harvester | `review_prompt()->&str`, `audit_prompt()->&str`, `gain_text()->&str`, `help_text()->&str`, `harvest_debt(repo_root)->DebtReport` |

### 3.2 `ruleset.rs` — the injected text

The ruleset is the ponytail `skills/ponytail/SKILL.md` body, ported **verbatim** into a Rust string constant, minus the YAML frontmatter, with the intensity-specific lines (the `| **lite** |` / `| **full** |` / `| **ultra** |` table rows and the `- lite:` / `- full:` / `- ultra:` worked-example bullets) filtered to the active mode — exactly the filtering `hooks/ponytail-instructions.js::filterSkillBodyForMode` performs. Wrapped as:

```
<origin-ponytail level="full">
…filtered ruleset body…
</origin-ponytail>
```

A unit test asserts the embedded constant stays in sync with a canonical copy (the same guard ponytail's `scripts/check-rule-copies.js` provides). The canonical copy lives at `crates/origin-ponytail/assets/ponytail-ruleset.md`; `ruleset.rs` `include_str!`s it so there is exactly one source.

### 3.3 `native_table.rs` — ported dependency knowledge

Static table seeded from `ponytail/docs/platform-native.md`, keyed by ecosystem. Only **package → native** rows are included (HTML/CSS/DB rows are not dependencies and are not gateable). Each entry names the rung (2 = stdlib, 3 = native platform) and the replacement.

**npm / JavaScript+Node (seed set):** `query-string`, `qs`→`URLSearchParams`; `lodash.clonedeep`/`lodash`→`structuredClone`/`Object.groupBy`/native; `moment`/`date-fns`→`Intl.DateTimeFormat`/`Intl.RelativeTimeFormat`; `numeral`/`accounting`→`Intl.NumberFormat`; `clipboard.js`→`navigator.clipboard`; `uuid`→`crypto.randomUUID`; `uuid-validate`→regex; `left-pad`→`padStart`; `is-online`→`navigator.onLine`; `mkdirp`/`make-dir`→`fs.mkdirSync({recursive:true})`; `rimraf`→`fs.rmSync({recursive,force})`; `slash`→`path.posix`; `is-stream`→`instanceof stream.Readable`; `object-assign`→`Object.assign`; `array-uniq`→`[...new Set()]`; `array-flatten`/`flat`→`Array.flat`; `path-exists`→`fs.existsSync`; `load-json-file`/`write-json-file`→`JSON.parse`/`JSON.stringify` + `fs`; `pkg-dir`→`path.resolve`.

**PyPI (seed set):** `python-dateutil`→`datetime.fromisoformat`; `pytz`→`zoneinfo`; `attrs`→`@dataclass`; `six`/`pathlib2`/`enum34`→drop (stdlib); `typing_extensions`→builtin generics; `simplejson`→`json`; `mergedeep`→`dict | dict`; `more-itertools`→`itertools`; `toolz`→`functools`; `tabulate`→`pprint`.

**Cargo / Rust (curated):** `lazy_static`→`std::sync::LazyLock` (stable 1.80); `once_cell`→`std::sync::OnceLock`/`LazyLock` (std covers most uses); `num_cpus`→`std::thread::available_parallelism` (1.59); `maplit`→`HashMap::from([...])`/`BTreeMap::from` (1.56); `failure`→`std::error::Error` (+`thiserror`/`anyhow`); `error-chain`→`std::error::Error`. *Omitted (genuinely earn their place): `rand`, `itertools`, `chrono`/`time`, `smallvec`, `derive_more`.*

**Go (curated):** `github.com/pkg/errors`→`errors` + `fmt.Errorf("%w", …)` / `errors.Is`/`As`/`Unwrap` (1.13); `github.com/sirupsen/logrus`→`log/slog` (1.21); `github.com/gorilla/mux`→`net/http.ServeMux` method+wildcard patterns (1.22); `golang.org/x/exp/slices`→`slices` (1.21); `golang.org/x/exp/maps`→`maps` (1.21). *Omitted: `github.com/google/uuid` (no stdlib UUID), `testify` (genuinely useful).*

**RubyGems (curated):** `rest-client`→`net/http` (also unmaintained); `awesome_print`→`pp` (stdlib, debug). *Omitted: `httparty` (can't tell simple from real use — same rule that omits `requests`), `faker`, `pry`/`byebug`, `dotenv` (no stdlib equivalent).*

Each entry carries its rung and a one-line native replacement. The table is still extensible (new rows are pure data additions); detection works for every ecosystem regardless of table coverage — the table only decides whether a flagged dep gets a *specific native suggestion* (`full`/`lite`) or, for an unlisted dep in `ultra`, a *generic justify-it challenge*.

**Deliberately omitted (edge-case-correct):** `ms`, `requests`, `click` — `platform-native.md` itself says these earn their place for real use; a deterministic gate cannot tell "simple GET" from real use, so it must not block them. Lazy means less code, not the flimsier call.

### 3.4 `detect.rs` — dependency extraction

**Manifests** (by filename): `package.json` (`dependencies`/`devDependencies` keys), `Cargo.toml` (`[dependencies]`/`[dev-dependencies]`/`[build-dependencies]`), `requirements.txt` (one req per line), `pyproject.toml` (`[project].dependencies`, `[tool.poetry.dependencies]`), `go.mod` (`require`), `Gemfile` (`gem "…"`). Compare `before` vs `after` and emit only **newly added** package names. For `Write` with no `before`, the daemon supplies the on-disk prior content (read-before-write guard guarantees it was read); if genuinely new file, every listed dep is "added."

**Bash installs** (parsed from the command string): `npm|yarn|pnpm|bun (add|install|i) <pkgs>`, `cargo add <pkgs>`, `pip|pip3|uv pip install <pkgs>`, `poetry add <pkgs>`, `go get <pkgs>`, `gem install <pkgs>`, `composer require <pkgs>`. Flags (`-D`, `--save-dev`, `--global`, version specs `pkg@1.2`, `pkg==1.2`, `pkg@scope`) are stripped to the bare name; scoped npm names (`@scope/x`) are preserved.

**Mandatory negative cases (tested):** bare `npm install` / `yarn` / `pnpm i` / `bun install` (no package named → installing existing manifest deps), `pip install -r requirements.txt`, `cargo build`, `go build`, `go mod download` → **emit nothing**. Installing existing deps is not adding a dep.

### 3.5 `gate.rs` — the pure verdict

```rust
pub fn evaluate(input: &GateInput) -> Vec<Flagged>
```

1. `Off` → `[]`.
2. Collect added deps via `detect` (manifest or bash, per tool).
3. Drop any dep in the allowlist.
4. For each remaining dep:
   - table hit → `Flagged{ kind: Replaceable(repl) }`.
   - no table hit → in `Ultra`: `Flagged{ kind: Unjustified }`; in `Lite`/`Full`: skip (full only blocks *replaceable* deps).

The gate is pure — it returns *what was flagged*, never performs I/O or prompts. The daemon turns flags into action.

## 4. Daemon wiring (`origin-daemon`)

### 4.1 Mode plumbing (mirrors `/effort`, confirmed in exploration)

- `protocol.rs::PromptRequest` gains `pub ponytail: Option<String>` with `#[serde(default, skip_serializing_if = "Option::is_none")]` (wire stays byte-identical when unset).
- `LoopOptions` gains `pub ponytail: PonytailMode`.
- `resolve_mode` precedence: per-request token → `~/.origin/ponytail.toml` `defaultMode` → `PONYTAIL_DEFAULT_MODE` / `ORIGIN_PONYTAIL` env → **`Full`**.

### 4.2 Injection

In the system-prompt assembly array at `crates/origin-daemon/src/agent.rs:3260-3431`, add `&ponytail_block` where `ponytail_block = origin_ponytail::ruleset::system_block(opts.ponytail)`. Empty when `Off`. It joins the cached static prefix (mode is fixed for the turn → cache byte-stability preserved, consistent with the existing volatile-vs-static split documented at `agent.rs:3406-3410`).

### 4.3 Gate

In the pre-dispatch filter region (`crates/origin-daemon/src/agent.rs:~4265`, alongside the conseca/governance filtering of path-touching tools), for tools in `{Edit, Write, MultiEdit, ApplyPatch, Bash}`:

1. `mode == Off` → skip.
2. Build `GateInput`; for `Write`/manifest edits without `before`, load prior on-disk content. `evaluate()` → flags.
3. No flags → proceed.
4. **`Lite`** → never block: attach a one-line advisory to the tool result and `debt::log` an advisory event; proceed.
5. **`Full` / `Ultra`** → for the flagged deps:
   - **Prompter present** (interactive session): reuse `ipc_prompter`/`cli_prompter` (the `ask_user`/permission prompt path) to ask `[Allow once · Allow & remember · Deny]`.
     - *Deny* → return a **recoverable** `ToolError { class: Edit/Bash, reason: "ponytail.blocked", message: <native suggestion or justify-it challenge>, recoverable: true, hint }` (shape per `origin-tools/src/error.rs`). The model sees the suggestion and rewrites with the native API.
     - *Allow once* → proceed; `debt::log` an override.
     - *Allow & remember* → `config::remember(dep)` (append to `ponytail.toml` allowlist); proceed; `debt::log`.
   - **No prompter** (subagent/swarm/headless/scheduled — detected the same way the existing prompter-availability check is) → **allow + `debt::log` a warning**.

Loop-safety: because Deny only happens with a human in the loop and every non-interactive path allows, the gate can never deadlock the (uncapped) loop.

## 5. CLI wiring (`origin-cli`)

- `App` (in `tui/mod.rs`) gains `pub ponytail_mode: Option<PonytailMode>` (seeded from `--ponytail`, mutated by `/ponytail`).
- `/ponytail [off|lite|full|ultra]` handler in `main.rs` (mirrors the `/plan` block at `main.rs:1799-1819`): parse via `origin_ponytail::mode::parse_ponytail_command`; no-arg reports current mode; set `app.lock().ponytail_mode`; add a `system>` line; `return`.
- `--ponytail <level>` global + `run`-subcommand flag in `cli_def.rs` (mirrors `--effort`).
- Snapshot before each prompt (beside the `effort` snapshot at `main.rs:~2178`): `let ponytail = app.lock().ponytail_mode.map(|m| m.as_str().to_string());` → carried on `PromptRequest`.

## 6. Commands (full port)

| command | implementation |
|---|---|
| `/ponytail [off\|lite\|full\|ultra]` | CLI toggle (section 5) |
| `/ponytail-review` | **auto-runs**: gathers `git diff` (working tree vs HEAD), feeds it + the embedded review prompt (ponytail's `skills/ponytail-review/SKILL.md`) to the model in one turn, and returns the delete-list directly — no separate "now review it" step. **Complements** `origin-review`/`/code-review` — does not replace it. |
| `/ponytail-audit` | **auto-runs**: same, scoped to the whole repo tree instead of the diff (embedded `ponytail-audit` prompt) |
| `/ponytail-debt` | `commands::harvest_debt`: read `~/.origin/ponytail-debt.jsonl` + grep `ponytail:` comments across the repo → table |
| `/ponytail-gain` | static benchmark scoreboard (`commands::gain_text`) |
| `/ponytail-help` | command reference (`commands::help_text`) |

Embedded skills register through the existing `origin-skills` embedded-skill mechanism so they're discoverable like other skills.

## 7. Statusline badge

Surface `[PONYTAIL]` (full/lite) or `[PONYTAIL:ULTRA]` in the existing TUI status readout (`crates/origin-cli/src/tui/mod.rs`), driven by `App.ponytail_mode`; `Off` → no badge. No separate statusline script (origin renders its own status line, unlike the Claude Code plugin path).

## 8. Files & config

- `~/.origin/ponytail.toml` — `defaultMode = "full"`, `allow = ["axios", …]`.
- `~/.origin/ponytail-debt.jsonl` — append-only ledger of advisories/overrides.
- Env — `PONYTAIL_DEFAULT_MODE` / `ORIGIN_PONYTAIL`. `ORIGIN_HOME` relocates `~/.origin` (existing origin convention).

## 9. Mode semantics (authoritative)

| mode | injection | gate behavior |
|---|---|---|
| `off` | none | none |
| `lite` | lite intensity | flags replaceable deps → **advisory only** (warn + log, never block) |
| `full` *(default)* | full intensity | flags replaceable deps → **block** (interactive prompt / headless-allow) |
| `ultra` | extremist intensity | flags **every** new dep → block; replaceable → native suggestion, otherwise → "justify it (rung 4)" challenge |

## 10. Testing (TDD)

**Unit (`origin-ponytail`):**
- `mode`: parse all levels + aliases + the `/ponytail` command grammar (usage error vs not-this-command), round-trip `as_str`.
- `native_table`: representative lookups (`lodash`→`Object.groupBy`, `pytz`→`zoneinfo`); omitted packages (`ms`, `requests`) return `None`.
- `detect`: manifest add-detection per format; bash install parsing per manager; **negative cases** (bare `npm install`, `pip install -r`, `cargo build`, scoped names, version pins, flags).
- `gate`: verdict per mode (`off`=∅, `lite`/`full` only replaceable, `ultra` all); allowlist short-circuit; `ultra` `Unjustified` for no-hit dep.
- `ruleset`: `system_block` filters intensity rows correctly; sync test vs canonical asset.

**Integration (`origin-daemon`):**
- Edit adding `lodash` to `package.json` in `full` → flagged; with a (mock) prompter denying → recoverable `ToolError`; allowing → write proceeds.
- Same in a subagent/headless context → allowed + ledger warning.
- Allowlisted dep → silent passthrough.
- Bare `npm install` Bash → not flagged.
- Injection present in system prompt for `full`, absent for `off`.

## 11. Why it's novel (signature-subsystem rule)

First agent harness with a **native, deterministic, table-driven dependency-bloat gate wired into tool dispatch** — zero LLM calls, microsecond overhead, kills the dependency rabbit-hole (the date-picker→404-lines class of over-build) at the source — combined with an always-on lazy ruleset living in the cached prefix (token-cheap). Beats the openclaude/jcode/opencode baseline on tokens (fewer deps pulled, leaner diffs) with no latency cost.

## 12. Non-goals (YAGNI)

- No LLM-in-the-gate.
- No "speculative abstraction" / dead-flexibility detection (false-positive prone for a deterministic gate).
- No reinvented-stdlib *code-pattern* detection in v1 — **deps only**.
- No statusline shell scripts; origin owns its status line.
- No new prompt path — reuse the existing prompter.

## 13. Risks & mitigations

- **False positive blocks a legitimate dep.** Mitigated: interactive-only blocking + allowlist + "Allow & remember" + conservative omissions (`ms`/`requests`/`click`). Worst case the user clicks Allow once.
- **Manifest/Bash parser misses an entry point.** Mitigated: gate is additive — a missed dep simply isn't flagged (fails open, never breaks a write). New formats/managers are table/parser additions.
- **Ruleset drift from canonical ponytail.** Mitigated: single `include_str!` source + sync test.
- **Ultra noise** (every dep prompts). Mitigated: allowlist + remember; ultra is opt-in above the `full` default.

## 14. Rollout

Default `full` ships on (per the brainstorming decision). One PATCH version bump (0.9.x series, per project norm). Implementation order is captured in the follow-on implementation plan (`writing-plans`).
