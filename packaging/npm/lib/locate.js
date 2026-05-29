'use strict';

const fs = require('fs');
const path = require('path');
const { currentTarget, binName } = require('./platform');

// Directory inside the *main* package where the postinstall / first-run
// fallback downloader stages a binary when the per-platform optionalDependency
// is unavailable. Lives in the main package so it is writable by the same user
// that installed it.
function fallbackDir() {
  return path.join(__dirname, '..', 'vendor');
}

function fallbackBinaryPath(target = currentTarget()) {
  if (!target) return null;
  return path.join(fallbackDir(), binName(target));
}

// Locate the binary shipped by the per-platform optionalDependency
// (e.g. originx-linux-x64). Resolve its package.json first — that is always
// present and never gated by an "exports" map — then look for the binary
// beside it. Returns null when the package is not installed (its os/cpu did
// not match the host, or optional deps were omitted).
function platformBinaryPath(target = currentTarget()) {
  if (!target) return null;
  try {
    const pkgJson = require.resolve(`${target.pkg}/package.json`);
    const candidate = path.join(path.dirname(pkgJson), 'bin', binName(target));
    return fs.existsSync(candidate) ? candidate : null;
  } catch {
    return null;
  }
}

// First existing binary: the platform package, then the downloaded fallback.
// Returns null when neither is present yet.
function resolveBinary(target = currentTarget()) {
  if (!target) return null;
  const fromPkg = platformBinaryPath(target);
  if (fromPkg) return fromPkg;
  const fb = fallbackBinaryPath(target);
  return fb && fs.existsSync(fb) ? fb : null;
}

module.exports = {
  fallbackDir,
  fallbackBinaryPath,
  platformBinaryPath,
  resolveBinary,
};
