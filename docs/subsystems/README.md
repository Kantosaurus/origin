# Subsystems

Per-domain deep-dives. Each page maps to one or more crates and links out to
their reference pages in the [crate index](../crates/README.md).

| Page | Primary crates |
| --- | --- |
| [Agent loop & sessions](agent-and-sessions.md) | `origin-daemon`, `origin-goal`, `origin-steering` |
| [Providers](providers.md) | `origin-provider*`, `origin-shimquirks`, `origin-modeldiscovery`, `origin-router`, `origin-cost` |
| [Tools](tools.md) | `origin-tools`, `origin-mcp`, `origin-browser`, `origin-vcs`, `origin-editfmt`, `origin-lspfleet`, … |
| [Skills, hooks & workflows](skills.md) | `origin-skills`, `origin-hooks`, `origin-workflowgen` |
| [Memory, code-graph & retrieval](memory-and-codegraph.md) | `origin-mem`, `origin-codegraph`, `origin-knowledge`, `origin-repomap` |
| [Swarm & orchestration](swarm-and-orchestration.md) | `origin-swarm`, `origin-plan`, `origin-ambient`, `origin-schedule` |
| [TUI & CLI](tui-and-cli.md) | `origin-cli`, `origin-tui`, `origin-i18n`, `origin-outputstyle`, `origin-mermaid` |
| [Observability, telemetry & diagnostics](observability.md) | `origin-trace`, `origin-metrics`, `origin-telemetry`, `origin-doctor`, `origin-notify`, `origin-cost` |

For the security layer, see the [Security model](../security/security-model.md).

[← Documentation home](../README.md)

_Last reviewed against workspace version 0.9.8._
