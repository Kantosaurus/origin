# origin-review

> Multi-agent confidence-scored review aggregation + issue auto-triage

## Purpose

`origin-review` is the pure decision layer over the output of origin's review
agents (bug hunter, security, type-design, simplifier) and adversarial
verifiers. It merges overlapping findings, scores and ranks them under a chosen
strictness, gates low-trust claims by adversarial vote, and separately triages
freeform issue text into a label using a keyword classifier plus a token-Jaccard
similarity helper for duplicate detection. No I/O, no async, no model calls — it
is `#![forbid(unsafe_code)]`.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `Dimension` | enum | Review lens: `Bug`/`Security`/`TypeDesign`/`Test`/`Simplification`/`Performance`/`Style`. |
| `Finding` | struct | One observation `{ dimension, file, line, title, detail, confidence }`. |
| `Finding::new(...)` | fn | Construct, clamping confidence to `[0,1]` (NaN → 0). |
| `Strictness` | enum | `Strict` (0.8) / `Balanced` (0.5) / `Lenient` (0.2). |
| `Strictness::threshold()` | fn → `f32` | Minimum confidence to surface. |
| `dedup(Vec<Finding>)` | fn → `Vec<Finding>` | Merge `(file,line,title)` keeping max confidence. |
| `filter(&[Finding], Strictness)` | fn → `Vec<Finding>` | Keep ≥ threshold, sorted confidence desc. |
| `vote(&[bool])` | fn → `bool` | Strict majority (empty/tie fail-closed). |
| `IssueLabel` | enum | `Bug`/`Feature`/`Question`/`Docs`/`Duplicate`. |
| `triage(title, body)` | fn → `IssueLabel` | Keyword classifier (title weighs double). |
| `similarity(a, b)` | fn → `f32` | Token-set Jaccard in `[0,1]`. |

## Key types

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    pub dimension: Dimension,
    pub file: String,
    pub line: u32,
    pub title: String,
    pub detail: String,
    pub confidence: f32,  // clamped [0.0, 1.0]
}

pub enum Strictness { Strict, Balanced, Lenient } // 0.8 / 0.5 / 0.2
```

## How it works

The aggregation pipeline is `dedup → filter → vote`:

```
raw findings ─► dedup()  merge (file,line,title), keep max confidence
                         first-seen order preserved (deterministic)
             ─► filter(strictness)  drop < threshold, sort confidence desc
             ─► vote(verdicts)  confirm only on strict majority (fail-closed)
```

`dedup` records first-seen slot order so output is deterministic and the
surviving finding keeps the strongest confidence (and that finding's
dimension/detail). `filter` keeps findings meeting the strictness threshold,
sorting confidence-descending with stable ties. `vote` confirms a finding only
when strictly more than half of the adversarial panel agrees — an empty panel or
an exact tie returns `false`.

Triage is independent: `triage` tokenizes title + body (lowercase alphanumeric),
scores each candidate label against small keyword tables with title matches
counted double, and returns the highest scorer — defaulting to `Question` on a
tie or no signal. `Duplicate` is never produced by `triage`; duplicate detection
uses `similarity`, the token-set Jaccard ratio (`1.0` for identical sets, `0.0`
for disjoint; two empty strings are `1.0`).

## Dependencies & features

- `serde` (with `derive`) — `Dimension` / `Finding` / `Strictness` /
  `IssueLabel` serialize as stable `snake_case` for daemon JSON.
- `#![forbid(unsafe_code)]`; no cargo features; no async/IO dependencies.

## Used by

`Grep "origin-review" glob "crates/*/Cargo.toml"`:

- `crates/origin-cli/Cargo.toml`
- `crates/origin-daemon/Cargo.toml`
- `crates/origin-review/Cargo.toml` (self)

## Testing

Inline tests cover: dedup merging same-key findings to max confidence (keeping
the winner's detail) and preserving first-seen order; `filter` thresholds + sort
across all three strictness modes; ordered thresholds; `vote` majority / tie /
empty / unanimous cases; `triage` classifying bug/feature/question/docs and
defaulting to `Question` with no signal (never `Duplicate`); `similarity`
identical/disjoint/empty/partial-overlap (Jaccard `0.5`) and case/punctuation
insensitivity; and confidence clamping (over-1, negative, NaN).

## See also

- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
