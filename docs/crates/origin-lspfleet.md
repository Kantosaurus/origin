# origin-lspfleet

> Registry and auto-install decisioning for 40+ language servers, plus diagnostic aggregation.

## Purpose

`origin-lspfleet` is the static knowledge base that maps a source file (by
extension) or a language name to the language server that should handle it,
along with the shell command to install it and the command to launch it in
stdio mode. It also provides pure helpers to aggregate (sort + dedup) and
summarize diagnostics. The crate performs no I/O: the daemon downloads and
spawns servers (driving `origin-lsp-client`) using the data exposed here.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `LspServer` | struct | `{ language, server_id, install, launch, extensions }`; `handles_extension(ext)`. |
| `Severity` | enum | Ordered `Error` > `Warning` > `Info` > `Hint`. |
| `Diagnostic` | struct | `{ file, line, col, severity, message, source }` (1-based line/col). |
| `server_for_extension` | fn | Lookup the server claiming a file extension. |
| `server_for_language` | fn | Lookup the server for a language name. |
| `aggregate` | fn | Sort + deduplicate a `Vec<Diagnostic>`. |
| `summary` | fn | Returns `(errors, warnings)` counts. |

## Key types

```rust
pub struct LspServer {
    pub language: &'static str,     // "rust"
    pub server_id: &'static str,    // "rust-analyzer"
    pub install: &'static str,      // "rustup component add rust-analyzer"
    pub launch: &'static str,       // "rust-analyzer" (split on spaces for argv)
    pub extensions: &'static [&'static str], // ["rs"]
}

pub enum Severity { Error, Warning, Info, Hint } // Ord: most→least severe
```

## How it works

A single `static REGISTRY: &[LspServer]` holds 44 entries spanning Rust, Go,
Python (`pyright-langserver --stdio`), TypeScript/JS, C/C++ (`clangd`), Java,
and dozens more. `server_for_extension`/`server_for_language` are linear scans
over that table; `handles_extension` matches case-insensitively. Launch strings
that need flags (`pyright-langserver --stdio`, `solargraph stdio`, …) are stored
whole so the daemon can split them into program + argv and route to
`origin-lsp-client::spawn_with_args`; argv-free servers like `rust-analyzer`
just spawn. `aggregate` sorts diagnostics (file, then position, then the `Ord`
on `Severity`) and removes exact duplicates; `summary` counts errors vs warnings
for a compact post-edit banner.

```
file.ext ─▶ server_for_extension ─▶ LspServer{ install, launch }
                                          │ daemon splits `launch`
                                          ▼
                              origin-lsp-client::spawn_with_args
raw diags ─▶ aggregate (sort+dedup) ─▶ summary ─▶ (errors, warnings)
```

## Registry coverage

The table currently holds 44 entries. A representative slice, with the launch
string the daemon splits into program + argv:

| Language | server_id | launch |
| --- | --- | --- |
| rust | `rust-analyzer` | `rust-analyzer` |
| go | `gopls` | `gopls` |
| python | `pyright` | `pyright-langserver --stdio` |
| typescript | `typescript-language-server` | `typescript-language-server --stdio` |
| c | `clangd` | `clangd` |
| cpp | `clangd-cpp` | `clangd` |
| java | `jdtls` | `jdtls` |

Entries store the *install* command too (`rustup component add rust-analyzer`,
`npm install -g pyright`, `go install …gopls@latest`, …) so a daemon that finds
a server missing can offer to provision it.

## Dependencies & features

`#![forbid(unsafe_code)]` and dependency-free — the crate is pure const data and
small pure functions, which keeps it trivially testable and cheap to compile.

## Used by

```
crates/origin-cli/Cargo.toml
crates/origin-daemon/Cargo.toml
crates/origin-lspfleet/Cargo.toml
```

## Testing

In-file tests assert extension and language lookups resolve to the expected
`server_id`, that `handles_extension` is case-insensitive, that `aggregate`
sorts and deduplicates deterministically, and that `summary` returns correct
error/warning counts. Because there is no I/O, the suite runs in milliseconds.

## See also

- [Tools subsystem](../subsystems/tools.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
