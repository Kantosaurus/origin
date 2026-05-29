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
