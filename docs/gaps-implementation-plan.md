# Gaps implementation plan

Classification of every item under "Has that origin lacks" / "Does better than origin"
in `Gaps.md`. Scope rule from the user: implement everything **except** GUI apps,
IDE/editor extensions, and remote/phone apps. `EXCL` = out of scope (with reason).
`META` = not a code feature (maturity/adoption/docs). Everything else is to be built.

Cross-peer features are **deduplicated into shared modules** (a single mechanism
satisfies the same gap across multiple peers).

## Shared implementation modules (deduped)

| Module | Crate / location | Satisfies (peer · item) |
|---|---|---|
| Cost & usage accounting | `origin-cost` | aider /tokens; claude /usage,/insights; jcode cache-cost; kilo microdollar |
| Model routing | `origin-router` | aider architect/editor; oc SmartRouter+routing+recommend; gemini auto-route; kilo virtual-quota |
| Telemetry / OTel | `origin-telemetry` (+ existing otel exporter) | aider analytics; gemini/cline OTel; jcode telemetry; oc verify:privacy |
| Scheduler / cron / loops / webhooks | `origin-schedule` | claude /schedule+/loop; cline cron; kilo Triggers; opencode cron |
| Voice / dictation | `origin-voice` | aider /voice; claude voice; jcode dictate; kilo STT |
| Post-edit lint/test/format | `origin-postedit` | aider auto-lint/test; opencode formatters |
| Edit-format matrix + apply_patch | `origin-edit` (apply_patch exists) | aider edit-formats; opencode apply_patch |
| Repo map + structure-aware grep | extend `origin-codegraph` | aider repomap+PageRank; jcode agentgrep; kilo/oc semantic |
| Conversation export | `origin-export` | openclaude /export; opencode share(local) |
| Doctor / diagnostics / onboarding | `origin-doctor` | openclaude doctor:runtime + onboarding |
| Git safety: checkpoint/undo/rewind/lanes | `origin-vcs` | aider git; cline/kilo checkpoints; gemini rewind; jcode lanes |
| Permission hardening + ConSeca | extend `origin-permission` | claude parser-hardening; gemini ConSeca |
| Hooks expansion (11 events + aliases) | extend `origin-hooks` | gemini 11 events; claude MessageDisplay |
| Multi-agent confidence review + triage | `origin-review` | claude/kilo/oc review; auto-triage |
| Teams / dynamic workflows / subagents | extend `origin-swarm` (+ daemon) | claude workflows+teams; cline teams+subagents; gemini md-subagents; kilo Gastown; oc scout |
| Plugin system + packaging + live interop | `origin-plugin` | claude marketplace; gemini gallery; cline/oc npm plugins; kilo/oc live .claude |
| Governance / policy / RBAC / managed cfg | `origin-policy` | claude MDM; gemini policy tiers; cline/kilo/oc enterprise (minus SSO) |
| GitHub/GitLab CI automation | `.github` + `origin run` headless | claude/gemini/kilo/oc bots+actions |
| Headless: stream-json + json-schema | extend `origin-cli/headless` + daemon | claude/gemini/openclaude/oc structured output |
| Web search + grounding + scrape | extend `origin-browser` tools | aider /web; gemini grounded; openclaude DDG; oc |
| Multimodal input (image/PDF) | extend core + tools | aider images; gemini PDF/sketch; claude |
| Notifications (email/webhook/desktop) | `origin-notify` | jcode multi-channel; cline (out-of-band part) |
| Ambient / overnight / self-dev | `origin-ambient` | jcode ambient/overnight/selfdev |
| Provider breadth + runtime discovery + shim | extend `origin-provider*` | kilo/oc breadth; openclaude descriptors+shim; LiteLLM |
| Knowledge / semantic index | `origin-knowledge` | openclaude orama; kilo/oc semantic_search |
| HTTP cassette recording | `origin-cassette` | opencode http-recorder |
| LSP auto-install fleet | extend `origin-lsp-client` | opencode 40+ LSP |
| Cross-harness live resume | extend `origin-migrate` | jcode/oc/kilo live resume + interop |
| i18n | `origin-i18n` | kilo ~21 langs; opencode i18n |
| Mermaid renderer | `origin-mermaid` | jcode mermaid |
| Watch-files (AI comments) | `origin-watch` | aider --watch-files |
| Clipboard / copy-paste mode | `origin-clipboard` | aider copy/paste |
| TUI: vim, themes, steering, focus-chain, output-styles, keybind cfg | extend `origin-tui`/`origin-cli` | aider/claude/cline/kilo |
| Reasoning controls + effort/fast + aliases | extend provider + cli | aider reasoning; claude effort/fast |
| Auth: WIF OIDC + provider logins + Gmail | extend `origin-keyvault` | claude WIF; jcode logins+gmail |
| Multi-root workspace | extend daemon session | cline multiroot |
| Local agent federation (A2A-local) | extend swarm | gemini A2A (local part only) |
| Repo clone / scout dep research | `origin-tools` builtin | opencode scout |

## Excluded (GUI / IDE / remote / phone) — annotate in Gaps.md, do not implement

- aider: Browser GUI (Streamlit) [GUI].
- claude-code: Mobile + Remote Control + claude.ai [mobile/remote]; cross-surface continuity (cross-device part) [remote].
- cline: VS Code/JetBrains [IDE]; Kanban web board UI [GUI]; messaging connectors (Slack/Discord/…) [remote chat]; ACP [IDE]; SSO [remote IdP]; in-editor diff UX [IDE].
- gemini-cli: VS Code companion [IDE]; ACP [IDE]; A2A network server [remote]; hosted extension gallery web UI [GUI]; editor surface breadth [IDE].
- jcode: iOS app + WS gateway over Tailscale [mobile/remote]; GPU desktop superapp [GUI].
- kilocode: IDE extensions [IDE]; FIM ghost-text autocomplete [IDE]; Cloud Agents/App Builder/Gastown UI [GUI/remote]; mobile apps [phone]; chat bots Slack/Telegram/Discord [remote]; Agent-Manager cockpit UI [IDE]; cloud-managed indexing [remote]; SSO [remote]; hosted gateway/billing [remote]; Next.js deploy [GUI/deploy].
- openclaude: VS Code extension [IDE]; Android Termux [phone].
- opencode: web UI + Electron [GUI]; ACP [IDE]; editor extensions [IDE]; hosted share/cloud sync [remote]; OpenCode Zen gateway [remote]; Slack bot [remote]; SSO [remote]; remote attach + mobile/web [remote]; multi-surface server (surfaces themselves) [GUI/IDE/remote].

## META (not implementable as a feature; note only)

- aider/cline/gemini/kilo: maturity, adoption, release cadence, distribution channels, docs depth.
