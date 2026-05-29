#!/usr/bin/env node
'use strict';

// Maintainer tool: assemble (and optionally publish) the npm package family
// from a directory of release binaries.
//
// Layout produced under <out>/ (default: packaging/npm/dist/):
//   originx/                  main package — JS launcher + fallback downloader
//   originx-linux-x64/        one prebuilt binary, gated by os/cpu
//   originx-darwin-arm64/     ...
//   ... (one per built target)
//
// Usage:
//   node scripts/build.mjs --version 0.1.0 --binaries ./binaries
//   node scripts/build.mjs --version 0.1.0 --binaries ./binaries --publish
//   node scripts/build.mjs --version 0.1.0 --binaries ./binaries --publish --provenance --dry-run
//
// `--binaries <dir>` must contain the release assets named
// `origin-<triple>[.exe]` (exactly as produced by release.yml). Targets whose
// binary is missing from that dir are skipped (with a warning) and dropped
// from the main package's optionalDependencies, so partial builds still work.

import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

const require = createRequire(import.meta.url);
const { TARGETS, assetName, binName, PKG_PREFIX } = require('../lib/platform.js');

const NPM_DIR = path.resolve(fileURLToPath(import.meta.url), '..', '..'); // packaging/npm
const ROOT = path.resolve(NPM_DIR, '..', '..'); // repo root (holds LICENSE + NOTICE)
const REPO_URL = 'git+https://github.com/Kantosaurus/origin.git';
const HOMEPAGE = 'https://github.com/Kantosaurus/origin#readme';
const LICENSE = 'Apache-2.0';

function parseArgs(argv) {
  const opts = { out: path.join(NPM_DIR, 'dist'), publish: false, dryRun: false, provenance: false, tag: null };
  for (let i = 2; i < argv.length; i++) {
    const a = argv[i];
    switch (a) {
      case '--version': opts.version = argv[++i]; break;
      case '--binaries': opts.binaries = argv[++i]; break;
      case '--out': opts.out = path.resolve(argv[++i]); break;
      case '--tag': opts.tag = argv[++i]; break;
      case '--publish': opts.publish = true; break;
      case '--provenance': opts.provenance = true; break;
      case '--dry-run': opts.dryRun = true; break;
      default: throw new Error(`unknown argument: ${a}`);
    }
  }
  if (!opts.version) throw new Error('--version is required');
  if (!opts.binaries) throw new Error('--binaries <dir> is required');
  opts.binaries = path.resolve(opts.binaries);
  // Prerelease versions (1.2.3-rc.1, -beta) publish under the `next` dist-tag
  // so they never become `latest`.
  if (!opts.tag) opts.tag = /-(rc|beta|alpha)/.test(opts.version) ? 'next' : 'latest';
  return opts;
}

function writeJson(file, obj) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, JSON.stringify(obj, null, 2) + '\n');
}

function copyFile(src, dst) {
  fs.mkdirSync(path.dirname(dst), { recursive: true });
  fs.copyFileSync(src, dst);
}

function platformPkgMeta(key) {
  const [platform, cpu] = key.split(' ');
  return { platform, cpu };
}

// Build one per-platform package dir; returns the package name, or null if the
// binary for this target was not present in --binaries.
function buildPlatformPackage(out, version, binariesDir, key, target) {
  const srcBin = path.join(binariesDir, assetName(target));
  if (!fs.existsSync(srcBin)) {
    console.warn(`[build] skip ${target.pkg}: missing ${assetName(target)} in ${binariesDir}`);
    return null;
  }
  const dir = path.join(out, target.pkg);
  fs.rmSync(dir, { recursive: true, force: true });

  // The exec bit on a unix-target binary is load-bearing — locate.js hands the
  // path to spawnSync, and a 0644 binary fails with EACCES on the user's
  // machine. A Windows filesystem cannot represent that bit, so refuse to
  // assemble a unix package on a Windows host rather than silently shipping a
  // broken one. Assemble on Linux/macOS (or CI, which is Ubuntu).
  if (target.ext === '' && process.platform === 'win32') {
    throw new Error(
      `cannot assemble ${target.pkg} on a Windows host: the executable bit cannot be set ` +
        `for ${target.triple}. Assemble on Linux/macOS (or CI).`
    );
  }

  const dstBin = path.join(dir, 'bin', binName(target));
  copyFile(srcBin, dstBin);
  // Gate on the TARGET, not the build host. copyFile preserves the source mode
  // and download-artifact strips the exec bit, so unix-target binaries arrive
  // as 0644 and must be restored to 0755. (.exe targets need no exec bit.)
  if (!target.ext) fs.chmodSync(dstBin, 0o755);

  const { platform, cpu } = platformPkgMeta(key);
  writeJson(path.join(dir, 'package.json'), {
    name: target.pkg,
    version,
    description: `Prebuilt origin binary for ${platform}-${cpu} (${target.triple}).`,
    os: [platform],
    cpu: [cpu],
    // Hint to Yarn PnP that this package must be unpacked to disk (it holds a
    // native executable, not importable JS).
    preferUnplugged: true,
    files: ['bin/'],
    license: LICENSE,
    repository: { type: 'git', url: REPO_URL },
    homepage: HOMEPAGE,
  });
  // README so the npm page is not blank.
  fs.writeFileSync(
    path.join(dir, 'README.md'),
    `# ${target.pkg}\n\nPrebuilt \`origin\` binary for ${platform}-${cpu} (\`${target.triple}\`).\n\n` +
      `This is an internal platform package for [\`${PKG_PREFIX}\`](https://www.npmjs.com/package/${PKG_PREFIX}). ` +
      `Install \`${PKG_PREFIX}\` instead:\n\n\`\`\`sh\nnpm install -g ${PKG_PREFIX}\n\`\`\`\n`
  );
  // Apache-2.0 §4(a): ship the license text with every distributed package.
  copyFile(path.join(ROOT, 'LICENSE'), path.join(dir, 'LICENSE'));
  console.log(`[build] ${target.pkg}@${version}  <- ${assetName(target)}`);
  return target.pkg;
}

