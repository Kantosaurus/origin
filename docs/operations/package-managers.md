# Package-manager publishing

`origin` is distributed through several package managers in addition to the
GitHub Release and npm. Each one is wired into `.github/workflows/release.yml`
as a **separate job** that runs after `release` and **never blocks** the core
binaries / npm publish. Every job that needs a credential **gates on its
secret**: when the secret is absent the job's steps skip and the job stays green,
so the release is unaffected until you opt in by provisioning the secret (and,
for tap/bucket-style managers, the destination repo).

The per-platform binaries and checksums are stamped into the manifests by
`xtask release` (the `release` job), which substitutes `{{VERSION}}` and the
`{{SHA256_*}}` placeholders from the published `SHA256SUMS`.

## Status matrix

| Manager | Secret(s) | Extra repo / account | Bootstrap | Activates |
|---|---|---|---|---|
| **cargo-binstall** | — | — | — | immediately (metadata in the crate) |
| **Homebrew** (tap) | `HOMEBREW_TAP_TOKEN` | `Kantosaurus/homebrew-origin` | create + init repo | once secret + repo exist |
| **Scoop** (bucket) | `SCOOP_BUCKET_TOKEN` | `Kantosaurus/scoop-origin` | create + init repo | once secret + repo exist |
| **Nix** (flake) | — (uses `GITHUB_TOKEN`) | — | first release commits `flake-sources.json` to `dev` | after the first tagged release |
| **Chocolatey** | `CHOCO_API_KEY` | chocolatey.org account | first push enters moderation | once secret exists (+ approval) |
| **winget** | `WINGET_TOKEN` (classic PAT) | `Kantosaurus/winget-pkgs` fork | first version submitted manually | from the **next** tag after bootstrap |
| **AUR** | `AUR_SSH_PRIVATE_KEY`, `AUR_USERNAME`, `AUR_EMAIL` | `aur.archlinux.org/origin-bin.git` | claim the AUR package once by hand | once secret + package exist |

Add secrets at **Settings → Secrets and variables → Actions → New repository
secret**. Create the extra repos under the `Kantosaurus` account.

## cargo-binstall — no setup

`crates/origin-cli/Cargo.toml` carries `[package.metadata.binstall]`. Because the
crate is not on crates.io, install with the `--git` form:

```sh
cargo binstall --git https://github.com/Kantosaurus/origin origin-cli
```

This channel does **not** verify a checksum/signature (cargo-binstall only checks
opt-in minisign, which we do not ship). Integrity-sensitive users should prefer
npm or Homebrew.

## Homebrew (tap)

1. Create a public repo **`Kantosaurus/homebrew-origin`** and initialise it (a
   README commit is enough — an empty repo cannot be cloned). The job writes
   `Formula/origin.rb`.
2. Create a PAT with **Contents: Read and write** on that repo (fine-grained,
   scoped to `homebrew-origin`) and add it as **`HOMEBREW_TAP_TOKEN`**.

Install: `brew install kantosaurus/origin/origin`. Apple Silicon + Linux only;
Intel macOS is unsupported (no `x86_64-apple-darwin` build) and the formula
`odie`s with a clear message there.

## Scoop (bucket)

1. Create a public repo **`Kantosaurus/scoop-origin`** and initialise it (README
   commit). The job writes `bucket/origin.json`.
2. Add a PAT with **Contents: Read and write** on it as **`SCOOP_BUCKET_TOKEN`**.

Install: `scoop bucket add origin https://github.com/Kantosaurus/scoop-origin; scoop install origin`.

## Nix (flake)

No secret or extra repo — the `nix-sources-update` job uses the built-in
`GITHUB_TOKEN` to commit the stamped `packaging/nix/flake-sources.json` back to
`dev` after each release. The flake installs the **prebuilt** binary (a
from-source build is impractical because `origin-mem` links ONNX Runtime).

Install: `nix profile install github:Kantosaurus/origin`. Linux + Apple Silicon.
**Bootstrap:** `flake-sources.json` does not exist until the first tagged release
runs the job, so `nix profile install` works only after v0.9.10's release
completes (the job's commit to `dev` is what `github:` resolves). The Linux
derivation uses `autoPatchelfHook` + `stdenv.cc.cc.lib` + `zlib`; if a future
build links a new shared lib, add it to `buildInputs` in `flake.nix`.

## Chocolatey

1. Create a free account at <https://community.chocolatey.org/>, copy the API key
   from your account page, add it as **`CHOCO_API_KEY`**.
2. The **first** push of the `origin` id enters the moderation queue and goes
   live only after a human moderator approves it. If the `origin` id is already
   taken on the community feed, rename the id in `packaging/chocolatey/origin.nuspec.tmpl`
   (and the install command) to e.g. `origin-cli`.

Install: `choco install origin`. Downloads + sha256-verifies the release `.exe`.

## winget

1. Fork **`microsoft/winget-pkgs`** to **`Kantosaurus/winget-pkgs`**.
2. Create a **classic** PAT with the **`public_repo`** scope (fine-grained PATs
   are not supported by `winget-releaser`) and add it as **`WINGET_TOKEN`**.
3. **Bootstrap (one time):** `winget-releaser` cannot create a brand-new package
   — it needs an existing `Kantosaurus.origin` version to base on. Submit the
   first version by hand from the stamped manifests on the v0.9.10 Release:
   ```pwsh
   # download the 3 stamped manifests from the Release, then:
   winget validate --manifest .
   wingetcreate submit --token <classic-PAT> .
   ```
   After that PR is merged, the `winget-publish` job handles every later tag
   automatically.

Install (after approval): `winget install Kantosaurus.origin`.

## AUR

1. Generate a dedicated key: `ssh-keygen -t ed25519 -C aur-origin-bin -f aur_key -N ''`,
   add the **public** key to your aur.archlinux.org account.
2. Add the **private** key as **`AUR_SSH_PRIVATE_KEY`**, plus **`AUR_USERNAME`**
   and **`AUR_EMAIL`** (matching the PKGBUILD `Maintainer`).
3. **Bootstrap (one time):** clone `ssh://aur@aur.archlinux.org/origin-bin.git`
   (empty for a new name), add a valid `PKGBUILD` + `.SRCINFO`, commit and push
   once to claim the package. The job keeps it updated thereafter.

Install: `yay -S origin-bin` (or any AUR helper). The `aur-publish` job is
skipped on prerelease (`-rc`/`-beta`) tags because AUR `pkgver` forbids hyphens.
