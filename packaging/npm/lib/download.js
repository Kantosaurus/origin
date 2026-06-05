'use strict';

const fs = require('fs');
const path = require('path');
const crypto = require('crypto');
const {
  currentTarget,
  assetName,
  RELEASES_REPO,
  AUX_BINS,
  auxAssetName,
  auxBinName,
} = require('./platform');

const USER_AGENT = 'originx-installer';

// GitHub release download URL for a target's binary. The release tag is
// `v<version>` (see .github/workflows/release.yml: `on: push: tags: ["v*"]`).
function releaseAssetUrl(version, target) {
  return `https://github.com/${RELEASES_REPO}/releases/download/v${version}/${assetName(target)}`;
}

// URL of the optional SHA256SUMS manifest published alongside the binaries.
function checksumsUrl(version) {
  return `https://github.com/${RELEASES_REPO}/releases/download/v${version}/SHA256SUMS`;
}

// Fetch `url` into `destPath` atomically (temp file + rename). Verifies a
// non-empty body and, when the server advertises Content-Length, that the body
// is complete. Uses the global fetch (Node >= 18), which follows the redirect
// GitHub issues to its asset CDN.
async function downloadTo(url, destPath) {
  const res = await fetch(url, {
    redirect: 'follow',
    headers: { 'user-agent': USER_AGENT, accept: 'application/octet-stream' },
  });
  if (!res.ok) {
    throw new Error(`GET ${url} -> HTTP ${res.status}`);
  }
  const buf = Buffer.from(await res.arrayBuffer());
  if (buf.length === 0) {
    throw new Error(`downloaded 0 bytes from ${url}`);
  }
  const declared = Number(res.headers.get('content-length'));
  if (Number.isFinite(declared) && declared > 0 && declared !== buf.length) {
    throw new Error(`truncated download from ${url}: got ${buf.length} of ${declared} bytes`);
  }
  fs.mkdirSync(path.dirname(destPath), { recursive: true });
  const tmp = `${destPath}.download`;
  fs.writeFileSync(tmp, buf);
  fs.renameSync(tmp, destPath);
  return buf;
}

// Best-effort fetch of the SHA256SUMS manifest. Returns its text, or null if
// the release does not publish one / it is unreachable.
async function fetchChecksums(version) {
  try {
    const res = await fetch(checksumsUrl(version), {
      redirect: 'follow',
      headers: { 'user-agent': USER_AGENT },
    });
    if (!res.ok) return null;
    return await res.text();
  } catch {
    return null;
  }
}

// Extract the expected lowercase sha256 for `fileName` from sha256sum-style
// manifest text (`<hex>  <name>` or `<hex> *<name>`). Returns null if absent.
function expectedHashFor(sumsText, fileName) {
  if (!sumsText) return null;
  for (const line of sumsText.split(/\r?\n/)) {
    const m = line.trim().match(/^([0-9a-fA-F]{64})\s+\*?(.+)$/);
    if (m && path.basename(m[2]) === fileName) return m[1].toLowerCase();
  }
  return null;
}

function sha256(buf) {
  return crypto.createHash('sha256').update(buf).digest('hex');
}

// Download the platform binary for `version` into `destPath`. When a
// SHA256SUMS manifest is available, the binary's hash MUST match (corruption /
// tamper guard); when no manifest is available we proceed on TLS integrity
// alone (the stronger cosign + SLSA verification is what the in-binary updater
// uses for subsequent updates). Marks the file executable on unix.
async function downloadBinary(version, destPath, target = currentTarget()) {
  if (!target) {
    throw new Error(`unsupported platform: ${process.platform}/${process.arch}`);
  }
  const url = releaseAssetUrl(version, target);
  const buf = await downloadTo(url, destPath);

  const sums = await fetchChecksums(version);
  const expected = expectedHashFor(sums, assetName(target));
  if (expected) {
    const actual = sha256(buf);
    if (actual !== expected) {
      fs.rmSync(destPath, { force: true });
      throw new Error(`checksum mismatch for ${assetName(target)}: expected ${expected}, got ${actual}`);
    }
  }

  if (process.platform !== 'win32') {
    fs.chmodSync(destPath, 0o755);
  }
  return { url, bytes: buf.length, verified: Boolean(expected) };
}

// Download the auxiliary binaries (origin-daemon, origin-supervisor) into the
// SAME directory as `mainDestPath`, so the CLI's sibling lookup finds them on
// the cold (fallback) path just as it would in a per-platform package. Each is
// best-effort: a missing/failed aux asset is reported but does not throw, since
// only daemon spawn / self-dev need them and the main binary still runs. When a
// SHA256SUMS manifest is present each download must match it. Returns the names
// successfully fetched.
async function downloadAuxBinaries(version, mainDestPath, target = currentTarget()) {
  if (!target) return [];
  const dir = path.dirname(mainDestPath);
  const sums = await fetchChecksums(version);
  const fetched = [];
  for (const name of AUX_BINS) {
    const asset = auxAssetName(name, target);
    const dest = path.join(dir, auxBinName(name, target));
    const url = `https://github.com/${RELEASES_REPO}/releases/download/v${version}/${asset}`;
    try {
      const buf = await downloadTo(url, dest);
      const expected = expectedHashFor(sums, asset);
      if (expected && sha256(buf) !== expected) {
        fs.rmSync(dest, { force: true });
        throw new Error(`checksum mismatch for ${asset}`);
      }
      if (process.platform !== 'win32') {
        fs.chmodSync(dest, 0o755);
      }
      fetched.push(name);
    } catch (err) {
      process.stderr.write(`origin: optional ${name} not fetched: ${err && err.message}\n`);
    }
  }
  return fetched;
}

module.exports = {
  releaseAssetUrl,
  checksumsUrl,
  downloadTo,
  fetchChecksums,
  expectedHashFor,
  sha256,
  downloadBinary,
  downloadAuxBinaries,
};
