# origin-migrate

> Migrate sessions, skills, and memories from other harnesses into origin

## Purpose

`origin-migrate` imports artifacts from other coding-agent harnesses — Claude
Code, jcode, opencode, Codex CLI, and `pi` — into origin's own store, and can
*reconstruct* an in-flight transcript into origin's native message model so a
session can be continued (live resume), not just archived. Each harness has a
`Source` adapter that scans a root directory into a normalized `MigrateBundle`;
a `sink` applies that bundle through `origin-store` with content-hash dedupe so
re-running `origin import` is idempotent.

## Public API surface

| Item | Module | Summary |
| --- | --- | --- |
| `Source` (trait) | `source` | `name()` + `scan(root) -> MigrateBundle`. |
| `ImportedSession` / `ImportedMessage` / `ImportedSkill` / `ImportedMemory` | `source` | Normalized importable artifacts. |
| `MigrateBundle` | `source` | `{ sessions, skills, memories }`. |
| `SourceError` | `source` | `Io` / `Parse { path, reason }` / `NotFound`. |
| `ClaudeCodeSource` / `JcodeSource` / `OpencodeSource` / `CodexSource` / `PiSource` | per-module | One adapter per harness. |
| `summarize(&bundle)` / `apply(&bundle)` | `sink` | Pure dry-run `ApplyReport`. |
| `apply_with_store(&store, &bundle)` | `sink` | Idempotent persist with blake3 dedupe. |
| `ApplyReport` | `sink` | Inserted / skipped-duplicate counts. |
| `SourceKind` | `reconstruct` | `ClaudeCode`/`Jcode`/`Opencode`/`Codex`/`Pi` (+ `as_str`/`from_tag`). |
| `ResumedSession` | `reconstruct` | Native `Message`s + suggested model + provenance. |
| `reconstruct` / `reconstruct_session` / `from_*` | `reconstruct` | Adapt an `ImportedSession` for live resume. |
| `reconstruct_from_path(kind, path, model)` | `reconstruct` | Scan a root/file and reconstruct the first session. |
| `suggest_model(Option<&str>)` | `reconstruct` | External model id → origin-catalog id (`claude-fable-5` fallback). |

## Key types

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportedSession {
    pub source_id: String,
    pub title: Option<String>,
    pub created_at_unix_ms: u64,
    pub messages: Vec<ImportedMessage>,
}

#[derive(Debug, Clone)]
pub struct ResumedSession {
    pub messages: Vec<Message>,        // origin_core native model
    pub suggested_model: String,       // never empty
    pub source_kind: SourceKind,
    pub original_id: String,
}
```

## How it works

Each adapter parses permissively — a line/record that fails to deserialize is
logged and skipped rather than aborting the whole scan (Codex/Pi walk
`sessions/**/*.jsonl`, Claude Code reads `projects/*.jsonl`, jcode opens
`sessions.sqlite`, opencode flattens `storage/*.json`).

```
harness root ─► Source::scan ─► MigrateBundle ─┬─► sink::apply_with_store
                                               │      blake3(length-framed) key
                                               │      contains_*? skip : insert
                                               └─► reconstruct::reconstruct
                                                      map_role + Block::text
                                                      suggest_model → ResumedSession
```

`apply_with_store` derives each dedupe key with **length-framed** blake3 over
the artifact's identifying fields (session: `source_id` + each message
role/body; skill: name + body; memory: kind + body + tags), so two distinct
artifacts can never collide into a false "duplicate". The reconstruction path
funnels every harness through one `reconstruct_session` core: roles are mapped
case-insensitively (unknown → `User`), each message becomes one `Block::Text`
(empty bodies preserved so turn boundaries survive), and `suggest_model` resolves
the external model id via an ordered substring table (`opus`/`haiku`/`sonnet`/
`gpt-5`/`codex`/`gemini`/…) to an origin-catalog id, falling back to
`DEFAULT_SUGGESTED_MODEL` (`claude-fable-5`). `reconstruct_from_path` accepts
either the harness root or a single transcript file, walking up to three
ancestor directories to find the scannable root.

## Dependencies & features

- `origin-core` (native `Message`/`Block`/`Role`), `origin-store` (persist),
  `origin-skills`, `origin-mem` — workspace artifact crates.
- `walkdir` / `globset` — directory traversal; `rusqlite` (bundled) — jcode's
  SQLite store; `blake3` — content-hash dedupe; `serde`/`serde_json`,
  `thiserror`, `tracing`.
- `#![forbid(unsafe_code)]`; no cargo features. Dev: `tempfile`.

## Used by

`Grep "origin-migrate" glob "crates/*/Cargo.toml"`:

- `crates/origin-cli/Cargo.toml`
- `crates/origin-daemon/Cargo.toml`
- `crates/origin-migrate/Cargo.toml` (self)

## Testing

`tests/` holds per-harness fixtures (`codex_fixture.rs`, `opencode_fixture.rs`,
`claude_code_fixture.rs`, `jcode_fixture.rs`), a cross-harness `three_paths.rs`,
and a `sink.rs` dedupe test. Inline `reconstruct` tests cover role/order mapping
per harness, the model-remap table + fallback, empty-body turn preservation,
`SourceKind` tag round-trips, unified dispatch parity, and
`reconstruct_from_path` resolution from both a root and a transcript file (plus
`NotFound` for a missing path).

## See also

- [Migration guide](../guides/migration.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
