# origin-cmdparse

> Bash command-line safety analysis that hardens the permission gate against known bypass classes.

## Purpose

`origin-cmdparse` is pure string analysis (std + `thiserror`, no I/O, no async)
that the permission gate runs inline before auto-approving a bash invocation. A
name-based allowlist alone is fooled by several well-known bypass shapes; this
crate detects them and downgrades or blocks auto-approval. Every observation
carries a human-readable reason the gate can surface to the user.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `analyze` | fn | Run every detector over a line; returns an `Analysis` (infallible). |
| `worst` | fn | Pick the most severe `Risk` (`Dangerous` > `Suspicious` > `Safe`). |
| `split_commands` | fn | Split a line on `;`, `&&`, `\|\|`, `\|`, newlines — quote-aware. |
| `Risk` | enum | `Safe`, `Suspicious(String)`, `Dangerous(String)`. |
| `Analysis` | struct | `{ risks: Vec<Risk>, commands: Vec<String> }`. |
| `CmdParseError` | enum | `Empty` (reserved typed-error for empty input). |

## Key types

```rust
pub enum Risk {
    Safe,
    Suspicious(String), // downgrade auto-approval to explicit confirmation
    Dangerous(String),  // block auto-approval outright
}

pub struct Analysis {
    pub risks: Vec<Risk>,
    pub commands: Vec<String>,
}

pub fn analyze(line: &str) -> Analysis;            // always succeeds
pub fn worst(a: &Analysis) -> Risk;                // Dangerous > Suspicious > Safe
pub fn split_commands(line: &str) -> Vec<String>;  // never splits inside quotes
```

## How it works

`analyze` splits the line into top-level commands (quote-aware), then runs two
families of detectors and returns the collected risks (a lone `Risk::Safe` when
nothing fires). `worst` ranks by `Risk::rank()`.

```text
analyze(line)
  ├─ whole-line (lowercased) shapes
  │    pipe-to-shell        curl|wget … | sh/bash/zsh/python/…   → Dangerous
  │    base64-to-shell      base64 -d … | sh                     → Dangerous
  │    archive-exfil        tar/zip broad-tree | curl/nc/ssh     → Dangerous
  │    secret-then-network  .ssh/id_rsa/.env/.aws + curl/wget    → Dangerous
  │    fork-bomb            :(){ :|:& };:                         → Dangerous
  └─ per-command shapes
       rm -rf home/root     rm -rf ~ / $HOME / ${HOME} / "/"     → Dangerous
       cd escape (multi)    `cd <dir>` before a later command    → Suspicious
       bare env prefix      `NAME=val realcmd`                    → Suspicious
```

The detectors are bypass-hardened: `rm` is matched by its **bare program name**
after stripping env-assignment prefixes (`FOO=bar`), wrapper commands
(`sudo`, `doas`, `env`, `command`, `exec`, `nice`, `xargs`, …), absolute/relative
paths (`/bin/rm`, `./rm`), and the alias-suppressing leading backslash (`\rm`).
Recursive+force intent is OR-ed across separate flag tokens (`-r -f`,
`--recursive --force`, `--no-preserve-root`) as well as the `-rf` bundle. The
home/root target test treats `~/`, `$HOME/`, and `${HOME}` (with a trailing
slash) as the whole home tree.

## Dependencies & features

- `thiserror` only. No cargo features, no async, no I/O — `#![forbid(unsafe_code)]`.

## Used by

`crates/*/Cargo.toml` matches for `origin-cmdparse`:

- `crates/origin-cmdparse/Cargo.toml`
- `crates/origin-daemon/Cargo.toml`

## Testing

All tests are in-file (`#[cfg(test)] mod tests` in `lib.rs`). They cover
quote-aware splitting, every detector class, the wrapper/path/backslash
`rm -rf` bypass variants, `worst` ordering, and that plain commands (`ls -la`)
stay `Safe`.

## See also

- [Security model](../security/security-model.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
