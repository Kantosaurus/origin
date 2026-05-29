# Third-party components

The `origin-cloak-sidecar` wrapper in this directory is licensed Apache-2.0
(see the repository root [`LICENSE`](../../LICENSE)). At install time it fetches
the following third-party packages via `npm install` (`package.json`
`dependencies`). They are **not** vendored into this repository; they are
downloaded onto the user's machine and are governed by their own licenses:

| Package | Source | License |
| --- | --- | --- |
| `cloak-browser` | [`github:CloakHQ/CloakBrowser`](https://github.com/CloakHQ/CloakBrowser), pinned to commit `14ec2ebf5f952b3dcc8ee019965cc48cbf7ccf53` | MIT |
| `playwright-core` | [npm](https://www.npmjs.com/package/playwright-core) (`^1.45.0`) | Apache-2.0 |

Both licenses (MIT and Apache-2.0) are compatible with this project's
Apache-2.0 license.

`cloak-browser` is pinned to an immutable commit SHA rather than a moving
`#main` ref so the fetched code is reproducible and auditable. Bump the SHA
deliberately (and re-verify the upstream license) when updating.
