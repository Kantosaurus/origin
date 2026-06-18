# Operations

For deploying, running, observing, and troubleshooting `origin`.

| Page | Purpose |
| --- | --- |
| [Deployment](deployment.md) | Install channels, auto-update, the single-binary model, the remote daemon. |
| [Daemon & supervisor](daemon-and-supervisor.md) | Daemon lifecycle, crash-restart and session resume, shutdown/draining, the IPC socket. |
| [Observability runbook](observability-runbook.md) | Enable OTLP, scrape Prometheus, read traces, toggle telemetry, run `origin doctor`. |
| [CI automation](ci-automation.md) | The `@origin` bot, PR review, issue triage, scheduled maintenance, and the quality-gate workflows. |
| [Benchmarking](benchmarking.md) | What `origin-bench` measures and the CI perf gate. |
| [Troubleshooting](troubleshooting.md) | A symptom → cause → fix reference. |

Conceptual background lives in [Architecture](../architecture/) and
[Observability](../subsystems/observability.md).

[← Documentation home](../README.md)

_Last reviewed against workspace version 0.9.8._
