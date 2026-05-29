'use strict';

// Postinstall fallback.
//
// The common path needs nothing here: the matching per-platform
// optionalDependency (e.g. originx-linux-x64) ships the binary and npm installs
// it automatically based on its os/cpu fields. This script only does work when
// that package is absent — typically because the install used
// --no-optional / --omit=optional, or the platform package was unavailable in
// the configured registry.
//
// It is strictly best-effort and NEVER fails the install: if the download does
// not succeed, the launcher (bin/origin.js) fetches the binary on first run and
// otherwise prints actionable guidance.

const { currentTarget } = require('./lib/platform');
const { resolveBinary, fallbackBinaryPath } = require('./lib/locate');
const { downloadBinary } = require('./lib/download');
const pkg = require('./package.json');

async function main() {
  // Escape hatch for air-gapped / source-build environments.
  if (process.env.ORIGINX_SKIP_DOWNLOAD) {
    return;
  }

  const target = currentTarget();
  if (!target) {
    console.warn(
      `[originx] No prebuilt binary for ${process.platform}/${process.arch}; ` +
        `build from source: https://github.com/Kantosaurus/origin`
    );
    return;
  }

  // Platform package already provided the binary — nothing to do.
  if (resolveBinary(target)) {
    return;
  }

  const dest = fallbackBinaryPath(target);
  try {
    const { bytes, verified } = await downloadBinary(pkg.version, dest, target);
    console.log(
      `[originx] Fetched origin v${pkg.version} for ${target.triple} ` +
        `(${bytes} bytes${verified ? ', sha256 verified' : ''}).`
    );
  } catch (err) {
    console.warn(`[originx] Could not pre-download the origin binary: ${err && err.message}`);
    console.warn(`[originx] It will be fetched automatically the first time you run 'origin'.`);
  }
}

main().catch((err) => {
  // Defensive: a thrown error must not break `npm install`.
  console.warn(`[originx] postinstall warning: ${err && err.message}`);
});
