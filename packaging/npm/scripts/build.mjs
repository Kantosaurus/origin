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
// `origin-<triple>[.exe]` (exactly as produced by release.yml). It SHOULD also
// contain the co-shipped `origin-daemon-<triple>` and `origin-supervisor-<triple>`
// assets, which are bundled next to `origin` in each platform package so the
// CLI's sibling lookup can spawn the daemon / self-dev supervisor; a missing aux
// asset is a warning, not a failure. Targets whose MAIN binary is missing are
// skipped (with a warning) and dropped from the main package's
// optionalDependencies, so partial builds still work.
//
// Publishing is IDEMPOTENT: in --publish mode each package whose exact
// `name@version` is already on the registry is skipped (npm forbids
// re-publishing a version). This makes a re-run safe after a partial failure —
// it completes only the packages that didn't make it. The skip happens BEFORE
// assembly, so an already-published unix package never trips the Windows-host
// assembly guard, letting a Windows maintainer finish a release whose unix
// packages already shipped.

import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

const require = createRequire(import.meta.url);
const { TARGETS, assetName, binName, PKG_PREFIX, AUX_BINS, auxAssetName, auxBinName } =
  require('../lib/platform.js');

const NPM_DIR = path.resolve(fileURLToPath(import.meta.url), '..', '..'); // packaging/npm
const ROOT = path.resolve(NPM_DIR, '..', '..'); // repo root (holds LICENSE + NOTICE)
const REPO_URL = 'git+https://github.com/Kantosaurus/origin.git';
const HOMEPAGE = 'https://github.com/Kantosaurus/origin#readme';
const LICENSE = 'Apache-2.0';

// On Windows `npm` is a `.cmd` shim that execFileSync cannot launch without a
// shell. Mirror the repo's own convention (packaging/npm/lib/update-check.js).
// Every npm arg below is a fixed token or a controlled name/version, never
// unsanitized input, so enabling the shell here is safe.
const IS_WIN = process.platform === 'win32';

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

