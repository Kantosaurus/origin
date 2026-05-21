# CloakBrowser sidecar

Node ≥18 sidecar that exposes CloakBrowser through the same stdio-JSON verb
protocol that `agent-browser` speaks. `origin-browser`'s router spawns this
on first use; you do not invoke it manually.

First-use bootstrap (origin runs this for you):

    npm install --omit=dev

The router then `node` runs `cloak-cli.mjs`.
