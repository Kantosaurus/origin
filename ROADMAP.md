# Roadmap

origin is **pre-1.0** (`0.0.1`). The core is landed and gated — content-addressed
storage, archived IR, the two-runtime daemon, planner, provider catalog,
KeyVault, sandboxing, and deterministic replay. Some subsystems are "real but
young." This roadmap sketches direction; it is not a commitment of dates.

## Now (toward a stable 0.x)

- **Distribution**: first tagged release with signed artifacts across all six
  targets; publish the Homebrew tap, winget, and AUR manifests; reserve the
  crates.io names.
- **Supply chain**: keep the `cargo-deny`, fuzz, unsafe-audit, and Scorecard
  gates green; tune the `deny.toml` license allow-list into a hard gate.
- **Docs/SDK**: ship the ergonomic typed `origin-ipc` client (the facade noted
  in [the SDK guide](docs/site/src/sdk.md)); expand API docs and examples.

## Next (toward 1.0)

- Harden the swarm coordinator/worker path and the remote QUIC transport.
- Broaden provider coverage and streaming parity across the catalog.
- Raise test coverage and grow the committed fuzz corpora.
- Finalize the sandbox + KeyVault security review signoff
  (see [`docs/security/p14-security-review.md`](docs/security/p14-security-review.md)).

## Later

- Desktop frontend on the same `origin-ipc` wire protocol.
- Richer code-graph retrieval and memory.

## How this is tracked

Concrete work is tracked in [GitHub Issues](https://github.com/Kantosaurus/origin/issues).
Proposals are welcome — open an issue to discuss direction.
