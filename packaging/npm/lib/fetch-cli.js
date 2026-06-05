'use strict';

// Tiny CLI shim: `node fetch-cli.js <version> <destPath>`
//
// The launcher (bin/origin.js) runs synchronously, so on the rare cold path
// where no binary is present it shells out to this script via spawnSync to
// perform the async download while blocking. Exit 0 on success, 1 on failure.

const { downloadBinary, downloadAuxBinaries } = require('./download');
const { currentTarget } = require('./platform');

const [, , version, dest] = process.argv;
if (!version || !dest) {
  process.stderr.write('usage: fetch-cli.js <version> <destPath>\n');
  process.exit(2);
}

downloadBinary(version, dest, currentTarget())
  .then(async ({ bytes, verified }) => {
    process.stderr.write(`origin: downloaded ${bytes} bytes${verified ? ' (sha256 verified)' : ''}\n`);
    // Best-effort: co-locate origin-daemon + origin-supervisor next to the CLI
    // so it can spawn the daemon (required) and self-dev/restart works. A
    // failure here must never fail the launch — the CLI binary is already in
    // place; only the daemon-spawn path needs these.
    try {
      const aux = await downloadAuxBinaries(version, dest, currentTarget());
      if (aux.length) {
        process.stderr.write(`origin: fetched ${aux.join(', ')}\n`);
      }
    } catch {
      /* best-effort */
    }
    process.exit(0);
  })
  .catch((err) => {
    process.stderr.write(`origin: download failed: ${err && err.message}\n`);
    process.exit(1);
  });
