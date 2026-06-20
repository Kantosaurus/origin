# origin-ponytail

Native ponytail: an always-on "lazy senior dev" ruleset injected into the system
prompt, plus a deterministic, table-driven dependency gate at tool-dispatch time.
A pure library crate (no daemon deps); the daemon and CLI wire it in.

## Modes

`off / lite / full (default) / ultra` — set via `/ponytail [level]`, the
`--ponytail` flag, or `PONYTAIL_DEFAULT_MODE` / `ORIGIN_PONYTAIL`. Resolution:
per-request token → `~/.origin/ponytail.toml` `defaultMode` → env → `Full`.

| mode | injected ruleset | dependency gate |
|------|------------------|-----------------|
| off | none | none |
| lite | lite intensity | advisory only (logs, never blocks) |
| full | full intensity | blocks deps that have a native/stdlib replacement |
| ultra | extremist intensity | challenges **every** new dependency (rung 4) |

## The dependency gate

Runs in the daemon's pre-dispatch filter (after permission/governance allow,
before the tool executes) for `Edit`/`Write`/`MultiEdit`/`ApplyPatch`/`Bash`.

1. `detect` extracts newly-added deps from manifest edits (package.json,
   Cargo.toml, requirements.txt, pyproject.toml, go.mod, Gemfile) and
   package-manager Bash commands (`npm/yarn/pnpm/bun add`, `cargo add`,
   `pip install`, `go get`, `gem install`, `poetry add`, `uv pip install`).
   Installing existing manifest deps (bare `npm install`, `pip install -r`,
   `cargo build`) adds nothing and is never flagged.
2. `gate::classify` checks each against the ported platform-native table
   (`native_table`). `full`/`lite` flag only deps with a native replacement;
   `ultra` flags every new dep. Allowlisted deps are skipped.
3. The daemon acts on the flags: **interactive** sessions get a
   `[Allow once · Allow & remember · Deny]` prompt (reusing the `ask_user`
   choice channel); "remember" appends to the allowlist. **Non-interactive**
   (subagent/swarm/headless/scheduled) sessions allow + log. A Deny surfaces a
   recoverable tool-result error so the model rewrites against the native API.

The gate is deterministic (no LLM, microsecond cost) and **fails open**: any
parse uncertainty yields no flag, never a broken or wrongly-blocked write.

## Files & config

- `~/.origin/ponytail.toml` — `defaultMode`, `allow = [...]` (the dep allowlist).
- `~/.origin/ponytail-debt.jsonl` — append-only ledger of advisories/overrides.

## Commands

`/ponytail [off|lite|full|ultra]`, `/ponytail-review` (over-engineering review of
the working diff, auto-run), `/ponytail-audit` (whole-repo), `/ponytail-debt`,
`/ponytail-gain`, `/ponytail-help`.

## Design

See the spec: `docs/superpowers/specs/2026-06-20-ponytail-native-design.md` and
the plan: `docs/superpowers/plans/2026-06-20-ponytail-native.md`.
