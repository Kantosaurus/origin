'use strict';

// Launcher-side helpers for the npm-channel auto-updater. The heavy lifting
// happens in update-check.js, which this module spawns detached so it never
// blocks the TUI.

const fs = require('fs');
const path = require('path');
const { spawn } = require('child_process');
const { markerFile } = require('./cachepaths');

// Fire-and-forget the background update worker. Fully detached + unref'd so it
// outlives this process and adds zero startup latency. Never throws.
function spawnBackgroundUpdate(currentVersion) {
  try {
    const child = spawn(process.execPath, [path.join(__dirname, 'update-check.js'), String(currentVersion)], {
      detached: true,
      stdio: 'ignore',
      windowsHide: true,
    });
    child.unref();
  } catch {
    /* best-effort; updates are not load-bearing for launching */
  }
}

// If a previous background run applied an update, return the version it
// installed (and clear the marker) so the launcher can announce it once.
// Returns null when there is nothing to announce.
function consumeUpdateMarker() {
  try {
    const f = markerFile();
    const m = JSON.parse(fs.readFileSync(f, 'utf8'));
    fs.rmSync(f, { force: true });
    return m && typeof m.version === 'string' ? m.version : null;
  } catch {
    return null;
  }
}

module.exports = { spawnBackgroundUpdate, consumeUpdateMarker };
