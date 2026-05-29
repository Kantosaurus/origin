'use strict';

// Background auto-updater (npm channel).
//
// The launcher (bin/origin.js) spawns this DETACHED and unref'd before handing
// control to the TUI, so it adds zero startup latency. It checks the npm
// registry (honoring the user's .npmrc, so private registries work) for a newer
// `originx`, and if found — and this is a global install — runs
// `npm install -g originx@<latest>` in the background. The update takes effect
// on the next launch; a marker file lets the launcher announce it once.
//
// This deliberately does NOT use the native binary's built-in self-updater:
// that path requires the `cosign` CLI (absent on typical npm machines) and, once
// a newer release exists, re-downloads + re-fails on every launch. Keeping npm
// as the update channel avoids node_modules/version-metadata drift entirely.
//
// Everything here is best-effort and fully self-contained: it never throws to
// the parent (it runs detached anyway) and rate-limits via a 24h cache so it
// cannot storm the registry or retry a failing install in a loop.
//
// Run as: node update-check.js <currentVersion>

const fs = require('fs');
const path = require('path');
const { execFileSync } = require('child_process');
const { cacheDir, cacheFile, lockDir, markerFile, logFile } = require('./cachepaths');

const PKG = require('../package.json');
const PKG_NAME = PKG.name; // "originx"

const CHECK_TTL_MS = 24 * 60 * 60 * 1000; // re-check at most once per day
const LOCK_STALE_MS = 60 * 60 * 1000; // a lock older than 1h is presumed dead
const IS_WIN = process.platform === 'win32';
const NPM = 'npm';
// On Windows `npm` is a .cmd shim, which child_process cannot execute without a
// shell. Safe to enable here because every npm arg below is a fixed string or a
// SEMVER_RE-validated version — never unsanitized input.
const SEMVER_RE = /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/;

// ---- helpers ---------------------------------------------------------------

function log(msg) {
  try {
    fs.mkdirSync(cacheDir(), { recursive: true });
    // Single, truncated log so it never grows unbounded.
    fs.writeFileSync(logFile(), `[${new Date().toISOString()}] ${msg}\n`);
  } catch {
    /* ignore */
  }
}

// Compare two validated semver strings. Returns true iff a > b. A stable
// release outranks any prerelease of the same x.y.z (no prerelease auto-update
// onto a stable line, and no stable->prerelease).
function semverGt(a, b) {
  const parse = (s) => {
    const [core, pre] = s.split('+')[0].split('-');
    const nums = core.split('.').map(Number);
    return { nums, pre: pre || null };
  };
  const pa = parse(a);
  const pb = parse(b);
  for (let i = 0; i < 3; i++) {
    if (pa.nums[i] !== pb.nums[i]) return pa.nums[i] > pb.nums[i];
  }
  if (pa.pre === pb.pre) return false;
  if (pa.pre === null) return true; // a is stable, b is prerelease of same core
  if (pb.pre === null) return false; // b is stable
  return pa.pre > pb.pre; // both prereleases: lexical
}

function readJson(file) {
  try {
    return JSON.parse(fs.readFileSync(file, 'utf8'));
  } catch {
    return null;
  }
}

function writeJson(file, obj) {
  try {
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, JSON.stringify(obj));
  } catch {
    /* ignore */
  }
}

function npm(args, timeoutMs) {
  return execFileSync(NPM, args, {
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
    timeout: timeoutMs,
    windowsHide: true,
    shell: IS_WIN,
  }).trim();
}

// Is this package installed into npm's GLOBAL prefix?
function isGlobalInstall() {
  if (process.env.ORIGINX_FORCE_GLOBAL === '1') return true; // test hook
  try {
    const root = npm(['root', '-g'], 15000);
    const pkgDir = path.resolve(__dirname, '..'); // .../node_modules/originx
    const real = (p) => {
      try {
        return fs.realpathSync(p);
      } catch {
        return path.resolve(p);
      }
    };
    const rootReal = real(root);
    // Match on a path boundary so a sibling like `<root>-other` can't false-match.
    return real(pkgDir) === rootReal || real(pkgDir).startsWith(rootReal + path.sep);
  } catch (e) {
    log(`isGlobalInstall: npm root -g failed: ${e.message}`);
    return false;
  }
}

// For a project-local install, resolve the project root (the directory whose
// `node_modules` contains us) so we can run `npm install` there. Returns null
// for layouts where a plain `npm install` would be wrong or unsafe:
//   - scoped/hoisted nesting where our parent isn't literally `node_modules`
//   - a project dir that is ITSELF inside another `node_modules` (pnpm virtual
//     store, hoisted transitive dep) — updating there would corrupt the store
//   - no package.json at the resolved root (not a real project)
// This intentionally covers only the clean `<project>/node_modules/originx`
// case; exotic layouts fall through to "skip" rather than risk damage.
function localProjectDir() {
  if (process.env.ORIGINX_FORCE_PROJECT_DIR) return process.env.ORIGINX_FORCE_PROJECT_DIR; // test hook
  const pkgDir = path.resolve(__dirname, '..'); // .../node_modules/originx
  const parent = path.dirname(pkgDir); // expected: .../node_modules
  if (path.basename(parent) !== 'node_modules') return null;
  const proj = path.dirname(parent);
  if (proj.split(path.sep).includes('node_modules')) return null; // nested store
  try {
    if (!fs.existsSync(path.join(proj, 'package.json'))) return null;
  } catch {
    return null;
  }
  return proj;
}

