# Release process

Releasing is a maintainer task. This page documents how a version is cut, how the
automated pipeline turns a tag into signed artifacts, and how the downstream
distribution channels (npm, Homebrew, winget, AUR, cargo-binstall) are fed. It is
grounded in `xtask/src/release.rs`, `packaging/npm/PUBLISHING.md`, and the GitHub
workflows.

---

## Versioning

- The workspace has a **single version**, `version = "0.9.8"` in
  `[workspace.package]` of the root `Cargo.toml`. Every member inherits it via
  `version.workspace = true`. There is no per-crate version skew.
- origin is **pre-1.0**: the public API and the IPC wire protocol may change
  between minor versions. Bumps are SemVer-flavored but conservative pre-1.0.
- A release is a git **tag** `vX.Y.Z` on `main`. Tag `vX.Y.Z` maps to npm
  `X.Y.Z`. Keep the npm version in lockstep with the Cargo workspace version.
- **Prereleases** use a suffix: `vX.Y.Z-rc.1`, `-beta.N`, `-alpha.N`. They publish
  to the npm `next` dist-tag and never become `latest`.

To bump: edit the single `version` key in the root `Cargo.toml`, run a `--locked`
build so `Cargo.lock` updates, finalize the `CHANGELOG.md`, and commit on `dev`.

---

## Branch flow

origin uses a two-branch model (see [contributing.md](contributing.md)):

- **`dev`** — default integration branch; all PRs target it; docs deploy from it.
- **`main`** — the release branch; never committed to directly; advances only by
  merging `dev` once stable.

```text
PRs ─▶ dev ──(merge when stable)──▶ main ──(push vX.Y.Z tag)──▶ release.yml
```

---

## CHANGELOG discipline

`CHANGELOG.md` loosely follows *Keep a Changelog*. The rules:

- Every behavior, config, or public-API change lands with an entry under
  `## Unreleased`, grouped into **Added / Changed / Fixed / Removed**.
- At release time, the maintainer renames `## Unreleased` to
  `## X.Y.Z — YYYY-MM-DD` and opens a fresh empty `## Unreleased`.
- The CHANGELOG is the source for the GitHub Release notes. Keep entries
  user-facing and specific.

---

## Cutting a release (maintainer)

1. Ensure `dev` is green across CI, perf-gate, audit, and docs.
2. Bump the workspace `version` and finalize the CHANGELOG on `dev`.
3. Merge `dev` → `main`.
4. Tag and push:

   ```sh
   git checkout main && git pull
   git tag v0.9.8
   git push origin v0.9.8        # for a prerelease: git tag v0.9.9-rc.1
   ```

