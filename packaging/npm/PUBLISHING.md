# Publishing the npm packages (maintainers)

The `origin` TUI is distributed on npm as a scoped family of packages
(`@kantosaurus/origin`; the family prefix is `PKG_PREFIX` in
`packaging/npm/lib/platform.js`, overridable at publish time via
`ORIGIN_NPM_PREFIX`):

- **`@kantosaurus/origin`** — the package users install. A tiny JS launcher
  (`bin/origin.js`) + a postinstall fallback downloader (`install.js`). Exposes
  the `origin` command. Contains **no** binary itself.
- **`@kantosaurus/origin-<platform>-<arch>`** — six platform packages, each
  carrying exactly one prebuilt binary and gated by npm's `os`/`cpu` fields so
  only the matching one installs on a given machine.

This mirrors how esbuild / @biomejs/biome / swc ship native binaries: fast,
offline-capable installs with no compiler required, and a download fallback for
the rare cases where optional dependencies are omitted.

## Automated (recommended)

Publishing is wired into `.github/workflows/release.yml`. Pushing a `vX.Y.Z`
tag:

1. Cross-compiles the six release binaries and uploads them (plus cosign
   signatures, SLSA provenance, and a `SHA256SUMS` manifest) to a GitHub
   Release.
2. Runs `packaging/npm/scripts/build.mjs --version <X.Y.Z> --binaries <dir>
   --publish --provenance`, which assembles and publishes all npm packages.

Requirements:

- A repository secret **`NPM_TOKEN`**. It MUST be a **Granular Access Token**
  with **read/write** package permission and the **"bypass two-factor
  authentication"** capability. A *classic automation* token is **not** enough
  when the account enforces 2FA for writes: the publish reaches the registry and
  even signs provenance, then fails with
  `E403 … Two-factor authentication or granular access token with bypass 2fa
  enabled is required to publish packages`. (Alternatively, lower the npm
  account's 2FA level to *Authorization only* so a classic automation token may
  publish — less secure; the GAT is preferred.)
- For the **first** publish the `@kantosaurus/origin*` names don't exist yet, so
  the token can't be scoped to them — grant it **All packages** read/write (or
  scope it to the whole `@kantosaurus` scope), then optionally narrow it once the
  names exist.
- **Why scoped:** the original unscoped `originx*` names tripped npm's
  **spam-detection** filter mid-publish — four platform packages went up at
  `0.0.1`, then `PUT originx-win32-x64` returned `E403 … Package name triggered
  spam detection`. Scoped names live under the account namespace, where that
  filter does not apply, so the family default is now `@kantosaurus/origin`
  (`PKG_PREFIX`). `ORIGIN_NPM_PREFIX` overrides it at publish time. **Caveat:**
  the prefix is read from the *tag's* checked-out scripts, so re-running an older
  tag's failed job (e.g. the original `v0.0.1`) uses that tag's frozen prefix —
  cut a fresh tag to change names.

### Re-running after a failed publish

The npm step reads `NPM_TOKEN` at run time, so after fixing the token you can
re-publish the **same** tag without cutting a new one:

```sh
gh run rerun <run-id> --failed   # re-runs only the failed npm-publish job
```

The platform packages publish before the main package. A partial publish is
recoverable in place: `build.mjs` is **idempotent** — in `--publish` mode it
probes the registry and skips any `name@version` already published, completing
only the missing packages, so a `gh run rerun --failed` (or a fresh equal-version
run) finishes the family without tripping npm's "cannot publish over a previously
published version" error.

## Manual

```sh
# 1. Build the release binaries (or download them from an existing release into
#    a directory named exactly origin-<triple>[.exe]).
#    e.g. via the release workflow artifacts, or locally with cross/cargo.

# 2. Assemble the packages without publishing (inspect packaging/npm/dist/):
node packaging/npm/scripts/build.mjs --version 0.1.0 --binaries ./binaries

# 3. Dry-run the publish:
node packaging/npm/scripts/build.mjs --version 0.1.0 --binaries ./binaries \
  --publish --dry-run

# 4. Log in and publish for real:
npm login
node packaging/npm/scripts/build.mjs --version 0.1.0 --binaries ./binaries --publish
```

Notes:

- The script publishes the platform packages **before** the main package so the
  `optionalDependencies` resolve for early installers.
- Prerelease versions (`-rc`, `-beta`, `-alpha`) auto-publish under the `next`
  dist-tag instead of `latest`. Override with `--tag <tag>`.
- `--provenance` requires running in GitHub Actions with `id-token: write`.
- Targets whose binary is absent from `--binaries` are skipped and dropped from
  the main package's `optionalDependencies`, so partial publishes are possible
  (but a normal release should include all six).

## Versioning

Keep the npm version in lockstep with the Cargo workspace version (the tag
`vX.Y.Z` → npm `X.Y.Z`). The committed `packaging/npm/package.json` version is a
placeholder; `build.mjs --version` is authoritative at publish time.

## Auto-update and dist-tags

Installed clients auto-update by following the **`latest`** dist-tag (the
launcher runs `npm view @kantosaurus/origin@latest version` in the background
once a day and, when it sees a newer version, `npm install -g
@kantosaurus/origin@latest` for a global install or `npm install
@kantosaurus/origin@latest --no-save` in the project root for a local one).
Consequences for releasing:

- Publishing `X.Y.Z` to `latest` rolls it out to all global installs within ~24h,
  and to local installs the next time `origin` is launched from that project
  (the `--no-save` refresh updates the working copy without touching the
  project's declared dependency range).
- Prereleases (`-rc`/`-beta`/`-alpha`) auto-publish to the **`next`** tag, so
  they never become `latest` and never auto-update stable users. The client's
  version comparison also refuses to move a stable install onto a prerelease.
- To pull a bad release, `npm deprecate` it and publish a fixed `latest`; clients
  converge on the next daily check.

## Local end-to-end test (no registry)

```sh
# Assemble into dist/ using a locally built binary:
mkdir -p /tmp/ob && cp target/debug/origin /tmp/ob/origin-x86_64-unknown-linux-gnu
node packaging/npm/scripts/build.mjs --version 0.0.1 --binaries /tmp/ob

# Pack + install both tarballs into a throwaway project. The scoped family
# assembles under dist/@kantosaurus/ (npm pack writes kantosaurus-origin-*.tgz):
cd packaging/npm/dist/@kantosaurus/origin-linux-x64 && npm pack
cd ../origin && npm pack
mkdir -p /tmp/ot && cd /tmp/ot && npm init -y >/dev/null
npm i /home/.../packaging/npm/dist/@kantosaurus/origin/kantosaurus-origin-0.0.1.tgz \
      /home/.../packaging/npm/dist/@kantosaurus/origin-linux-x64/kantosaurus-origin-linux-x64-0.0.1.tgz
./node_modules/.bin/origin --help
```
