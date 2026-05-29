#!/usr/bin/env node
'use strict';

// Launcher for the `origin` command. npm links this onto PATH (as `origin`)
// for both global (`npm i -g originx`) and local installs. Its only job is to
// locate the native binary for this platform and hand control to it, forming
// a transparent passthrough so the TUI behaves exactly as if invoked directly.

const { spawnSync } = require('child_process');
const path = require('path');
const { currentTarget } = require('../lib/platform');
const { resolveBinary, fallbackBinaryPath } = require('../lib/locate');
const { spawnBackgroundUpdate, consumeUpdateMarker } = require('../lib/autoupdate');
const pkg = require('../package.json');

function fail(msg) {
  process.stderr.write(`origin: ${msg}\n`);
  process.exit(1);
}

const target = currentTarget();
if (!target) {
  fail(
    `unsupported platform ${process.platform}/${process.arch}.\n` +
      `  Build from source: https://github.com/Kantosaurus/origin`
  );
}

let bin = resolveBinary(target);

// Cold path: neither the per-platform optionalDependency nor the postinstall
// fallback produced a binary (e.g. `npm ci --ignore-scripts --omit=optional`).
// Fetch it synchronously through a child node process, then re-resolve.
if (!bin) {
  process.stderr.write(`origin: fetching binary for ${target.triple} (first run)…\n`);
  const dl = spawnSync(
    process.execPath,
    [path.join(__dirname, '..', 'lib', 'fetch-cli.js'), pkg.version, fallbackBinaryPath(target)],
    { stdio: 'inherit' }
  );
  if (dl.status === 0) bin = resolveBinary(target);
}

if (!bin) {
  fail(
    `could not locate or download the origin binary for ${target.triple}.\n` +
      `  Reinstall:        npm install -g ${pkg.name}@${pkg.version} --force\n` +
      `  Or build source:  https://github.com/Kantosaurus/origin`
  );
}

// Auto-update is ON by default, via the npm channel (not the binary's built-in
// cosign-verified updater, which would need the `cosign` CLI that npm machines
// rarely have, and would re-download on every launch once a release exists).
//
//   ORIGINX_ALLOW_SELF_UPDATE=<any>  use the binary's own updater instead
//   ORIGIN_NO_UPDATE / ORIGINX_NO_UPDATE = <any>  disable updates entirely
const selfUpdate = Boolean(process.env.ORIGINX_ALLOW_SELF_UPDATE);
const optedOut = Boolean(process.env.ORIGIN_NO_UPDATE || process.env.ORIGINX_NO_UPDATE);

// Announce an update that a previous background run applied (now active here).
const applied = consumeUpdateMarker();
if (applied) {
  process.stderr.write(`origin: updated to v${applied} (now active).\n`);
}

// Kick off the non-blocking npm-channel update check in the background, unless
// the user opted out or delegated to the binary's own self-updater.
if (!selfUpdate && !optedOut) {
  spawnBackgroundUpdate(pkg.version);
}

// Unless the user explicitly opted into the binary's self-updater, suppress it
// (it would overwrite npm-managed files and require cosign) and let npm own the
// update lifecycle. A user-set ORIGIN_NO_UPDATE is always respected.
const env = { ...process.env };
if (!selfUpdate && env.ORIGIN_NO_UPDATE === undefined) {
  env.ORIGIN_NO_UPDATE = '1';
}

// Attach no-op handlers so terminal-generated signals (Ctrl-C etc.) are
// handled by the child TUI rather than killing this wrapper out from under it.
// The child receives the signal directly from the tty driver (shared process
// group); these listeners only keep node alive until the child exits, at which
// point we mirror its exit status below.
for (const sig of ['SIGINT', 'SIGTERM', 'SIGHUP', 'SIGQUIT']) {
  try {
    process.on(sig, () => {});
  } catch {
    /* not all signals exist on all platforms (Windows) */
  }
}

// stdio:'inherit' hands the real controlling TTY to the binary so raw mode,
// the alternate screen, mouse capture, and SIGWINCH resize all work. spawnSync
// blocks until the child exits; we then mirror its exit code or signal.
const child = spawnSync(bin, process.argv.slice(2), {
  stdio: 'inherit',
  env,
  windowsHide: false,
});

if (child.error) {
  fail(`failed to launch ${bin}: ${child.error.message}`);
}
if (typeof child.status === 'number') {
  process.exit(child.status);
}
if (child.signal) {
  const signo = { SIGHUP: 1, SIGINT: 2, SIGQUIT: 3, SIGKILL: 9, SIGTERM: 15 };
  process.exit(128 + (signo[child.signal] || 0));
}
process.exit(0);
