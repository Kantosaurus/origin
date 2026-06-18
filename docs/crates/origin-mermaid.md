# origin-mermaid

> Dependency-free renderer for a useful subset of mermaid flowcharts to ASCII.

## Purpose

`origin-mermaid` parses a small, common subset of mermaid `graph`/`flowchart`
syntax and renders a readable, deterministic ASCII view — pure `std`, no I/O, no
async, no external crates. It brings to `origin` the same self-hosted flowchart
rendering `jcode` ships, so the models' mermaid output can be shown in the
terminal without shelling out to a browser or a JS toolchain.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `parse` | fn | `&str` → `Result<Diagram, MermaidError>`. |
| `render_ascii` | fn | `&Diagram` → deterministic ASCII `String`. |
| `Diagram` | struct | `{ direction, nodes, edges }`. |
| `Direction` | enum | `TopDown` (`TD`/`TB`) / `LeftRight` (`LR`). |
| `Node` | struct | `{ id, label, shape }`. |
| `NodeShape` | enum | `Box` `[…]` / `Round` `(…)` / `Diamond` `{…}`. |
| `Edge` | struct | `{ from, to, label: Option<String> }`. |
| `MermaidError` | enum | `Empty` / `Unsupported(String)`. |

## Key types

```rust
pub struct Diagram {
    pub direction: Direction,
    pub nodes: Vec<Node>,   // first-seen order, deduplicated by id
    pub edges: Vec<Edge>,   // first-seen order
}

pub enum NodeShape { Box, Round, Diamond }   // [label] / (label) / {label}
```

## How it works

```
src ──parse──► Diagram ──render_ascii──► ASCII
```

`parse` trims lines and drops blanks and `%%` comments. The first meaningful
line must be a supported header (`graph`/`flowchart` + `TD`/`TB`/`LR`); a missing
or unknown header yields `MermaidError::Unsupported`, an input with no meaningful
lines yields `MermaidError::Empty`. Remaining lines are fed to an internal
`Builder` that recognises:

- node definitions with labels — `A[Box]`, `B(Round)`, `C{Diamond}`
- edges — `A-->B`, `A--text-->B`, `A---B`

Any unrecognised line (styling, subgraphs, class defs, comments) is skipped
gracefully rather than erroring — this tolerance is deliberate, so a model that
emits a richer diagram than the supported subset still renders the parts that
are understood instead of failing outright. Nodes referenced only by edges are
created implicitly as a default `NodeShape::Box` with `label == id`. The shape
of a node is taken from the first bracket style seen for its id (`[…]`/`(…)`/`{…}`),
and `NodeShape::delims` maps each shape back to its delimiter pair for rendering.
`render_ascii` then
prints a `flowchart TD`/`LR` header, every node as a boxed label, and a layered
adjacency block where each source node's outgoing edges render with `-->` arrows
(and `-- … -->` for labelled edges). Ordering is fully deterministic — nodes in
declaration order, each node's targets in edge-declaration order — so a given
diagram always renders identically.

## Dependencies & features

Zero external dependencies — pure `std`, `#![forbid(unsafe_code)]`. No Cargo
features. The CLI surfaces this crate as `origin mermaid <path|->`.

## Used by

`Grep "origin-mermaid" glob "crates/*/Cargo.toml"`:

- `crates/origin-cli/Cargo.toml`
- `crates/origin-mermaid/Cargo.toml` (self)

## Testing

Inline `#[cfg(test)]` unit tests and the module doctest exercise the
parse→render round-trip: header/direction parsing, the three node shapes, plain
and labelled edges, implicit-node creation for edge-only references, graceful
skipping of unrecognised lines, and the `Empty` / `Unsupported` error arms. The
doctest asserts a rendered diagram contains both a node label (`Start`) and the
`-->` arrow marker.

## See also

- [tui-and-cli subsystem](../subsystems/tui-and-cli.md)
- [crates index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
