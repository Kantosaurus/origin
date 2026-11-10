# Changelog

All notable changes to `origin` will be documented in this file.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) loosely;
versions correspond to phase milestones from the implementation plan.

## [Unreleased]

The harness-improvements work: one entry per item id. Measurements quoted below are the ones taken while building
(bytes of prompts and tool definitions); no benchmark baseline has been run, so no accuracy or cost effect is claimed.
The older `## Unreleased` and `## 1.0.0 — 2026-06-17` headings further down are historical and unverified: which
release their entries belong to cannot be told from the repository (the current version is pre-1.0 `0.9.14`).

### Breaking changes

Defaults that changed, each with its escape hatch:

- **On Anthropic the `system` field is now an array of marked blocks** (B2), not a string. Anthropic-compatible
  endpoints that accept only a string need `ORIGIN_SYSTEM_CACHE=0`.
- **Old tool results are elided** once the context passes 70% of the window (A7): an elided result is gone from the
  context and has no recall handle, so the model re-runs the tool. Escape: `ORIGIN_ELIDE_HIGH_WATER=0`.
- **`Read` caps its output** at 100,000 bytes and 2,000 characters per line (B10); no switch, use `offset`/`limit`.
  The public `ToolMeta.token_budget` field and `DEFAULT_TOKEN_BUDGET` were removed.
- **`Edit` returns `{ok, diff}`** instead of a bare confirmation (B9).
- **A sub-agent request naming an MCP server that is not in the user's configuration is refused** (A5). Escape:
  define the server in a `~/.origin/subagents/*.md` `mcp:` list.
- **`input_tokens` now means uncached input on the OpenAI-compatible and Gemini providers too** (B1), so token
  budgets for goals stretch further and exported telemetry reads lower.
- **Secret values are redacted** from tool output and stored sessions, and file tools refuse to write a redaction
  marker (C9); no switch.
- **The code graph builds in the background** on a session's first `graph_query` (A11). Escape:
  `ORIGIN_CODEGRAPH_AUTOBUILD=0`.
- **Per-turn test and lint gates** (`auto_test`, `auto_lint`, the repro gate) now run even with `ORIGIN_EDIT_CHECK=0`
  and LSP diagnostics off (A12).
- **`Diagnostics` and `LspNavigate` now ask** before starting a language server, and are denied where nobody can
  answer (C2). Escape: "Always allow", `/permissions off`, or a `[[permission_rules]]` allow rule.
- **`AuthorWorkflow` asks every time** (no "Always allow") (A4). No switch beyond `/permissions off`.
- **An unreadable or unparseable `governance.toml` stops the daemon from starting** (A4); it used to run with no
  governance. Escape: fix the file, or remove it to run without governance.
- **`[[permission_rules]]` deny rules match tool names in any case and ignore `scope`** (C2): a deny with another
  scope applies everywhere, an allow with another scope is ignored (both logged), and a deny beats an allow for the
  same tool.
- **Child processes no longer inherit the daemon's environment** (A6): they get an allow-list without provider keys,
  so proxy variables (`HTTP_PROXY`/`HTTPS_PROXY`/`NO_PROXY`) and version-manager variables (for example
  `PYENV_ROOT`, `ASDF_DIR`; `NVM_DIR` is kept) no longer reach them. The ssh agent socket (`SSH_AUTH_SOCK`) is
  passed. Escape: list names to pass through in `ORIGIN_ENV_PASSTHROUGH`, or `ORIGIN_ENV_SCRUB=0`.
- **Permission mode** is `workspace`, not auto-allow (C2): Bash, network tools, writes outside the workspace and
  delegations that grant gated tools now ask; **headless, scheduled and ambient runs deny** what would ask. Escape:
  `/permissions off`, `origin run --dangerously-allow-all`, `ORIGIN_ALLOW_ALL=1`, or a `[[permission_rules]]` allow
  rule. Explicit deny rules now also bind auto-allowed tools and beat the flag.
- **Default reasoning effort** on Anthropic is `high` for the main loop and `medium` for sub-agents (was `max`) (B3).
  Escape: `/effort`, `--effort`, `ORIGIN_DEFAULT_EFFORT=max`.
- **Headless runs** (`origin run`, self-dispatch) no longer get the default-workflow directive (B5). Escape:
  `ORIGIN_DEFAULT_WORKFLOW=on`.
- **Swarm mandates** ("swarm is always on", a separate verifier `Task` for every non-trivial change) are opt-in (D3).
  Escape: `ORIGIN_SWARM_ALWAYS=1`.
- **Sub-agents stop after 50 turns** (was unlimited) (B8). Escape: `"budget": {"max_tool_calls": N}` in the `Task`
  input.
- **Dependency-install gate**: installing a package the project does not declare needs approval (C6). Escape:
  `ORIGIN_CMD_GUARD=0` (also turns off the command guard).
- **`WebFetch` requires a `prompt`** (whenever the quarantine call runs) and returns a cheap model's answer, not the
  page (C4). Escape: `ORIGIN_WEBFETCH_RAW=1`.
- **The stealth-browser fallback is off** (D11). Escape: `ORIGIN_STEALTH_FALLBACK=1`.
- **Sub-agent results** report `status` `completed`/`budget_exhausted`/`aborted`/`goal_unreachable` and cap report
  text at 8,000 characters (was a silent 4,000 cut) (B8). **A full `origin rewind`** now needs git 2.23+ (C8).
  **Windows turn-end gate commands** return the program's own exit code instead of 1 (C5). **Turn-end gate commands**
  the command guard rates `NeedsApproval` (e.g. a `$(…)` or variable as the program) are no longer run (C3; use a
  command the guard can analyse — for example, call a script file).
- **Prose edits (`ORIGIN_EDITFMT=1`) now obey the permission mode** (C2, C7): an edit block written in reply text is
  applied only where a `Write` would be allowed without asking, so nothing lands in mode `on`, outside the workspace
  or on protected paths, and a whole-file edit needs a path line directly above a closed fence. Escape: use the edit
  tools, or `/permissions off`.
- **The browser sidecars** (agent-browser, CloakBrowser) get the allow-listed child environment instead of the
  daemon's, so `AGENT_BROWSER_*` configuration variables no longer reach them (A6). Escape: list the names in
  `ORIGIN_ENV_PASSTHROUGH`, or `ORIGIN_ENV_SCRUB=0`.
- **Best-of-N refuses to apply a winner** whose files have uncommitted changes in the workspace (C5); the first
  attempt's result is kept and the reason is shown. No escape hatch: applying it would fail or mix two untested
  edits.
- Also changed by default: `WebFetch` refuses private and local addresses
  (A9; escape `ORIGIN_WEBFETCH_ALLOW_PRIVATE=1`); the per-model edit-format table picks the advertised edit tools
  (C7; escape `ORIGIN_EDITFMT=0`); commands the guard cannot classify need approval (C3; escape
  `ORIGIN_CMD_GUARD=0`).

### Security

- **A4**: Swarm sub-agents inherit the daemon's `governance.toml` overlays (policy layers, conseca tool/path/domain
  lists, `[[permission_rules]]` deny rules (never allow rules), `[browser] max_actions_per_session`); previously a
  worker ran with none, so a parent denied a tool could delegate it. A worker's inline-MCP tools go through the same
  checks as built-in tools (governance policy, read-only mode, where an MCP tool always counts as mutating, the
  PreTool hook, the standard denial result, secret redaction of output), and their error bodies are bounded at 16 KiB
  like their results. `Task` is now `RequiresPermission`; its preview starts with the count of the gated tools and
  MCP servers the worker would get, naming the gated tools from the registry:
  `2/14 gated tools (Bash, Edit), 1 mcp servers | …` (up to eight names, then `+K`; `(all)` when every gated tool is
  granted). Under a skill mask the requested tools are narrowed to the mask and a `Task` naming MCP servers is
  refused; a nested `Task` or `RunWorkflow` inside a worker is an error result. Best-of-N candidates run under the
  session's `[[permission_rules]]` (allow and deny), policy layers and conseca. Escape hatch: the overlays themselves
  (remove the rule from `governance.toml`).
- **A5**: `Task` inline-MCP servers are admitted only by name against servers defined in user-scope config
  (`~/.origin/subagents/*.md` `mcp:`), and the worker runs the user's definition; a model-supplied `command`/`url` is
  never run, and an unknown entry fails the `Task` with an error showing the refused command line. Escape hatch:
  define the server in a `~/.origin/subagents/*.md` `mcp:` list.