5. The tag triggers `release.yml`. Watch the run; if the npm step fails on a
   token/2FA issue you can re-run it in place (see [npm](#npm-kantosaurusorigin)).

---

## The release pipeline (`release.yml`)

Pushing a `vX.Y.Z` tag on `main` runs the release workflow, which:

1. **Builds a 6-target matrix** of release binaries:

   | Target triple | Platform |
   | --- | --- |
   | `x86_64-unknown-linux-gnu` | Linux x64 (glibc/gnu) |
   | `aarch64-unknown-linux-gnu` | Linux arm64 (glibc/gnu) |
   | `x86_64-apple-darwin` | macOS x64 |
   | `aarch64-apple-darwin` | macOS arm64 |
   | `x86_64-pc-windows-msvc` | Windows x64 |
   | `aarch64-pc-windows-msvc` | Windows arm64 |

   > Linux targets are **glibc/gnu**, matching the release build (the project moved
   > off musl in the Homebrew/AUR templates and the `xtask release` stamper).

2. **Signs and attests** each artifact: cosign keyless signatures, SLSA
   build-provenance attestation, an SBOM, and a `SHA256SUMS` manifest, all uploaded
   to a GitHub Release.
3. **Publishes the npm family** by running
   `packaging/npm/scripts/build.mjs --version <X.Y.Z> --binaries <dir> --publish --provenance`.
4. **Stamps the OS package manifests** via `xtask release` (below) for
   Homebrew/winget/AUR.

CI hardening applies: actions are pinned to commit SHAs, workflows use
least-privilege `permissions:`, `concurrency`, `timeout-minutes`, and `--locked`
builds.

---

## `xtask release` — stamping packaging templates

The packaging manifests are committed as templates with placeholders.
`xtask release` substitutes the version and the per-target SHA-256 sums from a
manifest JSON the release job produces after uploading the binaries:

```sh
cargo run -p xtask -- release \
  --version 0.9.8 \
  --manifest path/to/sha256-manifest.json \
  --out target/release-packaging
```

It reads these templates from `packaging/` and writes stamped copies to `--out`:

| Template | Channel |
| --- | --- |
| `packaging/homebrew/origin.rb.tmpl` | Homebrew |
| `packaging/winget/manifests/Kantosaurus.origin.yaml.tmpl` | winget (version) |
| `packaging/winget/manifests/Kantosaurus.origin.installer.yaml.tmpl` | winget (installer) |
| `packaging/winget/manifests/Kantosaurus.origin.locale.en-US.yaml.tmpl` | winget (locale) |
| `packaging/aur/PKGBUILD.tmpl` | AUR |

Placeholders substituted: `{{VERSION}}` and `{{SHA256_MAC_ARM}}`,
`{{SHA256_MAC_X64}}`, `{{SHA256_LINUX_ARM}}`, `{{SHA256_LINUX_X64}}`,
`{{SHA256_WIN_X64}}`, `{{SHA256_WIN_ARM}}` (keyed by target triple in the manifest
JSON).

---

## Manpages

`xtask manpages` renders `clap_mangen` output for `origin` and every subcommand.
The docs workflow runs it during the site build; it can be run standalone:

```sh
cargo run -p xtask --locked -- manpages --out target/manpages
```

It introspects `origin_cli::main_cli()` (so it never depends on the binary crate)
and writes `origin.1` plus one `<sub>.1` per registered subcommand into `--out`.

---

## Distribution channels

### npm (`@kantosaurus/origin`)

The TUI ships on npm as a scoped family (the family prefix is `PKG_PREFIX` in
`packaging/npm/lib/platform.js`, overridable via `ORIGIN_NPM_PREFIX`):

- **`@kantosaurus/origin`** — what users install. A tiny JS launcher
  (`bin/origin.js`) + a postinstall fallback downloader (`install.js`). Contains
  **no** binary itself; exposes the `origin` command.
- **`@kantosaurus/origin-<platform>-<arch>`** — six platform packages, each
  carrying exactly one prebuilt binary, gated by npm's `os`/`cpu` fields so only
  the matching one installs.

Publishing is automated by `release.yml`. Key facts (full detail in
[`packaging/npm/PUBLISHING.md`](../../packaging/npm/PUBLISHING.md)):

- Requires a repository secret **`NPM_TOKEN`** — a **Granular Access Token** with
  read/write package permission and the **"bypass two-factor authentication"**
  capability (a classic automation token fails with `E403` when 2FA is enforced).
- The platform packages publish **before** the main package, so
  `optionalDependencies` resolve for early installers.
- `build.mjs --publish` is **idempotent**: it probes the registry and skips any
  `name@version` already published, so a partial publish is recoverable in place:

  ```sh
  gh run rerun <run-id> --failed   # re-runs only the failed npm-publish job
  ```

- **Names are scoped** because the original unscoped names tripped npm's
  spam-detection filter; the prefix is read from the *tag's* checked-out scripts,
  so to change names you cut a fresh tag.

Manual publish (maintainers), for reference:

```sh
node packaging/npm/scripts/build.mjs --version 0.9.8 --binaries ./binaries           # assemble only
node packaging/npm/scripts/build.mjs --version 0.9.8 --binaries ./binaries --publish --dry-run
npm login
node packaging/npm/scripts/build.mjs --version 0.9.8 --binaries ./binaries --publish
```

#### Auto-update and dist-tags

- Installed clients follow the **`latest`** dist-tag: the launcher checks
  `npm view @kantosaurus/origin@latest version` once a day in the background and
  updates when a newer version appears. Publishing `X.Y.Z` to `latest` rolls out
  to global installs within ~24h.
- Prereleases publish to the **`next`** tag, never become `latest`, and never
  auto-update stable users; the client refuses to move a stable install onto a
  prerelease.
- To pull a bad release: `npm deprecate` it and publish a fixed `latest`; clients
  converge on the next daily check.

### Homebrew / winget / AUR

These manifests are committed as templates and stamped by `xtask release` (above)
with the version and per-target SHA-256 sums, then the release job opens/updates
them in their respective taps/repos. The canonical repo owner is
`Kantosaurus/origin` across all packaging templates.

### cargo-binstall

The release ships `cargo-binstall` metadata so `cargo binstall origin` can fetch
a prebuilt binary from the GitHub Release rather than compiling from source.

### crates.io

Every crate carries a `description`; internal crates (e.g. `origin-bench`) are
marked `publish = false`. Reserving/publishing the crates.io names is on the
roadmap; the workspace metadata (`homepage`, `repository`, `license`,
`description`) is already crates.io-ready.

---

## Docs site → GitHub Pages (`docs.yml`)

The mdBook in `docs/site/` publishes to GitHub Pages automatically:

- On push to `dev` (the active default branch), `docs.yml` runs
  `mdbook build docs/site`, builds the manpages (`xtask manpages`), uploads the
  Pages artifact, and deploys.
- `mdbook` is pinned to `0.4.40` (the last 0.4.x that builds on a 1.83-safe
  toolchain). Bump the pin when the workspace MSRV moves.

Docs deployment is decoupled from the binary release — site changes go live from
`dev` without a version tag.

---

## Post-release checklist

- [ ] GitHub Release shows all six binaries + `SHA256SUMS` + signatures + SBOM.
- [ ] `@kantosaurus/origin@X.Y.Z` and all six platform packages are on npm under
      the correct dist-tag (`latest` for stable, `next` for prereleases).
- [ ] Homebrew/winget/AUR manifests updated to the new version + sums.
- [ ] `CHANGELOG.md` has the dated section and a fresh empty `## Unreleased`.
- [ ] Docs site reflects any user-facing changes.

_Last reviewed against workspace version 0.9.8._
