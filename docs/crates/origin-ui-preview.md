# origin-ui-preview

> Hot-reload terminal preview of the origin harness UI/UX (themes, palette, ANSI chrome).

## Purpose

`origin-ui-preview` is a tiny binary that renders `origin`'s terminal identity —
the "Burnished Copper" palette and the other `/theme` presets — as colour
swatches plus a mock transcript, so design changes to the harness chrome can be
eyeballed instantly. It exists purely to keep the edit → rebuild → re-render loop
in the ~1-second range while iterating on look-and-feel, without launching the
full TUI or a daemon.

## Public API surface

This is a binary crate (`[[bin]] origin-ui-preview`), not a library — there is no
public API surface. Its distinguishing trait is **how it is built**, not what it
exports:

```toml
# Zero dependencies on purpose: this crate compiles only
# origin-cli/src/theme.rs + ansi.rs (via #[path] includes) so the
# edit -> rebuild -> rerender loop stays in the ~1 second range.
```

```rust
#[path = "../../origin-cli/src/theme.rs"]
mod theme;
#[path = "../../origin-cli/src/ansi.rs"]
mod ansi;
```

Rather than depending on `origin-cli`, it pulls in just two source files by
`#[path]`, so a rebuild compiles two files instead of the entire harness.

## Key types

The preview reuses `origin-cli`'s own `Theme` / `Palette` types directly through
the `#[path]` include:

```rust
use theme::{palette, Palette, Theme};

fn fg(c: u32) -> String { /* 24-bit \x1b[38;2;R;G;Bm */ }
fn bg(c: u32) -> String { /* 24-bit \x1b[48;2;R;G;Bm */ }
fn swatch(name: &str, c: u32) -> String { /* colored block + field name + hex */ }
```

`print_swatch_grid` walks the 22 named palette fields (`surface`, `accent`,
`user`, `tool`, `code_fg`, `green`/`yellow`/`red`, `rule`, …) and prints each as
a labelled swatch.

## How it works

```
origin-ui-preview [theme] [--swatches|--transcript]
   │
   ├─ no arg          → render every theme preset
   ├─ <theme>         → render just that preset
   ├─ --swatches      → palette grid only
   └─ --transcript    → mock transcript only
```

Each theme is rendered as a 24-bit-truecolor palette grid plus a fake
conversation, so the colours and ANSI chrome appear exactly as they would in the
live TUI. Hot reload is driven by an external watcher rather than anything baked
into the binary; the module doc lists three options:

```
cargo watch -x 'run -p origin-ui-preview'   # if cargo-watch installed
bacon run -- -p origin-ui-preview           # if bacon installed
./scripts/ui-preview-watch.ps1              # zero-install fallback
```

`#![allow(dead_code)]` is set because the included `theme.rs`/`ansi.rs` are full
libraries for `origin-cli`; the preview only exercises a subset.

## Dependencies & features

Zero dependencies by design (see the Cargo comment above). `[lints] workspace =
true`. No Cargo features. Builds only the two `#[path]`-included files plus its
own `main.rs`.

## Used by

`Grep "origin-ui-preview" glob "crates/*/Cargo.toml"`:

- `crates/origin-ui-preview/Cargo.toml` (self)

It is a developer tool — nothing depends on it; it depends (by file include, not
by Cargo) on `origin-cli`'s theme/ansi modules.

## Testing

The crate has no test harness of its own — it is a visual tool whose "test" is
the rendered output. The theme/ansi logic it includes is covered by the unit
tests in `origin-cli` (where `theme.rs` and `ansi.rs` live).

## See also

- [tui-and-cli subsystem](../subsystems/tui-and-cli.md)
- [crates index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
