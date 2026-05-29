'use strict';

// Single source of truth for how Node's (platform, arch) maps onto the
// published artifacts. Both the runtime (launcher + postinstall) and the
// maintainer build script (scripts/build.mjs) consume this table, so the
// npm side can never drift from itself.
//
// Keep this in lockstep with two other places:
//   * .github/workflows/release.yml         — the build matrix / asset names
//   * crates/origin-cli/src/updater.rs       — current_target_asset_name()
// All three must agree on the target triples and the `origin-<triple>[.exe]`
// asset naming, or the download fallback / in-binary updater break.

// The npm package family name: the main package's name, and the prefix of the
// six per-platform packages (`${PKG_PREFIX}-<plat>-<arch>`). The *command*
// installed onto PATH is always `origin` regardless of this (see the main
// package's `bin` field).
//
// Overridable at PUBLISH time via `ORIGIN_NPM_PREFIX` so a publish blocked by
// npm's name-similarity filter can switch to the scoped `@kantosaurus/origin`
// family without hand-editing names across files. `scripts/build.mjs` reads
// this value when assembling, then bakes the *resolved* literal into the
// published copy of this file — so an end user's environment can never alter
// which package the launcher resolves the binary from.
const PKG_PREFIX = process.env.ORIGIN_NPM_PREFIX || 'originx';

// GitHub repository that hosts the release artifacts. Must match the Rust
// updater's RELEASES_REPO and the git remote.
const RELEASES_REPO = 'Kantosaurus/origin';

// platform+arch -> { triple, pkg, ext }
//   triple : Rust target triple, used in the GitHub release asset name
//   pkg    : per-platform optionalDependency carrying that one binary
//   ext    : executable extension ("" on unix, ".exe" on windows)
const TARGETS = {
  'linux x64': { triple: 'x86_64-unknown-linux-gnu', pkg: `${PKG_PREFIX}-linux-x64`, ext: '' },
  'linux arm64': { triple: 'aarch64-unknown-linux-gnu', pkg: `${PKG_PREFIX}-linux-arm64`, ext: '' },
  'darwin x64': { triple: 'x86_64-apple-darwin', pkg: `${PKG_PREFIX}-darwin-x64`, ext: '' },
  'darwin arm64': { triple: 'aarch64-apple-darwin', pkg: `${PKG_PREFIX}-darwin-arm64`, ext: '' },
  'win32 x64': { triple: 'x86_64-pc-windows-msvc', pkg: `${PKG_PREFIX}-win32-x64`, ext: '.exe' },
  'win32 arm64': { triple: 'aarch64-pc-windows-msvc', pkg: `${PKG_PREFIX}-win32-arm64`, ext: '.exe' },
};

function targetKey(platform, arch) {
  return `${platform} ${arch}`;
}

// Resolve the descriptor for the current (or a given) host. Returns null for
// platform/arch combinations we do not publish a binary for.
function currentTarget(platform = process.platform, arch = process.arch) {
  return TARGETS[targetKey(platform, arch)] || null;
}

// Release asset file name, e.g. `origin-x86_64-apple-darwin` or
// `origin-x86_64-pc-windows-msvc.exe`.
function assetName(target) {
  return `origin-${target.triple}${target.ext}`;
}

// On-disk binary name inside a package's bin/ directory.
function binName(target) {
  return `origin${target.ext}`;
}

module.exports = {
  PKG_PREFIX,
  RELEASES_REPO,
  TARGETS,
  targetKey,
  currentTarget,
  assetName,
  binName,
};
