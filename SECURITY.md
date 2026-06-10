# Security Policy

Thanks for helping keep **origin** and its users safe. This document explains
which versions receive fixes and how to report a security issue privately.

## Supported versions

origin is pre-1.0 software under active development. Security fixes are applied
to the latest release and the `dev` branch only; older snapshots are not
patched. Please reproduce on the most recent version before reporting.

| Version | Supported          |
| ------- | ------------------ |
| latest release / `dev` | :white_check_mark: |
| older   | :x:                |

> Maintainers: update this table as you cut tagged releases.

## Reporting a vulnerability

**Please do not open a public GitHub issue, pull request, or discussion for a
security problem**, and do not disclose it publicly until a fix is available.
Public reports give everyone the details before users can update.

Instead, report it privately through one of these channels:

1. **GitHub private vulnerability reporting** (preferred) — open a private
   advisory at:
   <https://github.com/Kantosaurus/origin/security/advisories/new>

2. **Email** — send the details to:
   `wooainsley@gmail.com`
   For an encrypted channel, prefer the GitHub private advisory above (it is
   encrypted in transit); if you need PGP, request a public key at the same
   address.

### What to include

A good report helps us confirm and fix the issue quickly. Where possible,
please include:

- A clear description of the issue and its security impact.
- The component affected (e.g. the daemon, CLI, the IPC transport, the
  sandbox, or credential/key storage) and the platform (OS + architecture).
- The `origin --version` you reproduced on.
- Step-by-step reproduction instructions, and any minimal sample input,
  config, or logs needed to observe the behavior.
- Any suggested remediation, if you have one.

Please send the report only to the private channel above — avoid attaching
secrets, credentials, or personal data beyond what is necessary to demonstrate
the issue.

## What to expect

- **Acknowledgement** within **3 business days** that we received your report.
- An initial assessment and a request for any additional information we need.
- Regular updates on remediation progress until the issue is resolved.
- Credit for the discovery in the release notes / advisory, unless you ask to
  remain anonymous.

We ask that you give us a reasonable period to release a fix before any public
disclosure. We will coordinate a disclosure timeline with you and aim to
publish an advisory once a fixed version is available.

## Scope

In scope: vulnerabilities in this repository's code — for example the
agent daemon, the CLI, the `origin-ipc` transport, the sandbox/permission
layer, and credential/key handling.

Out of scope: issues in third-party dependencies (please report those upstream;
you may still let us know so we can pin or patch), and findings that require a
privileged local attacker, a compromised host, or social engineering of the
user, unless they reveal a concrete weakness in origin itself.

## Safe harbor

We will not pursue or support legal action against anyone who reports a
vulnerability in good faith through the channels above, who avoids privacy
violations and service disruption, and who gives us a reasonable time to
remediate before public disclosure.

## Cryptography & trust model

origin is designed for a **zero-trust** posture (no channel or peer is trusted
implicitly) and a **post-quantum-aware** cryptographic posture (the load-bearing
*authentication* primitives are quantum-resistant today, and the remaining
classical primitives have a defined migration path).

### Zero trust

- **Remote IPC (QUIC) is mutually authenticated.** `origin-ipc` requires *both*
  sides of a remote connection to present a certificate (`crates/origin-ipc/src/quic.rs`).
  The server only accepts clients whose certificate SHA-256 fingerprint is on an
  explicit pinned allow-list, and an empty allow-list trusts **no** peer (fail
  closed). The client pins the server's certificate to the SHA-256 fingerprint
  distributed out of band in the pairing URL (`origin://host:port#<fingerprint>`)
  instead of validating a CA chain, so only the exact paired daemon is trusted
  and there is no PKI to subvert. A connection with no valid pin is refused
  rather than downgraded to unauthenticated trust.
- **Local IPC** still relies on filesystem permissions of the socket / named
  pipe path; binding it to per-user-only locations remains the deployment
  contract.

### Post-quantum posture

- **The authentication anchor is a hash, not a signature.** Certificate pinning
  uses SHA-256 (`CertFingerprint`). A quantum adversary able to forge the
  classical Ed25519 certificate signature still cannot produce a different
  certificate with the same SHA-256 fingerprint, so the *identity* decision is
  already quantum-resistant. Symmetric integrity elsewhere (BLAKE3 keyed MACs,
  HMAC, SHA-256) is likewise PQ-resistant (Grover only halves the security
  margin, which 256-bit digests absorb).
- **What is still classical:** the TLS 1.3 key exchange (X25519 ECDHE) and
  certificate signatures (Ed25519) provided by the `ring` crypto provider. These
  protect *confidentiality in transit* and are exposed to a future
  "harvest-now-decrypt-later" quantum adversary, **not** to forgery of the
  pinned identity.
- **Migration path:** moving the key exchange to a hybrid `X25519MLKEM768` group
  is a drop-in swap of the rustls crypto provider in `origin-ipc` (see the
  `provider()` helper) once a pure-Rust post-quantum provider is vendored; the
  pinning and frame logic are unaffected. Certificate signatures can likewise
  migrate to a hybrid Ed25519+ML-DSA scheme without touching the pinning anchor.

### Recommended follow-up hardening

A standing audit tracks these lower-severity / latent items (none are remotely
exploitable in the current default build, but each should be closed before the
relevant feature ships broadly):

- **Sandbox:** scrub provider API keys from the environment inherited by
  sandboxed child processes; make the `noop` backend fail closed (refuse to run
  unconfined where confinement is expected); tighten the macOS read/network
  profiles to the documented workspace scope.
- **Supply chain:** authenticate the self-update staged binary, the supervisor
  relaunch manifest, and the npm download with a signature (or at minimum a
  keyed MAC / mandatory fail-closed checksum) rather than transport trust alone.
- **Inline MCP / sub-agents:** treat a model-supplied `mcp_servers[].command` /
  `url` as untrusted — gate the spawn behind the same permission/allow-list path
  as other process execution, and host/scheme allow-list inline-MCP URLs.
- **OAuth loopback:** validate the `state` parameter on the loopback redirect in
  addition to PKCE.
- **Metrics endpoint:** add a `Host`-header / auth check (or keep it strictly
  loopback) to blunt DNS-rebinding reads of usage telemetry.
