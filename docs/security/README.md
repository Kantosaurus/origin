# Security

How `origin` defends the user: the model, the enforcement points, and the audit
trail.

| Page | Purpose |
| --- | --- |
| [Security model](security-model.md) | The threat model and every defense layer: permission tiers, `allowed-tools` narrowing, command-line safety, per-OS sandboxing, secret handling, governance policy, remote-transport security, the zero-`unsafe` posture, and an operator hardening checklist. |
| [`unsafe` audit](unsafe-audit.md) | The audited list of crates permitted to use `unsafe` (`origin-cas`, `origin-tui`, `origin-ipc`) and why each exception exists. |
| [P14 security review signoff](p14-security-review.md) | The security-review checklist for sandbox profiles and the KeyVault. |

For how secrets are stored and redacted, see also [`origin-keyvault`](../crates/origin-keyvault.md);
for the command-safety analyzer, [`origin-cmdparse`](../crates/origin-cmdparse.md);
for governance, [`origin-policy`](../crates/origin-policy.md) and
[`origin-conseca`](../crates/origin-conseca.md).

> **Reporting a vulnerability:** do **not** open a public issue. See the
> root [`SECURITY.md`](../../SECURITY.md) for private reporting via GitHub
> Security Advisories.

[← Documentation home](../README.md)

_Last reviewed against workspace version 0.9.8._