- **A6**: child processes get an allow-listed environment instead of the daemon's: Bash, turn-end gate commands
  (`auto_test`/`auto_lint`/repro gate, best-of-N's test oracle and its `git apply`), hooks, MCP stdio servers,
  language servers and the `ORIGIN_LSP_AUTO` installer, post-edit lint/format and syntax checks, notification
  commands and webhooks, shadow-git checkpoints, self-dev git (desktop toasts excepted) and the `Browser` tool's
  sidecars (`agent-browser`, and the CloakBrowser sidecar, opt-in since D11), whose `AGENT_BROWSER_*` configuration
  variables are therefore dropped unless listed in `ORIGIN_ENV_PASSTHROUGH`. Provider keys and other
  `*_API_KEY`/`*_TOKEN`/`*_SECRET`/`*_PASSWORD` names are dropped. The allow-list includes Windows build-discovery
  variables (`ProgramFiles`, `ProgramFiles(x86)`, `windir`, `NUMBER_OF_PROCESSORS`, `PROCESSOR_ARCHITECTURE`) and
  project-resolution variables (`PYTHONPATH`, `PYTHONHOME`, `GOFLAGS`, `GOMODCACHE`, `GOPRIVATE`, `GONOSUMDB`,
  `GONOPROXY`, `GOWORK`, `RUSTUP_TOOLCHAIN`, `RUSTFLAGS`, `CARGO_TARGET_DIR`, `XDG_CONFIG_HOME`, `XDG_CACHE_HOME`,
  `XDG_DATA_HOME`, `LD_LIBRARY_PATH`, `DYLD_LIBRARY_PATH`, `DOTNET_ROOT`, `NODE_PATH`), Unix tool variables
  (`SSH_AUTH_SOCK`, `PKG_CONFIG_PATH`, `TZ`, `SDKROOT`, `DEVELOPER_DIR`), and, for the browser
  sidecars, `PLAYWRIGHT_BROWSERS_PATH`, `DISPLAY`, `WAYLAND_DISPLAY`, `XAUTHORITY`, `XDG_RUNTIME_DIR` — being on the
  allow-list, these five reach every child process, Bash included. Turn-end gate commands run through the supervised
  spawn (command guard, PowerShell fallback when `pwsh` is absent). Add names with `ORIGIN_ENV_PASSTHROUGH`;
  `ORIGIN_ENV_SCRUB=0` restores full inheritance.
- **A9**: conseca `allow_paths`/`deny_paths` now gate `Grep` and `Glob` (`path`, or the cwd when omitted) and compare
  lexically canonicalized path components, so `..`, `.` and `//` segments, relative paths and Windows spellings
  (device and UNC prefixes, `\\?\`, trailing dots, alternate data streams) can no longer slip past a rule; anything
  unresolvable fails closed. Symlinks and 8.3 short names are not resolved. `WebFetch` refuses loopback,
  link-local (incl. 169.254.169.254), private, unique-local and unspecified addresses, checked after DNS resolution
  and on every redirect hop; system proxies are bypassed while the guard is on. Escape hatch:
  `ORIGIN_WEBFETCH_ALLOW_PRIVATE=1`.
- **B12**: zero-width, bidi-control and Unicode tag characters are stripped from fetched page text, MCP tool
  descriptions and recalled memory before they reach the model. Look-alike characters are not neutralised.
- **C1** (partly delivered): new `origin_sandbox::sandbox_status()` reports whether child processes are OS-confined;
  today it is `unavailable` on every platform (no backend is enforced on the shipped path), or `disabled_by_user`
  under `ORIGIN_SANDBOX=off`. **Not done:** enabling the Linux/macOS backends by default, the workspace carve-outs,
  fail-closed spawning, and sandbox-failure reporting. They need design decisions and a Linux and a macOS run; see
  the design doc's "What an OS sandbox would take".
- **C2**: new default permission mode `workspace`. Edits inside the workspace run without asking, except `.git` and
  `.origin` paths, Origin's configuration directory and `ORIGIN_HOME`, and any edit when the workspace is, or
  contains, your home directory, or is a drive root. Bash (no OS sandbox is active on any shipped build), network
  tools (WebFetch, WebSearch, Browser, gmail, MCP), writes outside the workspace, commands the command guard cannot
  classify, and `Task` delegations that grant permission-gated tools or MCP servers ask first; dangerous commands are
  refused without an ask. "Always allow <tool>" now lasts for the session (in memory, until the daemon restarts) and
  never covers `Task`, `RunWorkflow`, `AuthorWorkflow`, file-tool writes outside the workspace or to protected paths,
  or guard-flagged commands (a Bash command can still write anywhere the user can: there is no OS sandbox, and the
  guard does not inspect redirect targets).
  `/permissions workspace|on|off` switches modes (`off` restores the old auto-allow). Headless runs (`origin run`,
  scheduled prompts, ambient work) cannot ask, so such calls are denied and returned to the model as errors unless
  `origin run --dangerously-allow-all` or `ORIGIN_ALLOW_ALL=1` is given (the CLI strips `ORIGIN_ALLOW_ALL` from a
  daemon it spawns; a daemon's own variable applies to non-interactive requests only). Explicit `[[permission_rules]]`
  deny rules now also bind auto-allowed tools and win over the flag. Once a network or MCP result (or a sub-agent
  result that contained one) has entered a session, network tools and unsandboxed Bash need a fresh approval each
  time; a `RunWorkflow` that fails after loading its workflow taints the session when any step could reach a network
  or MCP tool (not one whose steps form a cycle). **Security fix:** prose edits (`ORIGIN_EDITFMT=1`, edit blocks applied from the model's reply text)
  previously bypassed every permission check; each is now resolved against the run's working directory and applied
  only where a `Write` to that path would be allowed without asking (nothing in mode `on`, nothing outside the
  workspace or on protected paths; deny rules, the governance policy, read-only mode, the conseca path policy and
  the `pre_tool` hook apply), and edits that were not applied are named in one `[editfmt] … not applied` line at
  the end of the reply. Sub-agents run only what their `Task` grant names and never a command that needs approval
  or a write outside the workspace. An unattended interactive `/goal` loop blocks on the first ask. Other run-on-next-use files (CI
  workflows, package scripts, Makefiles) are ordinary project code.
- **C3**: `origin-cmdparse` parses Bash command lines instead of pattern-matching them: quoting and escapes;
  `;` `&&` `||` `|` `&` and newlines; subshells, `$(…)`, backticks and `<(…)`; the `bash -c`, `pwsh -Command`,
  `cmd /c`, `eval` and `iex` wrappers; the `sudo`, `env`, `xargs`, `nohup`, `time`, `timeout` and `chrt` prefixes,
  analysing the first four non-option words after a prefix (variable, glob and group words skipped; a per-scan budget
  of 4,096 extra starts, then approval is needed). A command whose program cannot be determined statically, or that
  cannot be parsed, gets a new `NeedsApproval` verdict: it goes through the permission prompt and is denied when no
  one can be asked; such an ask never offers "Always allow". Turn-end gate commands rated `Dangerous` or
  `NeedsApproval` are not run; each run logs it, and the user is told once per session for each refused gate command with one `[origin]`
  line naming the gate, the command, the reason and the way out. Known limits:
  `sudo -u a -g b -h host -p prompt rm -rf ~` (past the four-word window) and bash single-dash long options (`bash -norc < evil.sh`) are rated `Safe`. `ORIGIN_CMD_GUARD=0` still disables the guard.
  Note for bisecting: `origin-daemon` does not build at the six commits `3b022e6..f2f975d`.
- **C4**: `WebFetch` takes a `prompt` (its description names `prompt` and `url`/`urls` as required; a missing
  `prompt` is an error only when the quarantine call will run, not under `ORIGIN_WEBFETCH_RAW=1` and not when the
  provider has no cheap model) and returns only the answer of a tool-less quarantine call to a cheap model of the
  active provider (on Anthropic, the sidecar model `ORIGIN_SIDECAR_MODEL`, default Haiku; other providers fall back
  to the wrapped page). A failed quarantine call is a tool error, never the raw page — if that
  model is not available on the account, every `WebFetch` fails until `ORIGIN_SIDECAR_MODEL` or
  `ORIGIN_WEBFETCH_RAW=1` is set. Results of `WebFetch`, `WebSearch`, `Browser`, `gmail` and every `mcp__*` tool are
  wrapped in `<external source="…" url="…">…</external>` with marker look-alikes escaped, and the system prompt states
  such text is data, not instructions. Not covered: Bash `curl` output, files read from disk, sub-agent reports,
  memory hits. Quarantine tokens are not counted in the usage counters or the spend cap. Escape hatch:
  `ORIGIN_WEBFETCH_RAW=1`.
- **C6**: `Bash` commands that install or run a package the project does not declare now need approval, naming the
  package. Covered: `npm|pnpm|yarn|bun install|add` (npm's aliases and typos, options before the verb, `npm.cmd`),
  `pip install` (`pip3`, `pip3.12`, `python -m pip`, `py -m pip`), `uv pip install`, `uv add`, `cargo add`, `go get`,
  found through `sudo`/`timeout`/`env` (any option shape), `bash -c` and chains. Always needing approval, even for a
  declared name or with no package named: another index or registry (`--index-url`, `--extra-index-url`,
  `--find-links`, `--registry`, glued `-i…`/`-f…`, bare `yarn --registry`, or `PIP_INDEX_URL`/`PIP_EXTRA_INDEX_URL`/
  `PIP_FIND_LINKS`/`npm_config_registry`/`UV_INDEX_URL`/`UV_EXTRA_INDEX_URL`/`UV_INDEX`/`YARN_NPM_REGISTRY_SERVER`/
  `YARN_REGISTRY`/`GOPROXY` set on the command, also for `npm ci`, and `npm cit`/`npm update`/`npm up` with
  `--registry`), packages supplied at run time (`xargs`, `pip install -r` of a URL/stdin/substitution); and for named
  packages: global installs (`-g`, `--location=global`, `uv pip install --system`, `yarn global add`,
  `cargo install`, `go install x@v`), URL/VCS/path/tarball sources (also in the version slot, `x@npm:y`), a working
  directory that does not resolve inside the workspace, and a missing, over-1 MB or unparsable manifest.
  Run-from-registry forms: `npx`/`npm exec` pass for a binary already in `node_modules/.bin` or a declared package (a
  `-p` value must be declared); `pnpm dlx`, `pnpx`, `yarn dlx`, `bunx`, `uvx`, `uv tool run`, `pipx run` only for a
  declared package. Still not covered: poetry, gem, apt, brew, corepack; `npm create`/`npm init <x>`, `yarn create`,
  `pnpm create`, `bun create`; `uv run --with`, `uv tool install`, `pipx install`; package managers run inside
  `uv run`/`poetry run`/`pipenv run`; `pip download`/`pip wheel`; registry state set by an earlier command or a
  config file; `--@scope:registry=`; a manifest edited first (declare, then install); `sudo pip install <declared>`
  (passes as declared); and two known extra asks: `pip install --upgrade pip` (pip is not in the manifest) and
  `go install example.com/x` without a version (treated as global). In permission mode `off` every ask is approved,
  so the gate does not bite there. Changed default: on with the command guard; `ORIGIN_CMD_GUARD=0` turns it off.
  Opt-in `ORIGIN_DIFF_REVIEW=1`: per-edit regex tripwires (key-shaped secrets, eval/exec of dynamic input, shell or
  SQL built by concatenation, disabled TLS verification, `chmod 777`, `innerHTML`/`dangerouslySetInnerHTML` with
  non-literal input) add a one-line note to the edit result (never blocks), and one fresh-context, tool-less model
  review of the run's diff at the end of the run (skipped beyond 2,000 changed lines; diff capped at 60,000 bytes and
  built from redacted content; files over 1 MB are not read; no review on the last allowed turn) whose findings get
  one extra round. Not done: `cargo audit` / secret-scanner integration (would need a new dependency or network
  call).
- **C9**: the daemon redacts its own provider credentials by value — env vars named like credentials (`*_API_KEY`,
  `*_TOKEN`, `*_SECRET`, `*_PASSWORD`, `PGPASSWORD`, `GITHUB_TOKEN`/`GH_TOKEN`, `ORIGIN_*_KEY`,
  `AWS_SECRET_ACCESS_KEY`) and keyvault credentials read at startup, values of 8+ chars — as `[REDACTED:<NAME>]` in
  tool output, recalled memory and the repo-map focus block added to a prompt, turn-end gate output (including what the goal verifier sees) and persisted sessions, and file-writing
  tools refuse to write such a marker back to disk. Best-effort, not complete: keys added or rotated after startup,
  other accounts, and multi-line/escaped or truncated secrets are not caught. The session DB is created 0600 on Unix
  (the content store and resume directory are not). Opt-in `ORIGIN_SESSION_RETENTION_DAYS` prunes idle sessions on
  daemon start (unset keeps every session).
- **D11**: the automatic stealth-browser (CloakBrowser) fallback on bot detection is opt-in
  (`ORIGIN_STEALTH_FALLBACK=1`). Without it the router never spawns the Cloak sidecar and returns the primary
  backend's bot-detected response as is. `playwright-core` in `vendor/cloak-browser/package.json` is pinned to
  `1.45.0` (there is no lockfile; this is the minimum of the old `^1.45.0` range).
- **B7**: the post-edit syntax check no longer runs code from the workspace: Python is run as
  `python -I -m py_compile <absolute path>`, and both checkers (`python`, `node --check`) run from the system temp
  directory on an absolute file path. A Python that rejects `-I` counts as no interpreter (the tree-sitter check
  covers the file). Previously a `py_compile.py` written to the workspace was imported and run at turn end without
  an ask. `ORIGIN_EDIT_CHECK=0` still turns the check off.
- **C2**: `Diagnostics` and `LspNavigate` are `RequiresPermission` (they start rust-analyzer, which runs
  `cargo metadata`, build scripts and proc-macros of the workspace). In mode `workspace` it is an ordinary ask that
  "Always allow" covers; headless runs and sub-agents without the tool in their grant are denied; mode `off` or a
  `[[permission_rules]]` allow rule runs them unasked. A `Task` granting either tool is no longer a read-only
  delegation.
- **C3**: the command guard also refuses `rm -rf` of every entry of home or root (`~/*`, `/*`, `~/.`, `~/.*`,
  `$HOME/*`) and of a Windows drive root (`C:`, `C:\`, `C:/`, `C:\*`, via `rm`, `Remove-Item`, `rd`, `rmdir`) and
  remote content piped into a grouped shell (`curl … | (sh)`, `| { sh; }`); asks for `env -S`/`--split-string` and
  for a shell or `source`/`.` run on a process substitution (`bash <(curl …)`); treats `nc`, `ncat`, `scp`,
  `Invoke-WebRequest`/`iwr` and `Invoke-RestMethod`/`irm` as network commands for the secret-exfiltration check; and
  treats `npm install-ci-test` as an install. Known false positive: `rm -rf C:` in a POSIX shell (a directory named
  `C:`) is refused.
- **C6**: the dependency gate also reads registry variables (`PIP_INDEX_URL`, `NPM_CONFIG_REGISTRY`, `GOPROXY`, …)
  from the `Bash` tool's `env` argument: an install run with one needs approval, as with `NAME=value` on the command.
- **A4**: `AuthorWorkflow` is asked every time and never offers "Always allow" (it writes `~/.origin/workflows.toml`,
  which later grants tools). The `RunWorkflow` prompt starts with what its steps can reach
  (`<G>/<T> gated tools (…), <D> mcp servers | … | workflow: "<name>"`), or names the workflow with
  `(workflow not found)`. Known limit: the workflows file is read again when the workflow runs, so one replaced
  after the ask runs with grants the user did not see.
- **A4**: a `governance.toml` that exists but cannot be read or parsed stops the daemon from starting; the reason
  (`failed to parse governance config at <path>: …; origin-daemon does not start without the governance this file
  configures (fix the file, or remove it to run without governance)`) is in `~/.origin/daemon.log`, which the CLI
  points to. Previously it ran with no governance at all. A missing file still means no governance. Known limit: the
  supervisor keeps restarting the daemon, logging the same error each time.
- **C2**: `[[permission_rules]]` tool names match in any case (`tool = "bash"` binds `Bash`). A deny rule with a
  `scope` other than `"*"` applies everywhere (logged: `scope '<s>' is not supported; this deny rule applies
  everywhere`); an allow rule with such a scope is ignored (logged: `…; this allow rule is ignored`). When a deny and
  an allow rule name the same tool, the deny wins.
- **C2**: a `Browser` screenshot to a path outside the workspace or on a protected path (`.git`, `.origin`, the
  governance directory, `ORIGIN_HOME`) asks the person every time, like a `Write` there; "Always allow Browser" no
  longer covers it and runs that cannot ask deny it.

### Added

- **B1**: per-request token telemetry. The agent loop emits one `request_usage` stream event per model request
  (uncached input, output, cache-read and cache-write tokens), visible in `origin run --output-format stream-json`,
  and one `prompt_prefix` event per session (system + tool-schema bytes, ≈tokens). `Usage.input_tokens` now means
  uncached input for every provider; the OpenAI-compatible and Gemini providers subtract cached tokens, so the
  context meter and cost no longer double-count cache reads (and goal token budgets on those providers stretch
  further on a warm cache). OpenAI-compatible (Chat Completions and Responses) and Bedrock report prompt-cache reads;
  Bedrock also reports cache writes. New `origin_cache_write_total{provider}` metric. Compaction's measured-token
  gate counts cached tokens. Runtime inline-MCP tool schemas are sent in name order.
- **B4** (partly delivered: no baseline run): `bench/regression/` regression benchmark. `run.py` runs N trials per
  task through `origin run` headless and writes pass@1, pass^k, per-request token/cache counters, dollars and wall
  time, with model and effort required and recorded. Two example fixture tasks; `--dry-run` and `--self-test` work
  offline. The runner pre-checks every task before the first trial, writes results after every trial
  (`"complete": false` on abort), restores `verify.restore` test files before the check (`tests_modified` marks
  tampering) and kills the whole process tree on timeout. `bench/swe/adapters/origin.sh` now counts tokens from
  `request_usage` events, so SWE-bench tokens and dollars no longer double-count Anthropic requests (earlier numbers
  from that adapter were roughly double); older builds fall back to raw `usage` with `token_source: usage`. The
  runner and adapter set `ORIGIN_ALLOW_ALL=1` for the `origin run` child: run them only in a disposable container or
  VM. **No baseline is committed**: none could be run where this was built (no API key, no Docker scorer).
- **B6**: the system prompt's `<origin-env>` block lists the toolchains and package managers found on `PATH` in one
  `Tools on PATH: …` line (PATH lookup only, no process spawned; a zero-length or non-executable first match, such
  as the Windows Store python alias, counts as not found, and on Unix a symlink to a directory is ignored so a later
  real binary is found). It is probed once per daemon process with a 2 s budget
  and stays fixed for every session, so the prompt cache holds. Disable with `ORIGIN_TOOL_PROBE=0`.
- **B7**: the default-on post-edit syntax gate also parses edited files with tree-sitter for every grammar
  `origin-codegraph` links (Rust, TypeScript, Go, Java, C/C++, C#, Ruby, Bash, PHP, Swift, Kotlin, Scala, Haskell,
  Elixir, Lua; Python when no real interpreter ran, including when only the Windows Store python alias is present).
  It reports only parse errors an edit added, as a separate `<syntax-advisory>` ("may be a parser limitation —
  ignore if the code builds"), not a must-fix. Files over 1 MB, `.tsx` and `.h` are skipped. `.jsx` files are no
  longer sent to `node --check` (node rejects JSX) and are skipped, since no JavaScript tree-sitter grammar is
  linked. The pinned grammars
  lag current languages (the pinned Rust grammar flags `use<>`, `&raw const` and `gen {}`). Disable the whole gate
  with `ORIGIN_EDIT_CHECK=0`.
- **B11**: opt-in pre-completion check. With `ORIGIN_PRECOMPLETE_CHECK=1`, a run that edited files
  (Edit/Write/MultiEdit/ApplyPatch) and ran nothing since the last edit (no Bash call, no test/lint gate that
  actually ran) gets one trailing message asking the model to verify the change or say why it cannot, at most once
  per run. Off by default. Also: the agent loop's end-of-run effects (usage/cache Prometheus counters, completion
  notification, PostPrompt/Notification hooks, Completed stop record, memory-proposal scan) now run once per run;
  previously a run continued by the repro gate fired them again at its real end, double-counting tokens, cache and
  tool calls in `/metrics`, `origin usage` and the TUI panel. They now fire after the terminal test gate.
- **C8**: with checkpoints on (`ORIGIN_CHECKPOINTS=1` / `ORIGIN_CHECKPOINTS_PER_TOOL=1`), a turn-end gate (auto_test
  / auto_lint / repro gate) that runs and passes tags the current tree as `last-green` in the run's shadow repo
  (`.origin/shadow.git`, ref `refs/tags/last-green`), provided every armed gate ran and none was red (a gate the
  command guard refused, or that could not run, blocks the tag). A run that later ends with a failing gate gets one note in its
  final output (not the conversation, not a sub-agent result) naming that checkpoint and the restore commands
  (`origin checkpoint-diff <id>`, `origin rewind <id> --files-only`, `origin rewind <id>`); nothing is restored
  automatically, and restoring overwrites current files. Fixed with it: shadow checkpoint commits ignore the user's
  commit-signing config and use a fixed identity (a global `commit.gpgsign=true` made every checkpoint fail
  silently); checkpoints no longer record the shadow repository itself (a legacy shadow that had recorded itself is
  repaired on the next snapshot), and `origin rewind` no longer corrupts it: a full rewind restores the tree with the
  shadow directory excluded and then moves HEAD, so `last-green` and later checkpoints keep working. Shadow commands
  no longer run the user's git hooks, and the first failed checkpoint in a run is logged at warn. A full
  `origin rewind` requires git 2.23 or newer (`git restore`).
- **C10**: opt-in post-edit LSP diagnostics (`ORIGIN_LSP_DIAGNOSTICS=1`) keep one language server warm across a
  prompt's turns instead of spawning one per edited file; every edited file is reported to it as changed, and the
  servers are restarted after a shell/unlisted mutation and at each new prompt (including each `/goal` iteration);
  timeouts are reported ("diagnostics timed out after 2.5s", "the language server is still starting", previous or
  untagged results marked "diagnostics may be stale"); hung servers are restarted, a server that fails to start 3
  times is not retried. In shell-heavy sessions a slow-starting server may never return diagnostics. Best-of-N
  candidates get none. Not run against a real language server.

### Changed

- **B2**: cache-stable prompt prefix. The system prompt is ordered static-first; the prefix is stable while the
  system prompt is unchanged, and a change to it (activating a skill, `/ponytail`, output style, a `/model` switch
  that changes the edit-format block) re-bills the message history once. The repo map is scanned once per session and
  workspace. Map entries for files a prompt names, and memory
  recall, move into that prompt's user message; they are hidden from session export, turn summaries and memory
  gardening. Anthropic requests send `system` as a block with a `cache_control` marker, so tools + system are cached
  from the first request, and keep three rolling message markers (four in total). Opt out of the marker with
  `ORIGIN_SYSTEM_CACHE=0` (string-form `system`, four message markers), and of the repo map with `ORIGIN_REPOMAP=0`.
  Not checked against the live API: the OAuth path's billing-header block, and `ORIGIN_CACHE_TTL_1H=1` from the
  first request.
- **B3**: reasoning effort defaults to `high` for the main loop and `medium` for swarm sub-agents on Anthropic (was
  `max` for both). `/effort`, `/fast` and `--effort` still win, and an explicit parent effort is handed down to
  workers (`Task` workers and the step workers of an in-session `RunWorkflow`, which also inherit the session's
  ponytail level; the `/workflow` command still uses each worker's defaults) and best-of-N candidates; set `ORIGIN_DEFAULT_EFFORT=max` (or `ultracode`) to restore the old main-loop
  default. Other providers still send no effort by default. The quality effect of `high` versus `max` was not
  measured.
- **B5**: headless `origin run` (and other non-interactive requests) no longer get the default-workflow directive in
  the system prompt; set `ORIGIN_DEFAULT_WORKFLOW=on` to keep it. Swarm sub-agents get a small fixed prompt (role,
  task contract, tool rules, env, the user's ponytail ruleset when active, and the external-content rule) and no
  longer scan for the repo map. Measured system-prompt bytes when this landed: headless 11,257 → 6,797; worker
  14,136 → 6,334 (2,282 with ponytail off; the 14,136 baseline included the ponytail ruleset at its default level).
- **B8**: sub-agent (`Task`) results report the files the worker's successful edits changed (`files_touched`, first
  100), say whether shell commands ran, and return status `completed` / `budget_exhausted` (turn cap) / `aborted`
  (permission denials) / `goal_unreachable` (error) with the partial work kept; the report text is capped at 8,000
  characters with a notice of how many were dropped (was a silent 4,000 cut, so long reports now cost the parent
  more); workers stop after 50 turns by default (was unlimited; override per call with
  `"budget": {"max_tool_calls": N}`).
- **B9**: `Edit` returns `{ok, diff}`, a unified diff of the change with 3 context lines (bounded in size and time),
  instead of echoing `old_string`/`new_string` back as `hunks[].before/after`.
- **B10**: text `Read` results are capped at 100,000 bytes and 2,000 characters per line, and end with
  `[N more lines — call Read with offset=<next>]` whenever lines remain. The unused `ToolMeta.token_budget` field and
  `origin_tools::DEFAULT_TOKEN_BUDGET` were removed.
- **C7**: the per-model edit-format table now picks the advertised edit tools by default: models it maps to
  whole-file (`gpt-3.5*`, `*turbo-instruct`) are not offered `Edit`/`MultiEdit`/`ApplyPatch` (`Write` stays); every
  other model keeps today's list. `ORIGIN_EDITFMT=0` restores the full list. New opt-in `ORIGIN_TOOL_PROFILE=lean`
  advertises only the core tools (Read, edit tools, Write, Bash, Grep, Glob, Recall, TodoWrite; MCP tools
  unaffected): 5,510 versus 14,708 tool-definition bytes (10 versus 32 tools), about 62% fewer. Both are fixed per
  session at its first prompt. Static prompt text still names some tools `lean` hides. With `ORIGIN_EDITFMT=1`, a
  whole-file prose edit is applied only for a path line standing alone — relative, no `..`, containing a `/` or a
  file name with an extension that has a letter (`README.md`, `src/a`) — directly followed by a closed fenced block;
  `Example:`, `Done.` or `v1.2.3` above a snippet, a plain `done`, or prose before a fence are not edits (a plain
  reply could previously create or overwrite a file). Several path+fence pairs in one reply are several edits. For
  the permission checks prose edits now get, see C2.
- **D3**: the "swarm is always on" and "dispatch a separate adversarial-verifier `Task` for every non-trivial change"
  mandates are opt-in via `ORIGIN_SWARM_ALWAYS=1`; the rest of the default-workflow directive is unchanged (it still
  asks for parallel `Task` dispatch in two steps). Measured interactive system prompt: 11,257 → 10,499 bytes.

### Fixed

- **A1**: on the Anthropic provider, a tool result over 2,048 bytes from a Mutating tool (`Bash`, `Edit`, `Write`, `MultiEdit`, `ApplyPatch`, `Browser`, `Task`, …), or from any handle the shared `Plan` had not registered (e.g. a session resumed after a daemon restart), is now sent to the model verbatim instead of as a `<result handle:XXXXXXXX — N bytes>` stub that `Recall` could not resolve. Anthropic now uses the same `inflate_tool_result_handles` helper as every other provider; `origin_planner::WireDecision` and `INLINE_BYTE_BUDGET` are removed. No escape hatch: the stub was never resolvable, so there is no prior behaviour worth opting back into.
- **A2**: the per-run tool memo (`Read`, `Grep`, `Glob`, `Diagnostics`, …) is now cleared whenever a Mutating tool (`Bash`, `Edit`, `Write`, `MultiEdit`, `ApplyPatch`, `Task`, …) is dispatched, so a re-`Read` after an edit returns the new bytes instead of a `(cached from turn N)` copy of the old ones. Repeated reads with no mutation in between are still memoized. No escape hatch: serving pre-edit bytes was never correct.
- **A3**: mid-stream speculative dispatch only pre-runs tools that are both pure and auto-allowed; permission-gated
  pure tools (WebFetch, WebSearch, gmail) wait for the permission decision. No speculation at all while a deny-only
  layer (policy, conseca, hooks, …) is configured, or in a run with its own working directory.
- **A7**: long autonomous prompts no longer overflow the context window. Once the context passes 70% of the model
  window, old tool results are replaced in one batch by `[tool result elided: <tool> <N> bytes]`, keeping the largest
  window of recent assistant turns (10 down to 3) that brings the context under 50%. If no window gets there, the
  3-turn window fires only when it frees at least 10% of the window; otherwise nothing is rewritten, so the prompt
  cache is not broken every turn. Tool_use/tool_result pairing is kept. A provider context-length error elides with
  a 3-turn window and retries once (not at all if there is nothing to elide). A failed request now persists its
  session. Summary compaction is unchanged. Escape hatch: `ORIGIN_ELIDE_HIGH_WATER=0`.
- **A8**: a denied tool call no longer ends the run: the model gets an `is_error` tool result naming the tier and rule
  that denied it, and the loop continues. The run stops with "tool denied" after 3 consecutive (counted per assistant
  turn, after the whole batch is answered) or 20 total denials.
- **A11**: the first `graph_query` in a session starts a one-time background build of the code graph over the
  session's workspace roots (gitignore-aware; the walk stops at 2,000 indexable files; fail-open). Previously nothing
  built it unless the model called `graph_rebuild`, so `graph_query by_name` silently returned nothing. Runs with
  their own working directory (best-of-N candidates) never auto-build. Escape hatch: `ORIGIN_CODEGRAPH_AUTOBUILD=0`.
  Test selection (`ORIGIN_TEST_SELECT`) is unchanged and still cannot follow cross-file callers: graph edges are
  file-local only, so shipped test selection only selects tests next to the edited files.
- **A12**: best-of-N (`ORIGIN_BESTOFN`) candidates run in their own git worktree through a per-run working directory
  (`LoopOptions::cwd`) instead of changing the daemon's process-wide cwd, so concurrent sessions are no longer
  affected while candidates run. A candidate's Bash/Glob/Grep directories, checkpoints, per-edit formatter/linter and
  rust-analyzer root follow its worktree; MCP tool arguments are never rewritten. Per-turn `auto_test` / repro gate /
  `auto_lint` now run even when the syntax gate (`ORIGIN_EDIT_CHECK=0`) and LSP diagnostics are both off. Bug fix;
  no default changed.
- **C5**: repro gate (`ORIGIN_REPRO_GATE`): before the model's first source edit, its reproduction test is run,
  narrowed to the test file where the runner allows (pytest, `python -m pytest`, `cargo test --test <stem>`,
  `go test`) and the file names are shell-safe. The edit is rejected, with the reason, when the test already passes, fails to load, cannot run, or (in
  a full-suite run) fails without naming the reproduction or on a suite that was already red; a failure the runner
  cannot classify is accepted with a caution. Rejections count once per turn, up to `[post_edit] max_repair_iters`
  turns, separately from the terminal test gate's budget. A source edit made before any reproduction test, or a spent
  budget, lets the fix proceed, and the final answer then says no reproduction was confirmed. A confirmed failure's
  output is handed to the model as the diagnosis. A RED test gate (repro gate or `auto_test`) on the last allowed turn
  returns the answer with the failure in the gate signals instead of ending the run with a max-turns error. On
  Windows, turn-end gate commands report the native program's own exit code (PowerShell folded every failure to 1;
  checked on Windows PowerShell 5.1 only). Best-of-N: candidates are scored on the tests that existed before the run
  (pre-existing test files a candidate changed are restored, and test files it added are set aside, for scoring
  only); a candidate that changed pre-existing tests is flagged with those paths; a tests-only candidate never wins;
  an unreadable change list scores Unknown; identical candidates (same normalised diff) are scored once, and when the
  first two agree and pass the rest are not run. Best-of-N's `git apply` of the winner runs with the scrubbed child
  environment. Best-of-N's outcome is shown to the user in one `[origin]` line: the applied candidate and why (it
  passed with the smallest change), or why none was applied; candidates that changed pre-existing tests, with their
  paths; candidates excluded as tests-only. A winner whose files have uncommitted changes in the workspace is not
  applied (it was made against HEAD; applying it would fail or mix both edits), and the first attempt's result is
  kept; when the winner is applied next to other uncommitted changes, the test gate is re-run once on the real tree
  and the line names those files and the re-test result. Both stay opt-in (D1, D2).
- **C5**: best-of-N refuses a winner that touches files outside the session workspace (a workspace in a sub-directory
  of the repository), naming them; candidates are scored from the workspace's directory in their worktree.
- **A2**: `Monitor`, `ask_user`, `gmail` and `graph_query` results are never served from the memo, and the memo is
  dropped after blocking sub-agents finish, when finished background sub-agents are collected, and after a prose
  edit is applied.
- **B1**: the governance spend cap (`max_spend_usd`) counts cache-write tokens.
- **B8**: a finished background sub-agent's `<background-results>` report is kept in the session history instead of
  being shown in one request only (it was lost on later turns, resume, compaction and a failed request).
- **A3**: a streamed `Monitor` call is not pre-run speculatively (it answered "unknown pid").
- **A1**: `Recall` of an offloaded JSON array returns its rows instead of offloading them again.
- **A7**: any single tool result is capped at 200,000 bytes where it is captured, with a truncation note, so one
  oversized result cannot overflow the window and break the session.
- **B1**: runs that end in an error (turn cap, denials, stuck loop, provider or governance error) still add their
  tokens to the usage counters, and a stopped sub-agent's report carries its real token usage.

### Documentation

- **A10**: the docs now match the code. README: no OS sandbox is active on any shipped build (the per-platform
  sandbox claim is gone), the secret lint is not run in CI and does not pass yet (11 violations), skills are a
  one-line catalog with bodies injected on activation (not an embedding index), version `0.9.14`, and the
  retired mdBook links point at `docs/`; the same sandbox and skill-injection claims are corrected in the
  architecture overview, skills, sandbox and authoring docs. Agent-loop docs describe denials (A8), prompt caching
  and counters (B1/B2), lean prompts and effort (B3/B5/D3), the tool list (C7) and the post-edit checks
  (B6/B7/B11/C10). `docs/security/security-model.md`: permission modes and their decision
  table, sub-agents, the command guard and dependency gate with their limits, external content, by-value redaction,
  the sandbox inventory, checkpoints. `docs/reference/environment-variables.md`: true defaults for
  `ORIGIN_CMD_GUARD` (on), `ORIGIN_REPOMAP` (on) and `ORIGIN_DEFAULT_WORKFLOW` (interactive only), every variable
  this work added or changed, and the gate switches `ORIGIN_REPRO_GATE`, `ORIGIN_TEST_SELECT`, `ORIGIN_BESTOFN`.
  The design doc records the 2026-11 evidence review (D1–D10) and what an OS sandbox would take. `ROADMAP.md`
  version label matches `Cargo.toml`. `.gitignore` covers `__pycache__/`.

## 0.9.14

### Added (execution feedback — all opt-in, unchanged when unset)
- **Reproduction/regression gate** (`ORIGIN_REPRO_GATE=1`, or `=<command>`): injects the `<repro-gate>` contract,
  runs the test command at turn end, and refuses a tool-free "done" while tests are RED, also without a `/goal`. A
  test command is auto-derived from the repo's ecosystem markers. Adds the `[post_edit] repro_gate` config key.
- **Regression-test selection** (`ORIGIN_TEST_SELECT=1`): narrows the gate command using the code graph's reverse
  dependencies (pytest/go). Graph edges are file-local, so callers in other files are not selected (see A11 above).
- **Best-of-N** (`ORIGIN_BESTOFN=N`, N≥2, capped at 6): for a hard instance with an armed test oracle, runs N
  candidates in isolated git worktrees and applies the verified winner's diff.
- **Sub-agents** arm `auto_test` (turn-end regression run and terminal enforcement), not the repro gate.

## 0.9.13

### Added (harness accuracy — fewer plan/execute errors)
Two adversarially-verified accuracy passes (25 fixes), each TDD'd:
- **Execution**: non-zero-exit `Bash` now sets the structural `is_error` flag (a failing `cargo test`/`pytest` no longer reads as a clean result); a new **stuck-loop detector** (`stuck.rs`) nudges then halts a degenerate loop (repeated/ping-ponging actions) that nothing previously bounded; schema-aligned **argument coercion** fixes common LLM shape mistakes (scalar→array, `"3"`→3, fenced JSON) before dispatch; unknown-tool "did you mean" + malformed-arg shape hints.
- **Editing**: the whitespace fallback now re-indents the replacement (no more `IndentationError`); `ApplyPatch` gets fuzzy hunk relocation + ws-tolerance; Read-gutter strip + no-op rejection; `MultiEdit` disjoint-snapshot apply; `WriteGuard` stat-snapshots so out-of-band (`Bash`/format) edits force a fresh Read.
- **Verification**: the goal verifier now sees the final syntax/test gate output **and** the actual last tool-result evidence (no more rubber-stamping prose); opt-in repro-gate + regression baseline (green→red classification); `auto_lint` feedback wired into a `<lint-results>` block.
- **Grounding**: the repo map is **default-on** (`ORIGIN_REPOMAP=0` to opt out) and biased toward files named in the prompt; a `graph_query { kind: by_name }` resolver lets the model verify a symbol exists before referencing it.
- **Routing / context**: compaction now also fires on the **measured** prefix-token count (token-dense transcripts no longer overflow into a 400) and is sized to the smaller of the session/routed-model window; legacy `gpt-4`/`gpt-4-32k` windows corrected; persistent `Api`/`Auth` provider failures fold into router health; effort re-resolved per routed turn; a failing-validation streak re-enters the stronger Plan model.
- **Providers**: a single malformed SSE frame no longer kills a non-Anthropic turn (skip-and-continue); empty responses retry.

## Unreleased

### Added
- **More package managers**: the release pipeline now publishes to **Homebrew** (tap `kantosaurus/origin`), **Scoop** (bucket `Kantosaurus/scoop-origin`), **Chocolatey**, **winget**, **AUR** (`origin-bin`), and a **Nix flake** (`nix profile install github:Kantosaurus/origin`), plus **cargo-binstall** metadata (`cargo binstall --git … origin-cli`). Each is a separate, non-blocking `release.yml` job that gates on its secret and skips cleanly until provisioned; `xtask release` stamps the new Scoop/Nix/Chocolatey manifests from the same per-target SHA-256 set as the existing Homebrew/winget/AUR templates. Setup (repos + secrets) is documented in [`docs/operations/package-managers.md`](docs/operations/package-managers.md).
- **Swarm focus view (TUI)**: while a swarm wave runs, **Tab** cycles a `❯` highlight through the sub-agents in the side panel and **Enter** drills the main pane into the selected agent's live full transcript (`⎇ viewing …` — assistant prose + tools streamed per agent); **Enter** again (or Tab + Enter) toggles back to the main origin conversation. The daemon streams each watched worker's output (`StreamEvent::SwarmAgentOutput`); the worker's assistant text is relayed via a per-worker ring subscriber (`stream_relay::relay_to_progress`).
- **Non-blocking background swarm**: `Task` now dispatches in the **background by default** (`background: true`) — the turn returns immediately so the main agent stays responsive while sub-agents run. Finished results are delivered automatically at the start of the next turn (a `<background-results>` prose block) or on demand via the new **`CollectTasks`** tool. Backed by a daemon-wide session-keyed `BackgroundJobs` registry + non-blocking `Coordinator::try_completion`/`evict`. Per-session cap `ORIGIN_SWARM_BG_MAX` (default 32, overflow falls back to blocking); global off-switch `ORIGIN_SWARM_BG=0`; per-call `background:false`.
- **`CollectTasks` builtin** (hot, `Tier::AutoAllowed`): gather finished background sub-agents in-turn + list those still running.
- **Unlimited swarm sub-agents by default**: the memory-`AdmissionGate` and runtime `Swarm` lane no longer impose a small fixed cap (Windows was capped at 3); the memory governor remains the real limiter on Linux. Re-cap with `ORIGIN_SWARM_MAX` / `ORIGIN_SWARM_LANE_MAX`.
- **Superpowers skills bundled** (`origin-skills`): 19 official superpowers skills embedded in the binary via `include_dir!` — workflow (brainstorming, writing/executing plans, goal), engineering discipline (TDD, systematic debugging, verification-before-completion), collaboration (requesting/receiving code review, subagent- & parallel-agent dispatch, worktrees, finishing a branch), and review/release (reviewing-security, investigating-performance-regressions, managing-dependencies, writing-commits-and-prs, writing-skills, using-superpowers). Override per skill at `~/.origin/skills/<name>/SKILL.md`. New `load_all(user_root)` merger lets user copies replace embedded ones by name.
- **`origin-browser` crate (new)**: dual-backend browser tool. `BrowserRouter` runs `agent-browser` as primary; on bot-detection (Cloudflare, reCAPTCHA, hCaptcha, PerimeterX, DataDome, Incapsula, Kasada, 4xx) transparently replays the verb against the vendored `CloakBrowser` sidecar and sticks to Cloak after two consecutive successes per session. Includes pure-Rust `WebFetch` (reqwest + readability + html2md) and `WebSearch` via Tavily (`TAVILY_API_KEY`).
- **Vendored CloakBrowser sidecar** (`vendor/cloak-browser/`): Node ≥18 stdio-JSON CLI (`cloak-cli.mjs`) wire-compatible with `agent-browser`'s snapshot/ref protocol so the router can swap backends mid-session.
- **`origin-tools` builtins**: `WebFetch`, `WebSearch`, `Browser` (all `Tier::RequiresPermission`).
- **Crate metadata**: every workspace crate now carries a `description` for crates.io; internal crates (`origin-bench`) marked `publish = false`; workspace `homepage` set.
- **Supply-chain CI** (`.github/workflows/audit.yml`): `cargo deny` gates RUSTSEC advisories, dependency bans (rustls-only TLS), and a crates.io-only source allow-list, on push/PR and a daily schedule. New root `deny.toml`.
- **Community health**: issue templates (`bug_report`, `feature_request`, `config`), `PULL_REQUEST_TEMPLATE.md`, `CODEOWNERS`, `.github/dependabot.yml` (cargo + github-actions + npm), and a root `.editorconfig`.
- **Coverage**: `ci.yml` now runs `cargo llvm-cov` and uploads an `lcov` artifact; added a stable-toolchain test lane alongside MSRV.

### Changed
- **Startup banner** is now the big block-letter `ORIGIN` wordmark **centered** in the viewport (was a small top-left wordmark); shown before the first prompt, then cleared for the compact top-chrome header.
- **Status readout never vanishes under the swarm panel**: the `model · ↑in ↓out · $cost` metrics are now the priority readout (they reserve their slot first and the phase clips to their left), so a narrowed status zone + the `⎇ N/M agents` phase can no longer push them off the row.
- **Docs** restructured into a plain-Markdown `docs/` tree (`architecture/`, `crates/`, `guides/`, `operations/`, `reference/`, `subsystems/`, `security/`); the mdbook site (`docs/site`) and its GitHub Pages publish were retired (the `Docs` CI now just validates manpages).
- **Canonical repo owner** normalized to `Kantosaurus/origin` across `Cargo.toml`, packaging templates (Homebrew/winget/AUR), the docs site, and quickstart (was inconsistently `wooainsley/origin`).
- **Linux packaging** now targets glibc/gnu (matching the release build) instead of musl in the Homebrew/AUR templates and the `xtask release` stamper.
- **CI hardening**: all GitHub Actions pinned to commit SHAs; least-privilege `permissions:`, `concurrency`, and `timeout-minutes` on every workflow; `--locked` builds/tests; docs site now deploys from `dev`. Added the `tool_use_parser` fuzz target to the nightly matrix.
- **npm packages** now ship `LICENSE` (and `NOTICE` in the main package) per Apache-2.0 §4; the vendored CloakBrowser sidecar is pinned to an immutable commit and documents its third-party deps (`vendor/cloak-browser/THIRD_PARTY.md`).
- **Docs**: the SDK guide's "Minimal Rust client" now reflects the real `origin-ipc` frame API instead of an unimplemented facade.
- **SPDX headers** added to all first-party `.rs` files; `REUSE.toml` + `LICENSES/Apache-2.0.txt` for REUSE compliance.
- **Community/security**: `GOVERNANCE.md`, `ROADMAP.md`, `SUPPORT.md`, an OpenSSF Scorecard workflow, Codecov upload, and an SBOM step on release; committed fuzz seed corpora for all six targets; winget manifests split into version/installer/locale.

### Fixed
- **Background sub-agents stayed `▸ running` in the swarm panel forever after finishing**. With the non-blocking background swarm (`Task { background: true }` by default), a dispatched worker streams a `"spawned"` `SwarmWorker` event but the turn returns without awaiting it — so, unlike the foreground path, no `"completed"`/`"failed"` event was ever emitted, and its panel row (and the `⎇ N/M agents` readout) showed it running indefinitely. The daemon now emits a terminal `SwarmWorker` event for each background job at the moment it is reaped — next-turn `<background-results>` delivery and the `CollectTasks` tool — keyed by the same stable worker id, so the row flips to `✔ completed` (or `✘ failed`, with the reason in `detail`). `TaskOutput.status == "completed"` maps to completed; every other terminal status reads as failed.
- **First-run discovery prompt no longer freezes the TUI when the agent asks an interactive question**. The auto-fired first-run prompt was `await`ed inline **before** the keyboard event loop started, so if that turn raised an `ask_user` / `AskUserQuestion` picker (the default brainstorming workflow actively drives one — typically a **multi-select** question), the picker rendered but every keystroke (↑/↓ navigation, space to toggle, Enter to confirm) was dropped, and the turn deadlocked because no `ChoiceDecision` could ever be sent. The first-run prompt now dispatches through the same non-blocking `spawn_prompt_turn` path as a hand-typed message, so the input loop is live from the first frame and the picker is fully responsive. The `interrupt_tx` slot is lifted into `run` and shared, so Ctrl+C also reaches the first-run turn.
- **Can't get back to the main agent after drilling into a sub-agent (swarm focus view)**. With 2+ sub-agents, the focus-view footer invites **Tab/Enter ↩ back**, but Tab moved the panel's *highlight* cursor onto a different agent while `swarm_toggle_focus` keyed its exit off the *highlighted* agent — so after a Tab, Enter compared the viewed id against the now-different selected id and **switched to viewing that other agent instead of exiting**, leaving the user hopping between sub-agents with no way back to the main origin view. `swarm_toggle_focus` now always returns to main whenever a sub-agent is focused (it keys off `swarm_view`, not the selection), Tab while focused also returns to main (matching the footer), and the `main.rs` Enter/Tab gate fires on `swarm_view || swarm_selected` so a focused user can always exit even if the highlight was cleared.
- **Prompt-cache 400 (`A maximum of 4 blocks with cache_control may be provided. Found 5`)** that killed long agentic turns. The agent loop's index-based per-turn cache breakpoints (`dynamic_message_markers`) were computed against `session.messages` but consumed by the Anthropic wire encoder against the post-`snapshot`/`expand`/`strip_orphan_tool_results` list; once a stranded/orphaned tool turn was stripped, those indices drifted onto extra blocks and the request exceeded Anthropic's 4-marker ceiling. The block-carried `cache_marker` (which rides with its block through every transform) is now the single source of truth, and the wire encoder hard-caps the request at 4 markers (keeping the latest, most cache-valuable ones) instead of merely warning. Adds `tests/cache_marker_ceiling.rs`.
- **`ask_user` / `AskUserQuestion` interactive picker now works in a normal TUI session**. It was gated on `permission_ask` (the `/permissions on` opt-in, default off), so the agent's interactive questions silently degraded to a "interactive prompting is not available" prose fallback. Decoupled via a per-request `interactive` flag the TUI always sets; the daemon now wires the choice registry on `interactive || permission_ask`.
- **Self-update fully lands on Windows**: applying a staged update now force-cycles the running daemon + supervisor onto the freshly-swapped binaries (instead of relying on a flaky mtime heuristic that left the daemon frozen at the old version). No more manual process-killing after an update.
- **Focused sub-agent transcript streams the agent's assistant prose**, not just its tool calls: the worker's token stream is relayed via a per-worker ring subscriber (`stream_relay::relay_to_progress`); previously assistant text never reached the focus view because workers set `event_tx` but not `relay_tx`.
- **Skill selector now scrolls through all skills**: pressing `/` previously truncated the suggestion list to the first 6 matches (alphabetically), so skills beyond that were unreachable. The full match list is now retained and the popup renders a scrolling window over it — arrowing up/down moves through every match, with `↑`/`↓` hints on the edge rows when there's more off-screen.

### Removed
- Internal `docs/superpowers/` planning and spec docs, and `docs/why-origin.md`, are no longer published in the repository; references to them were removed.

## 1.0.0 — 2026-06-17

### Added
- **Replay infrastructure** (`origin-replay`): `.origin-replay` bundle format (zstd-tar with manifest), `Recorder` trait with `Null`/`File` backends, `Frame` enum covering provider/IPC/CAS/clock/RNG events, virtual clock, seeded SplitMix64 RNG, opt-in `recorder` feature on `origin-provider`, `origin-ipc`, `origin-cas` via static `recorder_hook` registries.
- **Fuzz CI** (`.github/workflows/fuzz.yml`): nightly 5-target × 5-min cargo-fuzz matrix covering `ipc_frame` validator, `fastcdc_boundary` chunker, `anthropic_stream` + `openai_stream` SSE parsers, `streaming_json` rkyv decoder.
- **Migration** (`origin-migrate`, `origin import`): adapters for Claude Code (jsonl + SKILL.md), jcode (rusqlite reader), opencode (storage/*.json); idempotent content-hash dedupe via new `Store::{contains,insert}_migrated_{session,skill}` + V6 SQLite migration; `--dry-run` / `--apply` / `--json` modes.
- **Benchmarks** (`origin-bench`): 8-task fixed set, origin + generic subprocess runners, Markdown + JSON reports.
- **Docs site** (`docs/site/`): 11-chapter mdBook (intro/quickstart/architecture/configuration/providers/skills/hooks/mcp/migration/sdk/troubleshooting); `origin --tutorial` 7-step guided tour; clap_mangen manpages via `xtask manpages`.
- **Release engineering** (`.github/workflows/release.yml`): 6-target matrix build (glibc/gnu x86_64/aarch64, macOS x86_64/aarch64, Windows x86_64/aarch64) with cosign keyless signing + SLSA build-provenance attestation; packaging templates for Homebrew (`packaging/homebrew/origin.rb.tmpl`), winget, AUR, cargo-binstall metadata; `xtask release` stamps `{{VERSION}}` + `{{SHA256_*}}` placeholders.

### Gates
- Perf gate workflow asserts read-only task `wall_ms` worst ≤ 80 ms.
- Unsafe-audit workflow asserts `unsafe` only in `origin-cas`, `origin-tui`, `origin-ipc`.
- Security review signoff doc (`docs/security/p14-security-review.md`) for sandbox + KeyVault.
- Three migration paths validated by `crates/origin-migrate/tests/three_paths.rs`.

### Spec criteria
1. Deterministic replay + fuzz suite green: ✅ (`origin-replay`, `.github/workflows/fuzz.yml`).
2. Perf gates: ✅ (`.github/workflows/perf-gate.yml`).
3. Zero-unsafe in surface crates: ✅ (`.github/workflows/unsafe-audit.yml`).
4. Sandbox + KeyVault review: ⏳ (signoff doc landed; reviewer signature pending).
5. Three migration paths validated: ✅ (`tests/three_paths.rs`).

### Known follow-ups (out of GA scope)
- A.9–A.11 fuzz targets compile only under nightly Rust due to `anndists`/`ort-sys` transitive deps requiring `edition2024`; the GA build uses Rust 1.83 (MSRV) and the fuzz crate is excluded from the workspace. CI workflow runs against nightly.
- Apply-mode of `origin import` (vs dry-run) currently returns the same content-hash summary; persistent write through `apply_with_store` requires a CLI Store handle to be threaded — slated for a 1.0.x patch.

## Phase 13 — QUIC Remote IPC + Headless Polish (2026-05-20)

- New `origin-ipc::quic` transport: `QuicListener` / `QuicConnector` /
  `QuicConnection` over `quinn` + `rustls`. Identical wire framing to the
  local-socket transport so daemon dispatch is transport-agnostic. The
  `QuicConnection::Drop` impl detaches the underlying `quinn::Connection`
  onto a tokio task that awaits `Connection::closed()` so in-flight
  stream writes flush before teardown.
- New `origin-ipc::tls`: self-signed Ed25519 cert generation + SHA-256
  fingerprint helper. Peers pin by fingerprint; no PKI.
- New `origin-daemon::pairing`: 6-digit single-use pairing codes with
  TTL, bearer-token minting (`orb_` prefix, 24-byte random suffix),
  in-memory `BearerStore`, KeyVault persistence under
  `("origin-remote", <device>)`.
- Daemon IPC additions: `PairStart`, `PairRedeem`, `ListSessions`,
  `ResumeSession`, `RemoveSession`, `GetUsage`, `KeyringAdd`,
  `KeyringList`, `KeyringRemove` plus matching `StreamEvent`s
  (`PairCode`, `PairIssued`, `PairError`, `SessionsListed`,
  `UsageReport`, `KeyringAccounts`, `AdminOk`, `AdminError`).
- Daemon session_store: `list_summaries()` (id, created_at, title,
  model, message_count) and `delete(session_id)`. `Session.id` is now a
  `String` (was `MessageId`) to accept admin-supplied ids.
- New CLI subcommands:
  - `origin pair {start,redeem}` — pair a remote client; redeem opens a
    QUIC connection and prints the issued bearer.
  - `origin run [--json] [--remote <url>] [--bearer <t>] [--model <m>]
    <text>` — headless one-shot prompt. `--json` emits JSON-Lines per
    IPC event; default mode prints `text_delta` payloads concatenated.
    `--remote origin://host:port#fingerprint` routes through QUIC
    (CA loaded via `ORIGIN_REMOTE_CA_DER_FILE`).
  - `origin usage` — per-provider/per-model token totals from the
    metrics snapshot.
  - `origin sessions {ls,resume,rm}` — list/resume/remove persisted
    sessions.
  - `origin keyring {add,list,remove}` — manage KeyVault credentials;
    `add` reads the secret from stdin when the value is `-`.
- Workspace deps pinned: `quinn = "0.11"`, `rustls = "0.23"`,
  `rcgen = "0.13"`, `rustls-pemfile = "2"`, `x509-parser = "0.16"`,
  `sha2 = "0.10"`, `hex = "0.4"`. `origin-daemon` gains `rand = "0.8"`;
  `origin-cli` gains `hostname = "0.4"` + `url = "2"`.

### Test coverage at phase exit
- `origin-ipc`: 2 tls tests, 1 quic_smoke, 1 quic_concurrent.
- `origin-daemon`: 4 pairing_unit, 4 protocol_pair, 2 pairing_e2e,
  3 admin_ipc, 2 session_store_list, plus regressions exercised in
  the existing `account_switch` suite (extended for new variants).
- `origin-cli`: 1 run_help, 2 headless_stream, 3 pair_cli (incl. 2
  admin_url parsing tests), 3 admin_cli (--help surfaces),
  1 admin_e2e (fake daemon → sessions ls).

## Phase 3 — CachePlanner + Speculative Dispatch + Recall (2026-05-19)

- New `origin-planner` crate: `Band` enum (Frozen/Sticky/Sliding/Volatile),
  `PrefixLedger` stability scoring with promote/demote thresholds,
  `CachePlanner::plan` four-band sort + boundary marker indices,
  `WireDecision::for_block` inline-vs-reference rule.
- `origin-provider-anthropic` emits `cache_control: ephemeral` at planned
  band boundaries; consults `WireDecision` to inline small handles or
  emit `<result handle:XXXXXXXX — N bytes>` references for large ones.
- New `origin-daemon::tool_use_parser` — SAX-style incremental JSON parser
  yielding `Field` events before the streaming `tool_use` block closes.
- Speculative dispatch: agent forks pure-tool tasks on the parser's first
  complete field; side-effecting tools (`Bash`, `Edit`, `Write`, MCP
  writes) stay sequential. New `TokenKind::ToolUseStart` event surfaces
  the assistant's tool_use block-start to the agent loop.
- New `Recall` builtin: inflates a CAS handle with optional Lines /
  Match / OutlineOnly region selector. Threaded `Option<Arc<Store>>`
  through the dispatch path.
- Session-scope memoization: `(tool_name, raw_input_bytes)` → blake3
  hash → CAS handle; cached results annotated `(cached from turn N)`.
  `Bash`/`Edit`/`Write` opt out via `MEMOIZATION_SKIPLIST`.
- `phase3_cache_warm_ratio` checkpoint test: 20-turn synthetic workload,
  warm pass asserts `cache_read_input_tokens > 0.5 × input_tokens`.

### Known limitations (deferred)

- Section→wire-block index for `cache_control` marker placement is a
  stub (always Volatile); full plumbing lands with N4.3 encoder codegen
  in Phase 11.
- Multi-tool-use concurrent block delta routing uses "most-recent
  parser" instead of Anthropic's `index` field; sequential tool_use
  works correctly. Full index routing lands with concurrent-tool
  support in Phase 7.
- `OutlineOnly` region selector returns a placeholder; the sidecar
  coroutine that emits structure summaries lands in Phase 6.
- Tool-input normalization is byte-equivalent only; canonical-form
  normalization (path canon, regex parse-equivalence) lands with
  N10.4 in Phase 10.
- `cargo fuzz` target for `tool_use_parser` is stubbed; full corpus +
  CI fuzz cycle lands with N10.10 in Phase 14.

### Test coverage at phase exit

- All previous tests still passing.
- New: 8 `origin-planner` tests (3 ledger + 2 planner + 3 decision).
- New: 6 `origin-daemon` tests (3 + 1 prop tool_use_parser, 1
  speculative_e2e, 1 memoization_e2e, 1 phase3_cache_warm_ratio).
- New: 3 `origin-tools` recall tests + 3 memoization tests.
- New: 1 Anthropic `cache_markers` test + 1 `handle_substitution` test.

## Phase 2 — Streaming + CAS + Ring Buffer (2026-05-19)

- `origin-cas` crate: blake3 Hash, FastCDC chunker, mmap pack files,
  three-tier (Hot LRU + Warm mmap + Cold zstd) Store, refcount/GC enumerator.
- `origin-stream` crate: single-producer multi-tail byte ring;
  rkyv-archived TokenEvent records.
- Anthropic provider gains native SSE → ring streaming.
- `Provider::chat_stream` added to trait; default impl wraps `chat`.
- Daemon: tool outputs land in CAS; `Block::ToolResult` carries 32-byte
  handle, not inline bytes. `expand_messages_for_wire` re-inflates on the
  outbound encode hop.
- CLI: live token streaming infrastructure into TUI; assistant turn buffer;
  token/cost status bar driven by Usage events.
- RAM soak: 1000-msg session, ΔRSS < 200 MiB.
- New SQLite migration V2: `cas_refs(hash, refcount, tier, last_access)`.

## [phase-1] — 2026-05-19

### Added

- **`origin-provider`** — canonical `Provider` trait + `ChatRequest`/`ChatResponse`/`Usage`/`ToolSchema`/`ProviderError`.
- **`origin-provider-anthropic`** — non-streaming `POST /v1/messages` implementation with wiremock-driven tests; auth-error → `ProviderError::Auth`; rate-limit → `ProviderError::RateLimit`.
- **`origin-tools`** — inventory-backed compile-time tool registry, `origin_tool!` macro, and five builtin tools:
  - `Read` (AutoAllowed)
  - `Glob` (AutoAllowed)
  - `Grep` (AutoAllowed; recursive)
  - `Edit` (RequiresPermission/Medium; errors on missing or ambiguous old_string)
  - `Bash` (RequiresPermission/High; PowerShell 7+ on Windows with powershell.exe fallback)
- **`origin-permission`** — tier-based check with pluggable `Prompter` trait; `AlwaysAllow`/`AlwaysDeny` test prompters.
- **`origin-daemon`** — `Session`, `run_loop`, `SessionStore`. Agent loop:
  1. Append user message.
  2. Call provider with current messages + tool schemas.
  3. Dispatch each `tool_use` block through permission + registry.
  4. Append `tool_result` blocks as a `Role::Tool` message.
  5. Repeat until no `tool_use`; cap at 25 turns.
  Sessions persisted to SQLite (rkyv-archived `Message` blobs in `body_inline`).
- **`origin-cli`** — Ratatui baseline TUI with scrollback + prompt input; Ctrl-C / Esc to quit.

### Dogfood result

- SKIPPED — `ANTHROPIC_API_KEY` not set in environment; `anthropic_smoke.rs` test exited successfully with "skipping live_smoke" message. Agent-loop integration tests (P1.10) already verify the loop wires together correctly with a scripted provider.

### Test coverage at phase exit

- 48 tests passing across the workspace.

### Known limitations (deferred)

- Non-streaming only (streaming arrives in P2 with the byte ring buffer).
- AlwaysAllow prompter — interactive prompts move to the TUI side panel in P4.
- Sessions stored inline in `body_inline` blobs (CAS arrives in P2).
- API key read from `ANTHROPIC_API_KEY` env var; KeyVault with OS keychain arrives in P8.

### MSRV-pinned transitive dependencies (Cargo.lock)

Across P1 several transitive deps needed precise version pins to stay compatible with Rust 1.83:

- `url` 2.5.0 (avoid `idna_adapter` 1.2.2 which uses edition2024)
- `wiremock` 0.6.2 (0.6.5 uses edition2024)
- `hyper-rustls` 0.27.3 (0.27.9 requires Rust 1.85)
- `grep-matcher` 0.1.7 / `grep-regex` 0.1.12 / `grep-searcher` 0.1.14 (later versions require edition2024)
- `darling` 0.20.11, `instability` 0.3.5, `unicode-segmentation` 1.12.0 (Ratatui transitive deps)

Aggregate pin count now warrants re-evaluating the MSRV bump policy.

## [phase-0] — 2026-05-19

### Added

- **Workspace scaffold** — Cargo workspace at `crates/*`, Rust 1.83.0 toolchain pin,
  workspace lint policy (`unsafe_code = forbid`, `unwrap_used = deny`,
  `clippy::pedantic + nursery` warns).
- **Cross-platform CI** — `.github/workflows/ci.yml` running fmt + clippy + test on
  Ubuntu, macOS, and Windows.
- **`origin-core`** — Pure type crate.
  - `Role` (User / Assistant / Tool / System) with rkyv archive + check_bytes.
  - `MessageId` (ULID wrapper) and `TurnIndex` (overflow-safe `checked_add`).
  - `Block` enum (Text / ToolUse / ToolResult / Thinking) with optional cache markers.
  - `Message` (role + Vec<Block>).
  - `ProviderCaps` IR scaffold (const-constructible).
- **`origin-ipc`** — IPC transport crate.
  - Wire frame with magic, 17-byte header, length-prefixed body, validation against
    truncation / bad magic / unknown kind / length mismatch.
  - Cross-platform local-socket transport (Unix domain sockets / Windows named pipes)
    via `interprocess` crate.
- **`origin-store`** — SQLite persistence with `refinery` migrations.
  - WAL mode, V1 schema for `sessions` + `messages`.
- **`origin-daemon`** + **`origin-cli`** (binary `origin`) — Echo handshake over the
  IPC transport, verified end-to-end on Windows.

### Architecture milestones reached

- Daemon process, IPC transport, and client binary separated by crate boundary —
  same code shape will support multi-client and future remote/desktop surfaces.
- All types are `rkyv::Archive` from day 1 so the same bytes can flow through IPC,
  storage, and in-memory ring buffers without serialize/deserialize hops.
- 18 tests passing across the workspace; property tests for frame encoding.
