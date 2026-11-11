# CloakBrowser sidecar

Node ≥18 sidecar that exposes CloakBrowser through the same stdio-JSON verb
protocol that `agent-browser` speaks. `origin-browser`'s router spawns it only
when the stealth fallback is enabled (`ORIGIN_STEALTH_FALLBACK=1`); you do not
invoke it manually.

Install its dependencies once, in this directory (origin does not run this):

    npm install --omit=dev

The router then `node` runs `cloak-cli.mjs`.
