# Governance

origin is a young, pre-1.0 project with a lightweight governance model. This
document describes how decisions are made and who makes them, so contributors
know what to expect.

## Roles

- **Maintainer** — currently a single maintainer, [@Kantosaurus](https://github.com/Kantosaurus)
  (Ainsley Woo). The maintainer reviews and merges pull requests, cuts releases,
  triages issues, and sets technical direction. Ownership of security-sensitive
  areas is recorded in [`.github/CODEOWNERS`](.github/CODEOWNERS).
- **Contributors** — anyone who opens an issue or PR. You do not need to be a
  maintainer to propose changes; see [CONTRIBUTING.md](CONTRIBUTING.md).

## How decisions are made

- **Small changes** (bug fixes, docs, tests) are merged by the maintainer once
  CI is green and the change meets the quality gates in CONTRIBUTING.md.
- **Larger changes** (new subsystems, public-API or wire-protocol changes,
  dependency additions) should start as an issue or draft PR so the approach can
  be agreed before significant work. Designs and plans for non-trivial work are
  captured in the issue/PR or a linked tracking issue.
- The maintainer has final say. Disagreements are resolved by discussion in the
  relevant issue/PR; the goal is consensus, with the maintainer breaking ties.

## Adding maintainers

As the project grows, contributors with a sustained track record of high-quality
contributions may be invited to become maintainers. This document will be updated
to name them and to describe a more formal decision process if/when that happens.

## Releases

See the release process in [CONTRIBUTING.md](CONTRIBUTING.md) and the automated
pipeline in [`.github/workflows/release.yml`](.github/workflows/release.yml).