// Build the main package: copy the committed runtime files, then stamp the
// version + the optionalDependencies for exactly the platforms we built.
function buildMainPackage(out, version, builtPkgs) {
  const dir = path.join(out, PKG_PREFIX);
  fs.rmSync(dir, { recursive: true, force: true });

  const RUNTIME_FILES = [
    'bin/origin.js',
    'install.js',
    'lib/platform.js',
    'lib/locate.js',
    'lib/download.js',
    'lib/fetch-cli.js',
    'lib/cachepaths.js',
    'lib/autoupdate.js',
    'lib/update-check.js',
  ];
  for (const rel of RUNTIME_FILES) {
    copyFile(path.join(NPM_DIR, rel), path.join(dir, rel));
  }
  // The launcher is a POSIX-mode JS shim; always mark it executable. On Windows
  // npm regenerates its own .cmd/.ps1 shims, so a POSIX mode is harmless there.
  fs.chmodSync(path.join(dir, 'bin', 'origin.js'), 0o755);
  copyFile(path.join(NPM_DIR, 'README.md'), path.join(dir, 'README.md'));
  // Apache-2.0 §4(a) + §4(d): ship LICENSE and NOTICE with the published tarball.
  copyFile(path.join(ROOT, 'LICENSE'), path.join(dir, 'LICENSE'));
  copyFile(path.join(ROOT, 'NOTICE'), path.join(dir, 'NOTICE'));

  const optionalDependencies = {};
  for (const name of builtPkgs) optionalDependencies[name] = version;

  const base = JSON.parse(fs.readFileSync(path.join(NPM_DIR, 'package.json'), 'utf8'));
  writeJson(path.join(dir, 'package.json'), {
    ...base,
    version,
    optionalDependencies,
  });
  console.log(`[build] ${PKG_PREFIX}@${version}  (optionalDependencies: ${builtPkgs.join(', ') || 'none'})`);
  return dir;
}

function npmPublish(dir, { tag, provenance, dryRun }) {
  const args = ['publish', '--access', 'public', '--tag', tag];
  if (provenance) args.push('--provenance');
  if (dryRun) args.push('--dry-run');
  console.log(`[publish] (cwd=${path.relative(process.cwd(), dir) || '.'}) npm ${args.join(' ')}`);
  execFileSync('npm', args, { cwd: dir, stdio: 'inherit' });
}

function main() {
  const opts = parseArgs(process.argv);
  console.log(`[build] version=${opts.version} binaries=${opts.binaries} out=${opts.out} tag=${opts.tag}`);
  fs.mkdirSync(opts.out, { recursive: true });

  const built = [];
  for (const [key, target] of Object.entries(TARGETS)) {
    const name = buildPlatformPackage(opts.out, opts.version, opts.binaries, key, target);
    if (name) built.push({ name, dir: path.join(opts.out, name) });
  }
  if (built.length === 0) {
    throw new Error(`no binaries found in ${opts.binaries} (expected origin-<triple>[.exe])`);
  }
  const mainDir = buildMainPackage(opts.out, opts.version, built.map((b) => b.name));

  if (opts.publish) {
    // Publish platform packages first so the main package's optionalDependencies
    // resolve immediately for early installers.
    for (const b of built) npmPublish(b.dir, opts);
    npmPublish(mainDir, opts);
    console.log('[publish] done');
  } else {
    console.log(`[build] assembled ${built.length + 1} packages under ${opts.out} (no --publish given)`);
  }
}

try {
  main();
} catch (err) {
  console.error(`[build] error: ${err && err.message}`);
  process.exit(1);
}
