# winget (Windows Package Manager) packaging

Package id: **`Kantosaurus.origin`**
Install command (once published + approved): `winget install Kantosaurus.origin`

origin ships as a single standalone `.exe`, so the winget manifest uses
`InstallerType: portable` — winget drops the binary on `PATH` and aliases it as
`origin`. The two Windows release assets it points at are:

- `origin-x86_64-pc-windows-msvc.exe` (Architecture `x64`)
- `origin-aarch64-pc-windows-msvc.exe` (Architecture `arm64`)

## Two distinct flows

The 3 manifest templates in `manifests/*.tmpl` are stamped by
`xtask/src/release.rs` (the `release` job) into `out/packaging/` with the real
`{{VERSION}}`, `{{SHA256_WIN_X64}}` and `{{SHA256_WIN_ARM}}` values and attached
to the GitHub Release. They serve **one** purpose: the one-time manual
bootstrap below. They are NOT consumed by the automated `winget-publish` job.

### 1. First submission (ONE TIME, manual)

`vedantmgoyal9/winget-releaser` **cannot create a brand-new package** — it
requires at least one existing version of `Kantosaurus.origin` in
[microsoft/winget-pkgs][pkgs] to use as a base. So the very first version
(v0.9.10) must be submitted by hand. Use either path:

**Path A — submit the stamped manifests directly (recommended; uses our
already-correct portable manifest):**

```pwsh
# download the 3 stamped manifests from the GitHub Release into a folder, e.g.
#   .\Kantosaurus.origin.yaml
#   .\Kantosaurus.origin.installer.yaml
#   .\Kantosaurus.origin.locale.en-US.yaml
winget validate --manifest .                       # local sanity check
wingetcreate submit --token <classic-PAT> .        # opens the winget-pkgs PR
```

**Path B — let wingetcreate build from the published asset URLs:**

```pwsh
wingetcreate new `
  https://github.com/Kantosaurus/origin/releases/download/v0.9.10/origin-x86_64-pc-windows-msvc.exe `
  https://github.com/Kantosaurus/origin/releases/download/v0.9.10/origin-aarch64-pc-windows-msvc.exe
# answer prompts (id Kantosaurus.origin, InstallerType portable, Commands: origin), then `submit`
```

A Microsoft reviewer (plus automated validation) must approve the PR before
`winget install Kantosaurus.origin` works. Approval latency is typically a few
hours to a few days.

### 2. Every subsequent release (AUTOMATED)

Once v0.9.10 is merged into winget-pkgs, the `winget-publish` job in
`.github/workflows/release.yml` takes over for every later tag. It runs
`vedantmgoyal9/winget-releaser`, which uses **Komac** to:

1. read the existing `Kantosaurus.origin` manifest from winget-pkgs as a base,
2. find the new release's Windows assets via `installers-regex`
   (`^origin-(x86_64|aarch64)-pc-windows-msvc\.exe$` — anchored to the CLI
   binary so it does NOT also match the `origin-daemon-*`/`origin-supervisor-*`
   `.exe` assets the release ships),
3. **download those assets and compute their SHA256 itself**, then
4. open a versioned PR against winget-pkgs from our fork.

Because Komac downloads the assets straight from the published Release and
hashes them, the checksum in the PR is verified against exactly what shipped —
the `{{SHA256_WIN_*}}` values from our stamp pipeline are NOT reused for the
automated path (they only seed the manual bootstrap manifests).

## Prerequisites for the automated job

- **Fork:** create `Kantosaurus/winget-pkgs` (fork of [microsoft/winget-pkgs][pkgs]
  under the same owner as this repo). The action pushes its branch there and
  opens the PR from it. (Or set `fork-user` to whoever owns the fork.)
- **Secret `WINGET_TOKEN`:** a **classic** PAT with the `public_repo` scope
  (fine-grained PATs are NOT supported). Set at
  `Settings → Secrets and variables → Actions`. The job auto-skips when the
  secret is absent.

[pkgs]: https://github.com/microsoft/winget-pkgs
