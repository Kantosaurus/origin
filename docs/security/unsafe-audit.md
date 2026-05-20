# `unsafe` audit (P14 exit)

Per spec §11 GA acceptance criterion 3, `unsafe` is permitted only in:

- `origin-cas` — mmap + zero-copy slicing.
- `origin-tui` — SIMD `wide::u8x32` damage diff.
- `origin-ipc` — shared file mapping for blob handoff.

CI enforces this via `.github/workflows/unsafe-audit.yml` (cargo-geiger).
Any other workspace crate landing an `unsafe` block fails the gate.

## Audited blocks

### `origin-cas`

- **`crates/origin-cas/src/packfile.rs:164` — `Pack::open` mmap**: `Mmap::map(&file)` is inherently `unsafe` because the kernel cannot prevent another process from mutating the backing file under us. Invariants: the file is opened read-only immediately above (line 152 et seq.), pack files are write-once / content-addressed (concurrent builders create disjoint files by name, never mutate an existing pack), and all subsequent slice indexing into the map is bounds-checked against `map.len()` (e.g. line 169, `cursor + 32 + 8 + 4 > map.len()`) before reading.

### `origin-tui`

- **`crates/origin-tui/src/grid.rs:168` — `Grid::as_bytes` byte view for SIMD diff**: `std::slice::from_raw_parts` reinterprets `&[Cell]` as `&[u8]` so the P4.2 SIMD damage-diff can run `wide::u8x32` lanes over the cell buffer. `Cell` is `#[repr(C)]` with size 16 and no padding (asserted at the type definition); the resulting slice has exactly `cells.len() * size_of::<Cell>()` bytes, lifetime-tied to `&self`, so aliasing and provenance are sound.

### `origin-ipc`

No `unsafe` blocks currently present. The crate is reserved in the allow-list for the planned shared-file-mapping blob handoff path; once that lands, append the audit entry here.

## Re-audit guidance

If a new `unsafe` block is added to any of the three allowed crates, append an entry here in the same shape. If a future crate genuinely needs `unsafe` and isn't yet in the allow-list:

1. Update the allowed set in `.github/workflows/unsafe-audit.yml`.
2. Add a `## <crate>` section to this doc with the audit prose.
3. Cite a security review in the PR description.
