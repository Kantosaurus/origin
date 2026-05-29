# Publishing the npm packages (maintainers)

The `origin` TUI is distributed on npm as a family of packages:

- **`originx`** — the package users install. A tiny JS launcher (`bin/origin.js`)
  + a postinstall fallback downloader (`install.js`). Exposes the `origin`
  command. Contains **no** binary itself.
- **`originx-<platform>-<arch>`** — six platform packages, each carrying exactly
  one prebuilt binary and gated by npm's `os`/`cpu` fields so only the matching
  one installs on a given machine.

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
- For the **first** publish the `originx*` names don't exist yet, so the token
  cannot be scoped to them — grant it **All packages** read/write (or org-wide),
  then optionally re-scope to `originx` / `originx-*` once they exist.
- npm may also reject `originx` as *too similar* to the existing `origin`
  package. If a retry fails with a similarity `E403` (a *different* message from
  the 2FA one above), publish the scoped family `@kantosaurus/origin` by setting
  **`ORIGIN_NPM_PREFIX=@kantosaurus/origin`** for the publish (the `npm publish`
  job carries a commented env line for exactly this). `build.mjs` reads it, names
  the main package and the six platform packages accordingly, and bakes the
  prefix into the shipped launcher so binary resolution matches. The installed
  command stays `origin`. **Caveat:** this mechanism lives in the code as of the
  commit that added it, so it only applies to a tag built from that commit or
  later — re-running an *older* tag's failed job (e.g. `v0.0.1`) uses that tag's
  frozen scripts and ignores the variable; cut a fresh tag to switch names. (The
  bundled npm README still says `originx`; update it if the scoped name sticks.)

### Re-running after a failed publish

The npm step reads `NPM_TOKEN` at run time, so after fixing the token you can
re-publish the **same** tag without cutting a new one:

```sh
gh run rerun <run-id> --failed   # re-runs only the failed npm-publish job
```

The platform packages publish before the main package. If a run published some
but not all of them, bump the version (a new tag) rather than re-running —
re-publishing an existing `name@version` fails with `E409`. (`build.mjs` could
be made idempotent by skipping versions already on the registry; not yet done.)

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
launcher runs `npm view originx@latest version` in the background once a day and,
when it sees a newer version, `npm install -g originx@latest` for a global
install or `npm install originx@latest --no-save` in the project root for a
local one). Consequences for releasing:

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

# Pack + install both tarballs into a throwaway project:
cd packaging/npm/dist/originx-linux-x64 && npm pack
cd ../originx && npm pack
mkdir -p /tmp/ot && cd /tmp/ot && npm init -y >/dev/null
npm i /home/.../packaging/npm/dist/originx/originx-0.0.1.tgz \
      /home/.../packaging/npm/dist/originx-linux-x64/originx-linux-x64-0.0.1.tgz
./node_modules/.bin/origin --help
```
