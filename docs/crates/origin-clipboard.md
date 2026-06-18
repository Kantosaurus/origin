# origin-clipboard

> Copy/paste web-chat mode: format context to paste and parse pasted edits.

## Purpose

`origin-clipboard` powers a "bring your own browser chat" workflow: it formats a
bundle of files plus an instruction into a clean, prompt-ready block to paste
into any web chat, then parses the model's pasted reply back into structured
file edits. The crate is pure logic — reading and writing the OS clipboard is
left to the caller, which can use `os_copy_command` / `os_paste_command` to learn
the right shell program for the current platform.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `ContextBundle` | struct | `{ files: Vec<(path, contents)>, instruction }`; `new(...)`. |
| `EditBlock` | enum | `SearchReplace { file, search, replace }` / `WholeFile { file, contents }`. |
| `ClipboardError` | enum | `Malformed(String)`. |
| `format_for_paste` | fn | Render a bundle to a deterministic paste block. |
| `parse_pasted_edits` | fn | Parse a pasted reply into `Vec<EditBlock>`. |
| `os_copy_command` | fn | `(program, args)` that reads stdin onto the clipboard. |
| `os_paste_command` | fn | `(program, args)` that prints the clipboard to stdout. |

## Key types

```rust
pub struct ContextBundle {
    pub files: Vec<(String, String)>, // (path, contents)
    pub instruction: String,
}

pub enum EditBlock {
    SearchReplace { file: String, search: String, replace: String },
    WholeFile     { file: String, contents: String },
}
```

## How it works

`format_for_paste` renders each file as `File: <path>` followed by a fenced code
block (with a language hint from the extension), then appends the instruction —
deterministic output so it can be diffed and tested. `parse_pasted_edits` scans
the reply for the same fence markers and the aider-style
`<<<<<<< SEARCH` / `=======` / `>>>>>>> REPLACE` triple, emitting a
`SearchReplace` per block or a `WholeFile` when the reply supplies whole
contents. The OS command helpers select `pbcopy`/`pbpaste` on macOS,
`clip`/`powershell -Command Get-Clipboard` on Windows, and
`xclip -selection clipboard` elsewhere — the caller pipes through them.

```
files + instruction ─▶ format_for_paste ─▶ paste block ─(os_copy_command)─▶ web chat
web chat reply ─(os_paste_command)─▶ text ─▶ parse_pasted_edits ─▶ [EditBlock]
```

## Why pure logic

Keeping the clipboard I/O out of the crate has two payoffs. First, the format
and the parser are fully deterministic and unit-testable — the paste block can
be byte-for-byte asserted, and round-trips (format → parse) are checked without
ever touching a real clipboard. Second, the caller stays in control of *how* it
talks to the OS: a TUI might pipe through the `os_*_command` programs, while a
headless test can feed strings directly. The crate just supplies the contract.

## Dependencies & features

`#![forbid(unsafe_code)]`. Only `thiserror` for `ClipboardError`. No clipboard
crate, no subprocess, no async — platform selection is `cfg!`-based and the
actual exec is the caller's job.

## Used by

```
crates/origin-cli/Cargo.toml
crates/origin-clipboard/Cargo.toml
```

## Testing

In-file tests assert `format_for_paste` produces the exact expected block
(fences, language hint, trailing instruction) and that `parse_pasted_edits`
recovers both `SearchReplace` and `WholeFile` edits, including malformed-block
handling. The `os_*_command` selectors are checked per target via `cfg!`.

## See also

- [Tools subsystem](../subsystems/tools.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
