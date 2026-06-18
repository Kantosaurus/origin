# origin-cli

> Terminal UI and CLI for the origin agent runtime.

## Purpose

`origin-cli` is the user-facing front door to the harness: the `origin` binary.
It hosts the interactive TUI (the prompt loop, composer, plan panel, streaming
widget) and a broad subcommand surface — one-shot prompts, session management,
provider/keyring setup, scheduling, plugins, review, and dozens more — all
dispatched through a `clap` command tree. The binary connects to the
`origin-daemon` over IPC for actual agent work and renders the stream through
`origin-tui`; the bulk of this crate is wiring, command handlers, and UX glue.

## Command & feature map

| Surface | Subcommands / flags | Module(s) |
| --- | --- | --- |
| Interactive TUI | (no subcommand) — prompt loop, composer, plan panel | `tui`, `input`, `editor`, `plan_panel_wiring`, `goal_render` |
| Global flags | `--tutorial` `--effort` `--thinking-tokens` `--root` `--resume` `--lang` | `cli_def`, `effort`, `locale`, `tutorial` |
| One-shot prompt | `run <text>` (`--json`/`--output-format`/`--json-schema`/`--remote`/`--model`/`--alias`/`--attach`/`--root`) | `headless`, `main.rs` |
| Sessions | `sessions ls\|resume\|rm\|rewind`, `export`, `import`, `resume-foreign` | `resume`, `resume_foreign`, `import` |
| Providers / auth | `init`, `providers ls\|describe\|refresh\|recommend`, `keyring add\|list\|remove\|login`, `oidc-exchange` | `init`, `providers`, `keyring_login`, `oidc`, `recommend` |
| Cost / usage | `usage`, `insights` | `insights`, `status` |
| Checkpoints (shadow-git) | `checkpoint`, `checkpoints`, `rewind`, `checkpoint-diff` | `vcs` |
| Memory / knowledge | `memory inbox list\|accept\|reject`, `knowledge add\|search\|rm\|ls` | `memory_inbox`, `knowledge` |
| Scheduling | `schedule add\|ls\|rm` | `schedule` |
| Plugins / skills / LSP | `plugin ls\|info\|install`, `lsp ls\|ensure` | `plugin`, `lsp` |
| Web / clipboard / voice | `search`, `copy-context`, `apply-clipboard`, `dictate`, `scout`, `watch` | `search`, `clipboard`, `voice`, `scout`, `watch` |
| Diagrams | `mermaid <path\|->` | `mermaid` |
| Quality | `bench`, `review`, `doctor` (`--json`/`--privacy`) | `bench`, `review`, `doctor` |
| Integrations | `gmail`, `workflow author\|run`, `selfdev start\|status\|approve\|reset`, `team create\|assign\|status` | `workflows`, `admin` |
| Remote | `pair start\|redeem`, `trace query` | `admin_url`, `trace_cmd` |
| First-run UX | `--tutorial`, onboarding, welcome, theme preview | `onboarding`, `welcome`, `first_run_prompt`, `theme`, `ansi` |

(The full clap tree lives in `cli_def.rs`; `main_cli()` re-exports the
`clap::Command` so `xtask manpages` can render man pages without depending on the
binary.)

## Key types

The command surface is one `Cli` struct + `Cmd` enum in `cli_def.rs`:

```rust
#[derive(Parser)]
#[command(name = "origin", version, about = "origin agentic coding harness")]
pub struct Cli {
    #[arg(long)] pub tutorial: bool,
    #[arg(long)] pub effort: Option<String>,
    #[arg(long = "thinking-tokens")] pub thinking_tokens: Option<u32>,
    #[arg(long = "root")] pub root: Vec<String>,
    #[arg(long = "resume")] pub resume: Option<String>,
    #[arg(long = "lang")] pub lang: Option<String>,
    #[command(subcommand)] pub cmd: Option<Cmd>,
}

#[derive(Subcommand)]
pub enum Cmd { Run { .. }, Sessions { .. }, Keyring { .. }, Providers { .. },
               Plugin { .. }, Mermaid { .. }, Bench { .. }, Review { .. },
               Workflow { .. }, Selfdev { .. }, Team { .. }, /* … */ }
```

The TUI side uses shared, lock-guarded handles plumbed through the event loop:

```rust
type SharedApp      = Arc<Mutex<App>>;          // origin_cli::tui::App
type SharedComposer = Arc<Mutex<Composer>>;     // origin_tui::composer::Composer
type SharedWidget   = Arc<Mutex<StreamWidget>>; // origin_tui::stream_widget::StreamWidget
```