// Acquire an exclusive lock via atomic mkdir. A stale lock (>1h, presumed dead
// holder) is reclaimed atomically: remove + re-mkdir, so if two racers both see
// it as stale only one's mkdir wins (the other gets EEXIST and backs off).
function acquireLock() {
  const dir = lockDir();
  // Ensure the parent cache dir exists; the lock mkdir below is intentionally
  // non-recursive (that atomicity is the lock), so the parent must pre-exist.
  try {
    fs.mkdirSync(cacheDir(), { recursive: true });
  } catch {
    /* ignore — the lock mkdir will surface a real problem */
  }
  try {
    fs.mkdirSync(dir, { recursive: false });
    return true;
  } catch (e) {
    if (e.code !== 'EEXIST') return false;
    try {
      const age = Date.now() - fs.statSync(dir).mtimeMs;
      if (age <= LOCK_STALE_MS) return false; // a live holder owns it
      fs.rmSync(dir, { recursive: true, force: true });
      fs.mkdirSync(dir, { recursive: false }); // throws EEXIST if another racer won
      return true;
    } catch {
      return false;
    }
  }
}
function releaseLock() {
  try {
    fs.rmSync(lockDir(), { recursive: true, force: true });
  } catch {
    /* ignore */
  }
}

// ---- main ------------------------------------------------------------------

function main() {
  const current = process.argv[2];
  if (!current || !SEMVER_RE.test(current)) {
    log(`bad/absent current version: ${current}`);
    return;
  }

  // Opt-out (mirrors the env knobs the launcher documents).
  if (process.env.ORIGIN_NO_UPDATE || process.env.ORIGINX_NO_UPDATE) {
    return;
  }

  // Rate-limit: at most one real check per day.
  const cache = readJson(cacheFile());
  if (cache && typeof cache.ts === 'number' && Date.now() - cache.ts < CHECK_TTL_MS) {
    return;
  }

  // Single-flight: only one worker per machine performs the check+update. A
  // launch that can't get the lock bails instantly — this both serializes the
  // `npm install` and prevents an `npm view` storm when N launches fire before
  // the first one has written the 24h cache.
  if (!acquireLock()) {
    return;
  }
  try {
    runCheck(current);
  } finally {
    releaseLock();
  }
}

// Body of a single update check, run under the lock. Records the 24h cache on
// every terminal outcome so a transient failure backs off rather than retrying
// every launch.
function runCheck(current) {
  // Find the latest published version (respects the user's npm registry config).
  let latest = process.env.ORIGINX_FORCE_LATEST; // test hook
  if (!latest) {
    try {
      latest = npm(['view', `${PKG_NAME}@latest`, 'version'], 20000);
    } catch (e) {
      // Not published / offline / registry error — record the attempt so we
      // back off for 24h, then bail quietly.
      writeJson(cacheFile(), { ts: Date.now(), latest: null });
      log(`npm view failed: ${e.message}`);
      return;
    }
  }
  if (!latest || !SEMVER_RE.test(latest)) {
    writeJson(cacheFile(), { ts: Date.now(), latest: latest || null });
    log(`unusable latest version from registry: ${latest}`);
    return;
  }

  writeJson(cacheFile(), { ts: Date.now(), latest });

  if (!semverGt(latest, current)) {
    return; // already current (or ahead, e.g. a prerelease)
  }

  // Pick the install command for how THIS copy is installed:
  //   global -> `npm install -g originx@latest`
  //   local  -> `npm install originx@latest --no-save` in the project root.
  //             --no-save updates the working copy without rewriting the
  //             project's declared dependency in package.json (a clean
  //             `npm ci` would later restore the pinned version).
  const spec = `${PKG_NAME}@${latest}`;
  let mode;
  let cwd;
  let args;
  if (isGlobalInstall()) {
    mode = 'global';
    args = ['install', '-g', spec];
  } else {
    const proj = localProjectDir();
    if (!proj) {
      log(`update ${current} -> ${latest} available but install layout not recognized; skipping`);
      return;
    }
    mode = 'local';
    cwd = proj;
    args = ['install', spec, '--no-save'];
  }

  try {
    if (process.env.ORIGINX_UPDATE_DRY_RUN === '1') {
      log(`[dry-run] would run (${mode}${cwd ? ` @ ${cwd}` : ''}): npm ${args.join(' ')}`);
    } else {
      log(`updating ${current} -> ${latest} (${mode}): npm ${args.join(' ')}`);
      // Long timeout: a fresh binary download can take a while.
      const opts = { stdio: 'ignore', timeout: 10 * 60 * 1000, windowsHide: true, shell: IS_WIN };
      if (cwd) opts.cwd = cwd;
      execFileSync(NPM, args, opts);
    }
    // Announce on next launch.
    writeJson(markerFile(), { version: latest, ts: Date.now() });
    log(`updated to ${latest}`);
  } catch (e) {
    // EACCES (root-owned global prefix), EBUSY (Windows, replacing a running
    // .exe), network, etc. The 24h cache (already written) prevents a retry
    // storm; the user can still update manually.
    log(`npm install failed: ${e.message}`);
  }
}

// Only run when executed directly (the launcher spawns this as a CLI). Allows
// other modules / tests to require it without triggering a check.
if (require.main === module) {
  try {
    main();
  } catch (e) {
    log(`unexpected: ${e && e.message}`);
  }
}

module.exports = { semverGt, isGlobalInstall, localProjectDir, main };
