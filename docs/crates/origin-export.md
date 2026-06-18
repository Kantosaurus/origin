# origin-export

> Conversation transcript export to clean Markdown or pretty JSON

## Purpose

`origin-export` renders an origin session into a portable artifact a user can
read, diff, or hand to a teammate (mirroring openclaude's `/export` and
opencode's local share). It produces either a clean Markdown transcript with a
YAML-ish front-matter header and per-turn role headings, or a pretty-printed
JSON document. The crate is pure logic plus serde — no I/O, no async, no platform
concerns — and is `#![forbid(unsafe_code)]`.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `ExportTurn` | struct | `{ role, text, tools }` for one turn. |
| `ExportSession` | struct | `{ id, title, provider, model, created_at_unix_ms, turns }`. |
| `to_markdown(&ExportSession)` | fn → `String` | Infallible Markdown transcript. |
| `to_json(&ExportSession)` | fn → `Result<String, ExportError>` | Pretty JSON. |
| `ExportError` | enum | `Serialize(String)`. |

## Key types

```rust
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportSession {
    pub id: String,
    pub title: Option<String>,
    pub provider: String,
    pub model: String,
    pub created_at_unix_ms: u64,
    pub turns: Vec<ExportTurn>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportTurn {
    pub role: String,
    pub text: String,
    pub tools: Vec<String>,
}
```

## How it works

`to_markdown` emits a `---`-delimited front-matter block (title, id, provider,
model, `created_at_unix_ms`, turn count), a document `# title`, then one
`## Role` section per turn:

~~~markdown
---
title: Refactor the parser
id: sess-abc
provider: anthropic
model: claude-sonnet-4-6
created_at_unix_ms: 1700000000000
turns: 2
---

# Refactor the parser

## User

Please refactor the tokenizer.

## Assistant

Done. I edited two files.

**Tools**

```
read_file
edit_file
```
~~~

The title falls back to the session `id` when absent. Each role is capitalized
for its heading (an empty role becomes `Turn`). A turn whose text is empty after
trimming renders the `_(no content)_` placeholder; tool names render only when
present, in a fenced code block. Because every field is a plain string or number,
the rendering never fails — writes into the `String` are infallible — so only
`to_json` returns a `Result` (`ExportError::Serialize` on a serde failure).

## Dependencies & features

- `serde` / `serde_json` — session serde + pretty JSON.
- `thiserror` — `ExportError`.
- `#![forbid(unsafe_code)]`; no cargo features.

## Used by

`Grep "origin-export" glob "crates/*/Cargo.toml"`:

- `crates/origin-cli/Cargo.toml`
- `crates/origin-daemon/Cargo.toml`
- `crates/origin-export/Cargo.toml` (self)

## Testing

Inline tests verify: JSON round-trip and pretty (indented, multi-line) output;
Markdown containing the title, model, and role headings plus the turn text; the
tool list rendered inside a code fence (and omitted when no tools); the title
falling back to the id when absent; an empty session still rendering a header
(`turns: 0`) and round-tripping through JSON; an empty/whitespace turn showing
`_(no content)_`; and `heading_for` handling empty/`user`/`assistant` roles.

## See also

- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
