# origin-watch

> Editor-agnostic watcher that scans source files for AI-trigger comments.

## Purpose

`origin-watch` scans source trees for inline AI-trigger comments such as
`// AI: ...`, `# AI! ...`, or `-- AI? ...` and reports them as actionable items.
It mirrors the `aider --watch-files` workflow without depending on any editor:
detection is pure line parsing, and the only I/O is walking a directory. The
daemon polls it to discover instructions the user dropped directly into code.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `AiKind` | enum | `Ai` (general) / `Bang` (`AI!`, act now) / `Question` (`AI?`). |
| `AiComment` | struct | `{ file, line, kind, text }` (1-based line). |
| `ScanConfig` | struct | `{ root, extensions }`. |
| `WatchError` | enum | `Io(String)`. |
| `parse_line` | fn | Detect a trailing AI marker in one line → `Option<(AiKind, String)>`. |
| `scan_text` | fn | Scan in-memory file contents (pure, no I/O). |
| `scan_dir` | fn | Walk a tree and scan matching files. |

## Key types

```rust
pub enum AiKind { Ai, Bang, Question }

pub struct AiComment {
    pub file: String,
    pub line: u32,     // 1-based
    pub kind: AiKind,
    pub text: String,  // instruction after the marker
}

pub struct ScanConfig { pub root: String, pub extensions: Vec<String> }
```

## How it works

`parse_line` tries each recognized comment leader (`//`, `--`, `#`, `;`, `%`,
longest-first) and, after a leader, looks for the bare token `AI`, `AI!`, or
`AI?`. The character directly after `AI` decides the kind and must not be an
identifier character, so `AID`/`AISLE` never match. The remainder is trimmed of a
leading `:` or `-` to yield the instruction text. `scan_text` runs that over
every line, emitting one `AiComment` per hit with a 1-based line number.
`scan_dir` walks `root` with `walkdir`, keeps files whose extension matches
`ScanConfig::extensions` (case-insensitive), reads them lossily as UTF-8, and
skips unreadable files rather than aborting the whole scan.

```
ScanConfig{root, extensions} ─▶ scan_dir ─(walkdir)─▶ matching files
each file ─▶ scan_text ─(per line)─▶ parse_line ─▶ AiComment{file,line,kind,text}
// AI: rename  → Ai      # AI! fix now → Bang      -- AI? why?  → Question
```

## Marker grammar

A marker is `<leader> AI[!?] [:|-]? <instruction>`. The leader is one of `//`,
`--`, `#`, `;`, `%` (tried longest-first so `--` wins over a single `-`), and the
search retries at every leader occurrence on a line, so a marker that trails real
code is still found. The token must be exactly `AI`, `AI!`, or `AI?`; the kind is
decided by the character immediately after `AI`, which must not be alphanumeric
or `_`. This is what lets ordinary identifiers like `AID` or `AISLE` pass through
untouched while `// AI!` is flagged as `Bang`.

## Dependencies & features

`#![forbid(unsafe_code)]`. `serde` (derive on the result/config types), `walkdir`
(tree traversal), and `thiserror` (`WatchError`). Detection itself is pure; only
`scan_dir` touches the filesystem. Dev-dep `tempfile` builds scan fixtures.

## Used by

```
crates/origin-cli/Cargo.toml
crates/origin-watch/Cargo.toml
```

## Testing

In-file tests cover every leader/kind combination, the identifier guard
(`AID`/`AISLE` not matched), colon/dash separator trimming, multi-line
`scan_text` line numbering, and a `tempfile`-backed `scan_dir` that confirms
extension filtering and graceful skipping of unreadable files.

## See also

- [Tools subsystem](../subsystems/tools.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
