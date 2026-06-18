# Architecture

How `origin` is put together: the system-level design and the cross-cutting
concerns that every subsystem inherits.

| Page | What it covers |
| --- | --- |
| [Overview](overview.md) | System at a glance, the daemon/CLI split, the two-runtime model, archived IR, content-addressed storage, the end-to-end request lifecycle, and the full crate map. |
| [Runtime & concurrency](runtime-and-concurrency.md) | The two-runtime daemon, task classes and `spawn_in`, the byte-ring backpressure model, allocator strategy, shutdown/draining, and the performance KPIs enforced as CI gates. |
| [Data & storage](data-and-storage.md) | Content-addressed storage, the Hot/Warm/Cold tiers, FastCDC chunking, archived `rkyv` IR persistence, the SQLite store, and session persistence/resume. |

Then dive into a [subsystem](../subsystems/) or the [crate index](../crates/README.md).

[← Documentation home](../README.md)

_Last reviewed against workspace version 0.9.8._