## How it works

`main()` does **not** use `#[tokio::main]`. The whole TUI top-level future is one
large inlined state machine, and `block_on` materializes it on the stack before
polling — in a debug build that overflows Windows' 1 MiB main-thread stack even
for `--version`. So the runtime is driven on a dedicated thread with a generous
stack:

```
main()
 └─ thread "origin-rt" (stack_size = 16 MiB)
      └─ current_thread tokio runtime, enable_all()
           └─ rt.block_on(run())
                ├─ run_self_update()        swap staged binary, kick bg check
                ├─ clap parse → dispatch Cmd or enter TUI
                └─ TUI: connect daemon (IPC) → prompt loop
                         crossterm events → InputAction → call_daemon
                         StreamEvent ──► StreamWidget / Composer / plan panel
```

Self-update is non-blocking: step 1 renames any `<exe>.new` staged by a prior
background worker over the binary, step 2 spawns a detached worker to check the
registry and stage the next update — startup is never blocked on the network.
Two process-globals carry once-set or rarely-mutated session state without
threading new fields through `call_daemon`: a `OnceLock` `THINKING_TOKENS_SEED`
(set once from `--thinking-tokens`) and a `Mutex` `SESSION_ACCOUNT` (mutated by
the `/account` composer command and stamped onto every `PromptRequest`, since the
CLI opens a fresh daemon connection per prompt). Interactive input is reduced
through `input::reduce_editor` into `InputAction`s, with slash-command parsers
(`parse_model_command`, `parse_skill_command`, `parse_workflow_command`, …)
handling in-session `/model`, `/effort`, `/steer`, `/mem`, `/clear`, etc.; chrome
strings route through `origin-i18n` and output personas through
`origin-outputstyle`.

## Dependencies & features

A hub crate: it depends on most of the workspace — `origin-daemon`/`origin-ipc`
(IPC), `origin-tui` (renderer), `origin-runtime` (task spawning),
`origin-plan`/`origin-trace`/`origin-store`/`origin-migrate`, plus feature crates
`origin-skills`, `origin-tools`, `origin-goal`, `origin-cost`, `origin-router`,
`origin-doctor`, `origin-mermaid`, `origin-knowledge`, `origin-mem`,
`origin-cas`, `origin-schedule`, `origin-export`, `origin-i18n`, `origin-vcs`,
`origin-scout`, `origin-watch`, `origin-clipboard`, `origin-voice`,
`origin-websearch`, `origin-plugin`, `origin-modeldiscovery`,
`origin-outputstyle`, `origin-hooks`, `origin-steering`, `origin-lspfleet`,
`origin-ambient`, `origin-bench`, `origin-review`, `origin-notify`,
`origin-multimodal`, `origin-gmail`, `origin-workflowgen`. Third-party: `clap`,
`crossterm` (event-stream), `tokio`, `reqwest` (rustls), `serde`/`serde_json`,
`jsonschema`, `unicode-segmentation`/`unicode-width`, `which`, `sha2`. Ships both
a `[lib] origin_cli` and a `[[bin]] origin`; `package.metadata.binstall` defines
release-asset install. A `keystroke_to_pixel` Criterion bench measures
input-to-render latency.

## Used by

`Grep "origin-cli" glob "crates/*/Cargo.toml"`:

- `crates/origin-cli/Cargo.toml` (self)
- `crates/origin-clipboard/Cargo.toml`
- `crates/origin-ui-preview/Cargo.toml`

(`origin-ui-preview` and `origin-clipboard` reference `origin-cli` source/modules;
`origin-cli` itself is the top-level binary nothing else links against.)

## Testing

Inline `#[cfg(test)]` modules in `cli_def.rs` assert clap parsing invariants
(e.g. `rewind` accepts repeatable `--path` and rejects `--path` with
`--files-only`). Module-level tests across `input`, `editor`, `autocomplete`,
`mentions`, `markdown_tasks`, and the command parsers cover the editor reducer
and slash-command routing. Dev-dependencies (`origin-stream`, `tempfile`, `ulid`)
back integration-style tests, and the `keystroke_to_pixel` bench tracks the
input→render path.

## See also

- [tui-and-cli subsystem](../subsystems/tui-and-cli.md)
- [skills subsystem](../subsystems/skills.md)
- [crates index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