// True iff `name@version` is already on the configured registry. Used to make
// publishing idempotent. A 404 (unpublished name), an existing package without
// this version, or any network/registry error all return false, so callers
// fall through to a normal publish attempt rather than wrongly skipping.
//   `npm view foo@x.y.z version` -> prints "x.y.z" when present; prints nothing
//   when the package exists but lacks that version; exits non-zero on a 404.
function publishedVersionExists(name, version) {
  try {
    const out = execFileSync('npm', ['view', `${name}@${version}`, 'version'], {
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
      shell: IS_WIN,
    }).trim();
    return out === version;
  } catch {
    return false;
  }
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

  // Co-ship the daemon (required: the CLI spawns it as a separate process) and
  // the supervisor (self-dev hot-reload + crash-restart) next to `origin`, so
  // the CLI's sibling lookup resolves them. A missing aux asset is a loud
  // warning, not a failure: the CLI still launches; only daemon spawn / self-dev
  // need them, and a partial build should still yield an installable package.
  for (const aux of AUX_BINS) {
    const auxSrc = path.join(binariesDir, auxAssetName(aux, target));
    if (!fs.existsSync(auxSrc)) {
      console.warn(
        `[build] ${target.pkg}: missing ${auxAssetName(aux, target)} in ${binariesDir} ` +
          `(CLI sibling lookup will not find ${aux})`
      );
      continue;
    }
    const auxDst = path.join(dir, 'bin', auxBinName(aux, target));
    copyFile(auxSrc, auxDst);
    if (!target.ext) fs.chmodSync(auxDst, 0o755);
    console.log(`[build] ${target.pkg}: + ${auxBinName(aux, target)}`);
  }

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
// version + the optionalDependencies. `familyPkgs` is every platform package
// that will exist at this version after the run (built this run OR already on
// the registry), so an idempotent completion still yields a main manifest that
// references the whole family — not just the packages this run rebuilt.
function buildMainPackage(out, version, familyPkgs) {
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

  // Bake the resolved package-family prefix into the SHIPPED platform.js as a
  // literal, so the published launcher resolves its binary by a fixed name and
  // never reads an end user's ORIGIN_NPM_PREFIX. (We honor that env var only
  // here, at build time.) Guarded: a future rename of this line fails the build
  // loudly rather than silently shipping an env-dependent runtime.
  const shippedPlatform = path.join(dir, 'lib', 'platform.js');
  const platformSrc = fs.readFileSync(shippedPlatform, 'utf8');
  const PREFIX_LINE = /^const PKG_PREFIX = .*$/m;
  if (!PREFIX_LINE.test(platformSrc)) {
    throw new Error(`buildMainPackage: PKG_PREFIX line not found to bake in ${shippedPlatform}`);
  }
  fs.writeFileSync(
    shippedPlatform,
    platformSrc.replace(PREFIX_LINE, `const PKG_PREFIX = ${JSON.stringify(PKG_PREFIX)};`)
  );

  copyFile(path.join(NPM_DIR, 'README.md'), path.join(dir, 'README.md'));
  // Apache-2.0 §4(a) + §4(d): ship LICENSE and NOTICE with the published tarball.
  copyFile(path.join(ROOT, 'LICENSE'), path.join(dir, 'LICENSE'));
  copyFile(path.join(ROOT, 'NOTICE'), path.join(dir, 'NOTICE'));

  const optionalDependencies = {};
  for (const name of familyPkgs) optionalDependencies[name] = version;

  const base = JSON.parse(fs.readFileSync(path.join(NPM_DIR, 'package.json'), 'utf8'));
  // Drop `//`-prefixed comment keys (JSON has no comments; the committed
  // manifest uses them to document maintainer intent) so they never ship in
  // the published tarball.
  for (const k of Object.keys(base)) {
    if (k.startsWith('//')) delete base[k];
  }
  writeJson(path.join(dir, 'package.json'), {
    ...base,
    // Follow PKG_PREFIX so an ORIGIN_NPM_PREFIX override renames the main
    // package too (not just the platform packages + optionalDependencies).
    name: PKG_PREFIX,
    version,
    optionalDependencies,
  });
  console.log(`[build] ${PKG_PREFIX}@${version}  (optionalDependencies: ${familyPkgs.join(', ') || 'none'})`);
  return dir;
}

function npmPublish(dir, name, { tag, provenance, dryRun, version }) {
  const args = ['publish', '--access', 'public', '--tag', tag];
  if (provenance) args.push('--provenance');
  if (dryRun) args.push('--dry-run');
  console.log(`[publish] (cwd=${path.relative(process.cwd(), dir) || '.'}) npm ${args.join(' ')}`);
  try {
    execFileSync('npm', args, { cwd: dir, stdio: 'inherit', shell: IS_WIN });
  } catch (err) {
    // Idempotent recovery: if the publish failed only because this exact
    // name@version is already on the registry (a prior partial run, or a
    // concurrent racer, beat us to it), treat it as done. Re-probe instead of
    // parsing npm's error wording, which varies across versions.
    if (!dryRun && version && publishedVersionExists(name, version)) {
      console.log(`[skip] ${name}@${version} already published; continuing`);
      return;
    }
    throw err;
  }
}

function main() {
  const opts = parseArgs(process.argv);
  console.log(`[build] version=${opts.version} binaries=${opts.binaries} out=${opts.out} tag=${opts.tag}`);
  fs.mkdirSync(opts.out, { recursive: true });

  // Idempotency probe runs whenever publishing (including --dry-run, so a dry
  // run accurately previews the skip behavior). It's a read-only `npm view`.
  // Assemble-only runs skip it and build every present binary, for inspection.
  const checkPublished = opts.publish;

  const built = [];
  const willExist = new Set(); // platform packages that exist at this version after the run
  for (const [key, target] of Object.entries(TARGETS)) {
    // Skip BEFORE assembly so an already-published unix package never trips the
    // Windows-host assembly guard (lets a Windows maintainer finish a release
    // whose unix packages already shipped).
    if (checkPublished && publishedVersionExists(target.pkg, opts.version)) {
      console.log(`[skip] ${target.pkg}@${opts.version} already published`);
      willExist.add(target.pkg);
      continue;
    }
    const name = buildPlatformPackage(opts.out, opts.version, opts.binaries, key, target);
    if (name) {
      built.push({ name, dir: path.join(opts.out, name) });
      willExist.add(name);
    }
  }
  if (willExist.size === 0) {
    throw new Error(`no binaries found in ${opts.binaries} (expected origin-<triple>[.exe])`);
  }

  // optionalDependencies = the whole family that exists at this version, in
  // TARGETS order (built this run ∪ already on the registry).
  const familyPkgs = Object.values(TARGETS)
    .map((t) => t.pkg)
    .filter((p) => willExist.has(p));
  const mainDir = buildMainPackage(opts.out, opts.version, familyPkgs);

  if (opts.publish) {
    // Publish platform packages first so the main package's optionalDependencies
    // resolve immediately for early installers.
    for (const b of built) npmPublish(b.dir, b.name, opts);
    if (checkPublished && publishedVersionExists(PKG_PREFIX, opts.version)) {
      console.log(`[skip] ${PKG_PREFIX}@${opts.version} already published`);
    } else {
      npmPublish(mainDir, PKG_PREFIX, opts);
    }
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
