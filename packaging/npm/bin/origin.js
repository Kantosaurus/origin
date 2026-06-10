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

// Auto-update is ON by default, via the binary's own self-updater: it checks
// the npm registry, downloads + sha256-verifies the matching release asset (no
// `cosign` CLI required), and swaps the binary in place. Set
// ORIGINX_ALLOW_SELF_UPDATE=0 to fall back to the npm-launcher channel
// (`npm update`-style) instead; ORIGIN_NO_UPDATE/ORIGINX_NO_UPDATE disables all
// updates.
//
//   ORIGINX_ALLOW_SELF_UPDATE=0      fall back to the npm-launcher channel
//   ORIGIN_NO_UPDATE / ORIGINX_NO_UPDATE = <any>  disable updates entirely
//
// `Boolean(process.env.X)` is truthy for "0"/"false", so parse the flag
// explicitly to honor an opt-out value.
function envBool(name, dflt) {
  const v = process.env[name];
  if (v === undefined || v === '') return dflt;
  return !/^(0|false|no|off)$/i.test(v.trim());
}
const selfUpdate = envBool('ORIGINX_ALLOW_SELF_UPDATE', true);
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

// When the user opted OUT of the binary's self-updater (ORIGINX_ALLOW_SELF_UPDATE=0),
// suppress it and let the npm channel own the update lifecycle. A user-set
// ORIGIN_NO_UPDATE is always respected.
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
