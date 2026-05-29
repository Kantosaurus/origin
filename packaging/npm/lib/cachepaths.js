'use strict';

const os = require('os');
const path = require('path');

// Per-user cache directory for the npm-channel auto-updater's state (check
// timestamp, lock, "update applied" marker, log). Kept separate from the
// native binary's own $ORIGIN_HOME/.origin cache so the two never collide.
function cacheRoot() {
  if (process.platform === 'win32') {
    return process.env.LOCALAPPDATA || path.join(os.homedir(), 'AppData', 'Local');
  }
  return process.env.XDG_CACHE_HOME || path.join(os.homedir(), '.cache');
}

function cacheDir() {
  return path.join(cacheRoot(), 'originx');
}

module.exports = {
  cacheDir,
  cacheFile: () => path.join(cacheDir(), 'update-check.json'),
  lockDir: () => path.join(cacheDir(), 'update.lock'),
  markerFile: () => path.join(cacheDir(), 'update-applied.json'),
  logFile: () => path.join(cacheDir(), 'update.log'),
};
