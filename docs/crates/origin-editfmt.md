# origin-editfmt

> Model-tuned edit-format matrix: parsers and appliers for diff formats.

## Purpose

Different models are reliable with different edit encodings. `origin-editfmt`
parses the common LLM edit formats into one normalized `Hunk` representation,
applies those hunks against original file contents, and exposes a per-model
"best format" table so the daemon can both *ask* a model for the format it
handles best and *parse* whatever it emits. It also builds the
`<origin-edit-format>` system-prompt block that steers prose edits.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `EditFormat` | enum | `SearchReplace` / `DiffFenced` / `WholeFile` / `Udiff`; `label()`, `guidance()`. |
| `Hunk` | struct | Normalized edit `{ file, before, after }` (`before` empty for whole-file). |
| `EditFmtError` | enum | `Parse` / `NoMatch` / `Ambiguous`. |
| `parse` | fn | Parse `text` in an explicit `EditFormat` into hunks. |
| `apply` | fn | Apply a single hunk to original contents (unique-match enforced). |
| `best_format_for` | fn | Map a model name (case-insensitive, prefix) to its best `EditFormat`. |
| `system_block` | fn | Render the `<origin-edit-format>` prompt block for a model. |
| `format_from_text` | fn | Auto-detect which format a block of prose uses. |
| `extract_all_hunks` | fn | Detect-or-fallback parse of every hunk in model output. |

## Key types

```rust
pub enum EditFormat {
    SearchReplace, // <<<<<<< SEARCH / ======= / >>>>>>> REPLACE
    DiffFenced,    // fenced ```diff wrapping SEARCH/REPLACE
    WholeFile,     // full replacement of the file
    Udiff,         // minimal unified diff (--- / +++ / @@)
}

pub struct Hunk {
    pub file: String,
    pub before: String, // empty for whole-file edits
    pub after: String,
}
```

## How it works

`parse(format, text)` dispatches to the per-format parser
(`parse_search_replace`, `parse_whole_file`, `parse_udiff`) and normalizes the
result into `Hunk`s. `apply` replaces the *unique* occurrence of `before` with
`after`, returning `NoMatch` if it is absent or `Ambiguous` if it occurs more
than once (whole-file hunks with empty `before` replace wholesale). The model
matrix is prefix-based and case-insensitive: Claude/Anthropic/sonnet/opus/haiku
→ `SearchReplace`; `gpt-4`/`o1`/`o3` → `Udiff`; `deepseek` → `DiffFenced`;
`gpt-3.5`/`turbo-instruct` → `WholeFile`; unknown → `SearchReplace`.
`extract_all_hunks` first tries `format_from_text`, then falls back to
`best_format_for(model)` so parsing succeeds even when the model deviates.

```
prompt: system_block(model) ─▶ best_format_for ─▶ <origin-edit-format> guidance
model output ─▶ format_from_text ?─▶ best_format_for(model) ─▶ parse ─▶ [Hunk]
Hunk + original ─▶ apply ─▶ new contents | NoMatch | Ambiguous
```

## Why a matrix

Aider's research showed the *same* model is markedly more reliable when asked to
edit in the encoding it was trained to emit, and that the best encoding differs
by model family. This crate captures that as data plus parsers: the daemon calls
`system_block(model)` to *steer* prose edits toward the right format, then
`extract_all_hunks(text, model)` to *robustly parse* whatever comes back —
auto-detecting first and only falling back to the model default. The structured
`Edit`/`MultiEdit`/`ApplyPatch` tools remain the preferred path; this matrix is
the safety net for edits a model writes directly into prose.

## Dependencies & features

`#![forbid(unsafe_code)]`. Only `thiserror` for `EditFmtError`; no serde, no
async, no I/O — purely string-in / string-out logic.

## Used by

```
crates/origin-daemon/Cargo.toml
crates/origin-editfmt/Cargo.toml
```

## Testing

In-file tests pin the model matrix (`claude-3-5-sonnet`/`Opus-4` →
`SearchReplace`, `gpt-4o`/`o3-mini` → `Udiff`) and exercise each parser, the
unique-match `apply` contract (no-match and ambiguous paths), format
auto-detection, and `extract_all_hunks` fallback behaviour.

## See also

- [Tools subsystem](../subsystems/tools.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
