# P14 Security Review Signoff

Spec criterion 4 — "Security review pass on sandbox profiles + KeyVault" —
is satisfied by completing each item below with the date + reviewer.

## Sandbox profiles

- [ ] **Linux**: namespace + seccomp + landlock applied for every AutoAllowed tool. Verified by `crates/origin-sandbox/tests/linux_*.rs`. Reviewer / date: ____
- [ ] **macOS**: `sandbox-exec` profiles deny network for AutoAllowed; allow only for explicitly-widened tools. Verified by `crates/origin-sandbox/tests/macos_*.rs`. Reviewer / date: ____
- [ ] **Windows**: AppContainer + JobObject CPU/RAM caps. Verified by `crates/origin-sandbox/tests/windows_*.rs`. Reviewer / date: ____
- [ ] **Hook scripts inherit triggering tool's profile** (no escalation). Spec §10D N10.12. Reviewer / date: ____

## KeyVault

- [ ] **Linux Secret Service backend** never persists plaintext to disk; age-encrypted fallback uses passphrase from `ORIGIN_KEYVAULT_PASSPHRASE` env. Audit `crates/origin-keyvault/src/backend_linux.rs`. Reviewer / date: ____
- [ ] **macOS Keychain backend** stores items under `service = "origin"` with access ACL restricted to the daemon binary. Audit `crates/origin-keyvault/src/backend_macos.rs`. Reviewer / date: ____
- [ ] **Windows Credential Manager backend** uses generic credentials scoped to the daemon SID. Audit `crates/origin-keyvault/src/backend_windows.rs`. Reviewer / date: ____
- [ ] **OAuth**: PKCE used for every device-flow provider; refresh-token rotation verified. Audit `crates/origin-keyvault/src/oauth.rs`. Reviewer / date: ____
- [ ] **Audit log** (`crates/origin-keyvault/src/audit.rs`) records every credential access with `(provider, account, tool, ts)`. 30-day rotation verified. Reviewer / date: ____

## MCP

- [ ] **Message validation** at the buffer layer with 16 MiB cap per response (spec §10D N10.13). Audit `crates/origin-mcp/src/`. Reviewer / date: ____
- [ ] **Schema mismatches** are rejected before agent exposure. Reviewer / date: ____

## `Secret<T>` lint

- [ ] CI lint asserts no struct field named `*key*`, `*token*`, `*password*`, `*auth*` emits raw bytes through `tracing`. Spec §10D N10.14. Reviewer / date: ____

## Worker isolation

- [ ] Swarm workers run as child processes with CPU/RAM caps via cgroup (Linux), Job Objects (Windows), `taskpolicy` (macOS). Spec §10D N10.15. Reviewer / date: ____

## Sign-off

Reviewer: __________________________  Date: __________

Hash of audited tree (commit SHA at sign-off time): __________

The reviewer signs this file by committing their name + date in place of
each `____` placeholder, then committing a `chore(security): P14 review signoff`
commit that gates the v1.0.0 tag.
