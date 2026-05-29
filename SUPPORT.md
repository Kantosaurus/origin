# Getting help with origin

Thanks for using **origin**. Here is where to go, depending on what you need.

| I want to… | Go here |
| --- | --- |
| **Report a bug** | [Open a bug report](https://github.com/Kantosaurus/origin/issues/new?template=bug_report.yml) |
| **Request a feature** | [Open a feature request](https://github.com/Kantosaurus/origin/issues/new?template=feature_request.yml) |
| **Ask a question / discuss an idea** | [GitHub Discussions](https://github.com/Kantosaurus/origin/discussions) (enable in repo settings if you don't see it) |
| **Report a security vulnerability** | **Do not** open a public issue — follow [SECURITY.md](SECURITY.md) |
| **Read the docs** | The handbook at <https://Kantosaurus.github.io/origin/> (source in [`docs/site/`](docs/site/src/SUMMARY.md)) |
| **Troubleshoot** | [Troubleshooting guide](docs/site/src/troubleshooting.md) |

## Before you file a bug

- Reproduce on the latest release (`origin --version`).
- Grab the daemon log: `<data-dir>/origin/logs/daemon.log`
  (e.g. `%LOCALAPPDATA%\origin\logs\daemon.log` on Windows).
- Include your OS/platform and the provider in use.

origin is pre-1.0 software maintained on a best-effort basis. There is no SLA for
issues, but security reports are acknowledged within 3 business days (see
[SECURITY.md](SECURITY.md)).
