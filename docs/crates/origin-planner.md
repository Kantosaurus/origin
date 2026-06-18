# origin-planner

> Predictive prompt-cache prefix planner with stability bands and a prefix ledger.

## Purpose

`origin-planner` decides how to lay out an outgoing model request so the
provider's prompt cache hits as often as possible. It sorts request sections into
four stability **bands** (Frozen → Sticky → Sliding → Volatile), emits cache
markers at every adjacent-band boundary, and tracks each section's empirical
stability in a **prefix ledger** that promotes or demotes sections over time. A
companion `WireDecision` rule chooses, per tool-result block, whether to inline
its bytes or emit a compact CAS-handle reference.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `Band` | enum | `Frozen` / `Sticky` / `Sliding` / `Volatile` (ordered `0..3`). |
| `Band::promoted` / `demoted` | fn | One step toward Frozen / toward Volatile. |
| `CachePlanner` | struct | Sorts `Section`s into band order and emits markers. |
| `Plan` | struct | `ordered_sections()` + `marker_indices()`; shared handle/marker state. |
| `Section` | struct | `{ id, band, bytes }` — one contiguous request portion. |
| `PrefixLedger` | struct | Per-section running stability table. |
| `SectionId` | struct | Opaque `&'static str` section identifier. |
| `Stability` | struct | `{ score, band }` running score for one section. |
| `LedgerError` | enum | `Unknown(&'static str)`. |
| `WireDecision` | enum | `Inline` / `Reference`; `for_block(band, byte_len)`. |
| `INLINE_BYTE_BUDGET` | const | `2048` — inline budget for non-Frozen/Sticky bands. |

## Key types

```rust
#[repr(u8)]
pub enum Band { Frozen = 0, Sticky = 1, Sliding = 2, Volatile = 3 }

pub const PROMOTE_THRESHOLD: i32 = 3;   // score ≥ ⇒ promote toward Frozen
pub const DEMOTE_THRESHOLD: i32 = -2;   // score ≤ ⇒ demote toward Volatile

pub enum WireDecision { Inline, Reference }
impl WireDecision {
    pub const fn for_block(band: Band, byte_len: usize) -> Self {
        match band {
            Band::Frozen | Band::Sticky => Self::Inline,            // always inline; amortized
            Band::Sliding | Band::Volatile =>
                if byte_len <= INLINE_BYTE_BUDGET { Self::Inline } else { Self::Reference },
        }
    }
}
```

## How it works

```
sections ──CachePlanner::plan──► Plan { ordered (Frozen→Volatile), markers @ band edges }
                                        │
  PrefixLedger.record_hit / record_miss│   per-section score
            score ≥ +3 ⇒ band.promoted()│   score ≤ -2 ⇒ band.demoted()
```

`CachePlanner::plan` orders sections by band and places a cache marker after each
section at an adjacent-band boundary (`marker_indices()[i]` ⇒ "emit a marker
after `ordered_sections()[i]`"). Volatile content always lands last because it is
most likely to change between turns, maximising the stable cached prefix.

The `PrefixLedger` scores each `(section_id, band)` with `record_hit` (positive)
and `record_miss` (negative); crossing `PROMOTE_THRESHOLD` promotes the section
one band toward Frozen, crossing `DEMOTE_THRESHOLD` demotes it one band toward
Volatile — so sections that prove stable migrate into the cached prefix and
churny ones drift out.

`Plan` is cheaply cloneable and carries two `Arc<RwLock<…>>` slots —
`handle_bands` (CAS-handle → `Band`) and `dynamic_message_markers` — so the
daemon's tool-result dispatch (writer) and the provider's wire-encoder (reader)
hold separate `Plan` clones that **share** the same interior-mutable state: a
write on one side is immediately visible to the other without an explicit
channel. The wire-encoder then calls `WireDecision::for_block` per tool-result:
Frozen/Sticky always inline (their bytes amortize across many cache-hitting
turns), Sliding/Volatile inline only when `byte_len ≤ INLINE_BYTE_BUDGET` (2048),
otherwise emitting a `<result handle:… — N bytes>` reference the model can
inflate via `Recall`. Competing stacks re-serialize full tool-result bytes every
turn; the per-handle band map is the novel demotion mechanism.

## Dependencies & features

`thiserror` only. No async, no I/O. `proptest` is a dev-dependency. No Cargo
features.

## Used by

`Grep "origin-planner" glob "crates/*/Cargo.toml"`:

- `crates/origin-daemon/Cargo.toml`
- `crates/origin-planner/Cargo.toml` (self)
- `crates/origin-provider-anthropic/Cargo.toml`
- `crates/origin-swarm/Cargo.toml`

## Testing

`proptest` drives randomized hit/miss sequences to verify promotion/demotion
monotonicity and that the ledger never moves a section past `Frozen`/`Volatile`.
Inline unit tests cover `Band::promoted`/`demoted` boundaries, marker placement
at band edges, `Plan` clone-shares-state equality, and every `WireDecision::for_block`
arm around the `INLINE_BYTE_BUDGET` threshold.

## See also

- [tui-and-cli subsystem](../subsystems/tui-and-cli.md)
- [runtime-and-concurrency architecture](../architecture/runtime-and-concurrency.md)
- [crates index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
