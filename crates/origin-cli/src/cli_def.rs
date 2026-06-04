// SPDX-License-Identifier: Apache-2.0
//! Clap command definitions for the `origin` binary.
//!
//! These live in the library so that introspection tools (notably
//! `xtask manpages`, which renders `clap_mangen` output) can build the
//! same `clap::Command` tree without depending on the binary crate.

use crate::trace_cmd::TraceQuery;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "origin", version, about = "origin agentic coding harness")]
pub struct Cli {
    /// Run the 7-step interactive guided tour (P14.D.3).
    #[arg(long)]
    pub tutorial: bool,
    /// Reasoning-effort level for the session (`fast`/`low`/`medium`/`high`/`max`).
    /// Defaults to unset, leaving the provider wire unchanged. Parsed via
    /// [`crate::effort::ReasoningEffort::parse_level`] at the call site.
    #[arg(long)]
    pub effort: Option<String>,
    /// Extended-thinking budget in tokens for the session (Anthropic). Seeds the
    /// session so every prompt carries it. Defaults to unset, leaving the
    /// provider wire byte-identical. `0` is rejected. Only the Anthropic
    /// provider honours it; other providers ignore it. *Closes: aider
    /// `--thinking-tokens`.*
    #[arg(long = "thinking-tokens")]
    pub thinking_tokens: Option<u32>,
    /// Extra workspace root the agent may read/edit across (repeatable, cline
    /// multi-root). Applies to the interactive session; for `origin run` use the
    /// `Run`-level `--root`.
    #[arg(long = "root")]
    pub root: Vec<String>,
    /// Resume a previous session by id: reuse it so the daemon rehydrates that
    /// session's transcript (the model picks up where you left off). Find ids
    /// with `origin sessions list`. Defaults to a fresh random session.
    #[arg(long = "resume")]
    pub resume: Option<String>,
    /// UI locale override for terminal chrome (`en`/`es`/`fr`/`de`/`ja`/`zh`).
    /// Tolerant of region subtags (e.g. `fr-FR`, `zh-Hans`). When set it takes
    /// precedence over `$LC_ALL`/`$LANG`; an unrecognized code is ignored and
    /// resolution falls through to the environment, then English. Default-off:
    /// unset leaves chrome rendering exactly as before.
    #[arg(long = "lang")]
    pub lang: Option<String>,
    #[command(subcommand)]
    pub cmd: Option<Cmd>,
}

#[derive(Subcommand)]
pub enum Cmd {
    /// Query the trace ring (P11.11). Without any flags, prints the most
    /// recent 100 spans across every kind.
    Trace {
        #[command(subcommand)]
        sub: TraceSub,
    },
    /// Start or redeem a pairing session for remote QUIC clients (P13.2).
    Pair {
        #[command(subcommand)]
        sub: PairSub,
    },
    /// One-shot prompt: connect to the daemon, send `text`, drain to completion, exit.
    Run {
        /// The user prompt.
        text: String,
        /// Emit JSON-Lines stream of every IPC event.
        #[arg(long)]
        json: bool,
        /// Remote daemon URL (`origin://host:port#fingerprint`).
        #[arg(long)]
        remote: Option<String>,
        /// Optional bearer token for remote auth.
        #[arg(long)]
        bearer: Option<String>,
        /// Model override.
        #[arg(long)]
        model: Option<String>,
        /// Reasoning-effort level (`fast`/`low`/`medium`/`high`/`max`). When
        /// omitted, falls back to the global `--effort`. Maps to the provider
        /// effort wire; unknown values leave the wire unchanged.
        #[arg(long)]
        effort: Option<String>,
        /// Extended-thinking budget in tokens for this turn (Anthropic). When
        /// omitted, falls back to the global `--thinking-tokens`. `0` is
        /// rejected. Only the Anthropic provider honours it. *Closes: aider
        /// `--thinking-tokens`.*
        #[arg(long = "thinking-tokens")]
        thinking_tokens: Option<u32>,
        /// Define an ad-hoc model alias for this invocation, as
        /// `name=provider/model` (or `name=bare-model-id`). Repeatable. Resolved
        /// before the prompt is sent, in addition to any `[aliases]` table in
        /// `~/.origin/config.toml`. Ad-hoc aliases take precedence over config.
        #[arg(long = "alias")]
        alias: Vec<String>,
        /// Attach an image or PDF as multimodal context (repeatable). Each
        /// file is classified and base64/text-encoded into the first user turn.
        #[arg(long = "attach")]
        attach: Vec<String>,
        /// Stdout contract: `text` (default human text), `json` (a single final
        /// JSON object), or `stream-json` (JSON-Lines of every IPC event, the
        /// same shape as `--json`).
        #[arg(long = "output-format")]
        output_format: Option<String>,
        /// Path to a JSON Schema the final answer must satisfy. Enables
        /// structured-output mode: the schema is injected into the prompt, the
        /// reply is validated, and on failure the model is re-prompted (bounded
        /// retries). Emits only the validated JSON object on success.
        #[arg(long = "json-schema")]
        json_schema: Option<String>,
        /// Extra workspace root the agent may read/edit across (repeatable,
        /// cline multi-root). Surfaced to the model as a `<workspace-roots>`
        /// block.
        #[arg(long = "root")]
        root: Vec<String>,
    },
    /// Workload Identity Federation token exchange (RFC 8693): mint a bearer
    /// token from a CI OIDC id token for keyless provider auth.
    OidcExchange {
        /// STS endpoint that performs the exchange.
        #[arg(long = "token-url")]
        token_url: String,
        /// Subject token: a literal JWT, `@<path>` (read file), or `env:<NAME>`.
        #[arg(long = "subject-token")]
        subject_token: String,
        /// Target audience for the exchanged token.
        #[arg(long)]
        audience: String,
        /// Optional Anthropic workspace id (`ANTHROPIC_WORKSPACE_ID`).
        #[arg(long = "workspace-id")]
        workspace_id: Option<String>,
        /// Optional federation rule id (`anthropic_federation_rule_id`).
        #[arg(long = "federation-rule-id")]
        federation_rule_id: Option<String>,
        /// Emit the full `ExchangedToken` as JSON instead of just the token.
        #[arg(long)]
        json: bool,
    },
    /// Daemon usage snapshot (tokens in/out per provider/model).
    Usage,
    /// Per-session cost / usage insights: the usage+COST table plus a
    /// session-insights footer with a prompt-cache warm/cold nudge.
    Insights,
    /// Manage persisted sessions.
    Sessions {
        #[command(subcommand)]
        sub: SessionsSub,
    },
    /// Manage stored provider credentials.
    Keyring {
        #[command(subcommand)]
        sub: KeyringSub,
    },
    /// Import a session/skill set from another harness (P14.B.7).
    Import(crate::import::ImportArgs),
    /// Cross-harness *live resume*: reconstruct a foreign harness's transcript
    /// (Claude Code / jcode / opencode) into a brand-new resumable origin
    /// session, then continue it with `origin sessions resume <id>`. Unlike
    /// `origin import` (which only stores history), this hydrates a session you
    /// can keep talking to. *Closes: jcode L227.*
    ResumeForeign {
        /// Originating harness: `claude-code` | `jcode` | `opencode` (aliases
        /// `claude`/`cc`/`oc` are also accepted).
        source: String,
        /// Path to the external session file or harness root directory.
        path: String,
    },
    /// List and describe known providers from the builtin catalog.
    Providers {
        #[command(subcommand)]
        sub: ProvidersSub,
    },
    /// Interactive first-time setup. Picks primary / backup / subagent
    /// providers and models, captures credentials, and writes
    /// `~/.origin/config.toml`. Re-running overwrites the existing config.
    Init,
    /// Environment & runtime diagnostics + a privacy disclosure of every
    /// outbound behaviour (openclaude `doctor:runtime` / `verify:privacy`).
    Doctor {
        /// Emit the report as JSON instead of human-readable text.
        #[arg(long)]
        json: bool,
        /// Only print the privacy / phone-home disclosure and exit.
        #[arg(long)]
        privacy: bool,
    },
    /// Render a mermaid flowchart to ASCII in the terminal — a dependency-free
    /// renderer (jcode parity). Reads a file, or stdin when PATH is `-`.
    Mermaid {
        /// Path to a `.mmd`/`.md` file, or `-` for stdin.
        path: String,
    },
    /// Local knowledge / semantic index (`/knowledge`) persisted to
    /// `~/.origin/knowledge.json`.
    Knowledge {
        #[command(subcommand)]
        sub: KnowledgeSub,
    },
    /// Manage scheduled / recurring triggers (cron, `@every`, `@daily`,
    /// webhook, fs-event) persisted to `~/.origin/schedule.toml`.
    Schedule {
        #[command(subcommand)]
        sub: ScheduleSub,
    },
    /// Export a persisted session transcript to Markdown or JSON.
    Export {
        /// Session id to export.
        session_id: String,
        /// Emit JSON instead of Markdown.
        #[arg(long)]
        json: bool,
        /// Write to this file instead of stdout.
        #[arg(long, short = 'o')]
        out: Option<String>,
    },
    /// Snapshot the working tree into the shadow-git checkpoint history.
    Checkpoint {
        /// Optional human label for the checkpoint.
        label: Option<String>,
    },
    /// List shadow-git checkpoints, newest first.
    Checkpoints,
    /// Restore the working tree from a checkpoint id.
    Rewind {
        /// Checkpoint id to restore.
        id: String,
        /// Restore only the tracked files (do not move HEAD).
        #[arg(long)]
        files_only: bool,
        /// Restore only these paths from the checkpoint (repeatable, e.g.
        /// `--path src/a.rs --path src/b.rs`). Scopes the restore to the given
        /// files without moving HEAD; mutually exclusive with `--files-only`.
        #[arg(long = "path", conflicts_with = "files_only")]
        path: Vec<String>,
    },
    /// Print the patch for a checkpoint id.
    CheckpointDiff {
        /// Checkpoint id to diff.
        id: String,
    },
    /// Manage auto-memory (the mem-garden draft inbox).
    Memory {
        #[command(subcommand)]
        sub: MemorySub,
    },
    /// Shallow-clone a dependency repo and print a compact overview.
    Scout {
        /// Repository URL to clone.
        repo_url: String,
        /// Cache directory (defaults to `~/.origin/scout`).
        #[arg(long)]
        cache: Option<String>,
    },
    /// Scan a source tree for `AI` / `AI!` / `AI?` trigger comments.
    Watch {
        /// Root directory to scan (defaults to the current directory).
        #[arg(long)]
        root: Option<String>,
        /// Comma-separated file extensions to scan (defaults to a builtin set).
        #[arg(long)]
        ext: Option<String>,
    },
    /// Bundle files + an instruction onto the clipboard for a web chat.
    CopyContext {
        /// Optional instruction for the model.
        #[arg(long, short = 'm')]
        instruction: Option<String>,
        /// Files to include in the bundle.
        #[arg(required = true)]
        files: Vec<String>,
    },
    /// Apply edits pasted from a web chat (read from the clipboard).
    ApplyClipboard,
    /// Dictate a prompt via an external speech-to-text engine.
    Dictate {
        /// Emit each transcript line eagerly instead of buffering utterances.
        #[arg(long)]
        interleave: bool,
        /// Spoken-language hint passed to the STT engine.
        #[arg(long)]
        lang: Option<String>,
        /// Capture device passed to the STT engine.
        #[arg(long)]
        device: Option<String>,
    },
    /// Pluggable web search (`DuckDuckGo` / Brave / Tavily).
    Search {
        /// The search query.
        query: String,
        /// Engine: `ddg` (default), `brave`, or `tavily`.
        #[arg(long)]
        engine: Option<String>,
    },
    /// Discover and inspect plugins / cross-tool skills.
    Plugin {
        #[command(subcommand)]
        sub: PluginSub,
    },
    /// Inspect the builtin LSP server registry (opencode-style fleet).
    Lsp {
        #[command(subcommand)]
        sub: LspSub,
    },
    /// Ambient / overnight autonomous mode (jcode Ambient/OpenClaw + Overnight).
    Ambient {
        #[command(subcommand)]
        sub: AmbientSub,
    },
    /// Run the origin-bench reliability harness and emit the multi-sample
    /// report (pass@k / pass^k / flakiness + a failure histogram).
    ///
    /// Default (live) path: the bench task set is run `--samples` times per
    /// task through the offline-capable subprocess runner. With
    /// `--from <results.json>` a recorded `TaskResult` array is grouped into
    /// per-task samples and rendered instead — no provider/daemon needed.
    Bench {
        /// Number of independent runs collected per task; also the `k` used for
        /// the `pass@k` / `pass^k` columns. Capped to keep work bounded.
        #[arg(long, default_value_t = 1)]
        samples: u32,
        /// Emit the report as JSON instead of Markdown.
        #[arg(long)]
        json: bool,
        /// Compute the report from a recorded `TaskResult` JSON array at this
        /// path (offline) instead of running the task set live. Repeatable: with
        /// `--leaderboard`, pass one `--from` per model to rank them together.
        #[arg(long)]
        from: Vec<String>,
        /// Aggregate every `--from` file into a ranked cross-model leaderboard
        /// (best mean pass@k first) instead of a single reliability report.
        #[arg(long)]
        leaderboard: bool,
    },
    /// Confidence-scored, multi-dimension review of the working-tree diff vs
    /// `HEAD`. Runs fully local static heuristics through `origin-review`'s
    /// confidence dedup + strictness filter and prints a deduped report
    /// (claude-code multi-agent confidence-scored review, local half).
    Review {
        /// How aggressively to surface findings: `strict` (high-confidence
        /// only), `balanced` (default), or `lenient` (surface almost
        /// everything).
        #[arg(long, default_value = "balanced")]
        strictness: String,
    },
    /// First-class Gmail tool over Google OAuth. Loads credentials from the
    /// keyvault (`google`/`gmail`), runs one operation, and prints the JSON
    /// result. OP is `search`, `get`, `list_threads`, or `login` (run the
    /// interactive loopback OAuth flow to mint and store the refresh token).
    Gmail {
        /// The operation: `search`, `get`, `list_threads`, or `login`.
        op: String,
        /// Gmail search expression (for `search` / `list_threads`).
        #[arg(long)]
        query: Option<String>,
        /// Message id (for `get`).
        #[arg(long)]
        id: Option<String>,
        /// Max results for list operations (clamped to 1..=500).
        #[arg(long)]
        max: Option<u32>,
        /// For `get`: fetch the full message and decode its text body (costs
        /// more tokens). Defaults to metadata-only.
        #[arg(long = "include-body")]
        include_body: bool,
        /// OAuth client id (for `login`). Falls back to `GMAIL_CLIENT_ID`.
        #[arg(long = "client-id")]
        client_id: Option<String>,
        /// OAuth client secret (for `login`). Falls back to
        /// `GMAIL_CLIENT_SECRET`.
        #[arg(long = "client-secret")]
        client_secret: Option<String>,
        /// Loopback redirect port (for `login`). `0` (default) lets the OS pick
        /// an ephemeral port; the chosen port must be an authorized redirect.
        #[arg(long, default_value_t = 0)]
        port: u16,
    },
    /// Dynamic workflow authoring + run substrate.
    Workflow {
        #[command(subcommand)]
        sub: WorkflowSub,
    },
    /// Supervised binary self-development (gated `ORIGIN_SELFDEV=1`). Drives the
    /// daemon's edit → checkpoint → build → test → restart cycle.
    Selfdev {
        #[command(subcommand)]
        sub: SelfdevSub,
    },
    /// Named agent teams: register teams, assign tasks to teammates, and render
    /// a team's mission log + teammate statuses (origin-swarm control plane).
    Team {
        #[command(subcommand)]
        sub: TeamSub,
    },
}

/// `origin memory …` subcommands.
#[derive(Subcommand)]
pub enum MemorySub {
    /// Operate on the auto-memory inbox (drafts staged by the daemon's
    /// mem-garden; distinct from the in-session `/mem` proposal queue).
    Inbox {
        #[command(subcommand)]
        sub: MemoryInboxSub,
    },
}

/// `origin memory inbox …` subcommands.
#[derive(Subcommand)]
pub enum MemoryInboxSub {
    /// List the staged auto-memory drafts.
    List,
    /// Promote a draft into the live memory store, then remove it from the inbox.
    Accept {
        /// Draft id (full content-hash or a unique prefix).
        id: String,
    },
    /// Discard a draft without saving it.
    Reject {
        /// Draft id (full content-hash or a unique prefix).
        id: String,
    },
}

/// `origin workflow …` subcommands.
#[derive(Subcommand)]
pub enum WorkflowSub {
    /// Author a workflow from a natural-language goal, render its TOML, and
    /// persist it to `~/.origin/workflows.toml` so it is runnable via
    /// `{workflow:<name>}`.
    Author {
        /// Natural-language description of what the workflow should accomplish.
        #[arg(required = true)]
        goal: Vec<String>,
        /// Optional explicit workflow name (overrides the slug derived from the
        /// goal).
        #[arg(long)]
        name: Option<String>,
    },
    /// Run an authored workflow by name as a phase-layered parallel DAG of
    /// sub-agents (the daemon dispatches one swarm worker per step per layer,
    /// independent same-layer steps concurrently), then print the run summary.
    Run {
        /// Name of an authored workflow in `~/.origin/workflows.toml`.
        #[arg(required = true)]
        name: String,
    },
}

/// `origin selfdev …` subcommands.
#[derive(Subcommand)]
pub enum SelfdevSub {
    /// Queue a self-modification job and begin the supervised cycle.
    Start {
        /// Human description of the change (also the prompt driven onto the
        /// agent for the self-edit step).
        #[arg(required = true)]
        description: Vec<String>,
        /// Source path the job intends to touch (repeatable; empty ⇒ unscoped).
        #[arg(long = "path")]
        path: Vec<String>,
    },
    /// Query the self-dev driver's current state.
    Status,
    /// Approve the in-flight self-dev restart.
    Approve,
    /// Reset the storm guard after acknowledging repeated failures.
    Reset,
}

/// `origin team …` subcommands.
#[derive(Subcommand)]
pub enum TeamSub {
    /// Register a named team (idempotent-by-replace).
    Create {
        /// Team name (unique within the daemon-wide registry).
        name: String,
    },
    /// Assign a task to a (possibly new) named teammate within a team.
    Assign {
        /// The team the teammate belongs to.
        team: String,
        /// Human-facing teammate name (created on first assign).
        teammate: String,
        /// The task description handed to the teammate.
        #[arg(required = true)]
        task: Vec<String>,
    },
    /// Render a team's mission log + per-teammate statuses.
    Status {
        /// The team to render.
        team: String,
    },
}

/// `origin ambient …` subcommands.
#[derive(Subcommand)]
pub enum AmbientSub {
    /// Render the morning report for the most recent ambient session, plus the
    /// standing overnight plan. Read-only: rendering schedules no work.
    Report,
}

/// `origin lsp …` subcommands.
#[derive(Subcommand)]
pub enum LspSub {
    /// List every server in the builtin registry (language, server id, launch
    /// command, extensions, install hint).
    Ls,
    /// Resolve the language server for a file extension and report whether its
    /// binary is already on `PATH`, the would-launch command, and the install
    /// hint. Read-only by default; set `ORIGIN_LSP_AUTO=1` to additionally print
    /// the launch the daemon would perform (still no spawn from the CLI).
    Ensure {
        /// File extension to resolve (with or without a leading dot).
        ext: String,
    },
}

#[derive(Subcommand)]
pub enum PluginSub {
    /// List discovered `.claude` / `.agents` skills.
    Ls,
    /// Parse a plugin manifest and report its surface + context cost.
    Info {
        /// Path to a plugin manifest TOML file.
        manifest: String,
    },
    /// Install a plugin bundle from a local path or git URL into ~/.origin/plugins/.
    Install {
        /// Local directory path or git URL (http(s)/git/ssh) of the bundle.
        source: String,
    },
}

#[derive(Subcommand)]
pub enum KnowledgeSub {
    /// Index a document by id with free text.
    Add {
        /// Stable document id.
        id: String,
        /// The text to index.
        text: String,
    },
    /// Full-text search the index; prints the top hits.
    Search {
        /// Query string.
        query: String,
        /// Maximum number of hits.
        #[arg(long, default_value_t = 5)]
        k: usize,
    },
    /// Remove a document by id.
    Rm { id: String },
    /// List every indexed document id.
    Ls,
}

#[derive(Subcommand)]
pub enum ScheduleSub {
    /// Add a trigger. SPEC is `@every 5m`, `@daily 09:30`, or a 5-field cron
    /// (`min hour dom mon dow`). The PROMPT is run when the trigger fires.
    Add {
        /// Unique trigger id.
        id: String,
        /// Schedule spec.
        spec: String,
        /// Prompt template to run on fire.
        prompt: String,
    },
    /// List configured triggers and their next fire time.
    Ls,
    /// Remove a trigger by id.
    Rm { id: String },
}

#[derive(Subcommand)]
pub enum ProvidersSub {
    /// List every catalog entry (id, display name, wire, auth, capabilities).
    Ls,
    /// Print one provider's full config.
    Describe { id: String },
    /// Best-effort refresh of the runtime model catalog from a custom
    /// provider's `/models` endpoint, persisting the result to the on-disk
    /// model cache (`~/.origin/models-cache.json`).
    ///
    /// When no custom provider with a reachable models endpoint and resolvable
    /// API key is configured, this prints a clear message and changes nothing.
    Refresh {
        /// Refresh only this custom provider; defaults to the first configured
        /// custom provider.
        #[arg(long)]
        provider: Option<String>,
    },
    /// Recommend the cheapest capable model from a candidate set, ranked by the
    /// builtin pricing table, and optionally save the pick as a profile.
    Recommend {
        /// Candidate models to rank (`provider/model` or bare model id). When
        /// omitted, a builtin set spanning the major families is ranked.
        models: Vec<String>,
        /// Persist the recommendation to `~/.origin/recommended.json`.
        #[arg(long)]
        write: bool,
    },
}

#[derive(Subcommand)]
pub enum TraceSub {
    /// Print spans matching the given filters.
    Query(TraceQuery),
}

#[derive(Subcommand)]
pub enum SessionsSub {
    /// List recent sessions (most-recent first).
    Ls,
    /// Resume a session by id: reloads its transcript so the model recalls the
    /// earlier conversation (real `ResumeSession` round-trip to the daemon).
    Resume { session_id: String },
    /// Delete a session and all its messages.
    Rm { session_id: String },
    /// Conversation rewind: keep only the first `--keep` turns of a session,
    /// dropping the rest (the session itself stays resumable).
    Rewind {
        session_id: String,
        /// Number of leading turns to keep (turns at or after this index are
        /// removed).
        #[arg(long)]
        keep: u32,
    },
}

#[derive(Subcommand)]
pub enum KeyringSub {
    /// Add or overwrite a provider secret.
    Add {
        provider: String,
        account: String,
        /// The secret value; read from stdin if `-`.
        secret: String,
    },
    /// List accounts for a provider.
    List { provider: String },
    /// Remove a provider account secret.
    Remove { provider: String, account: String },
    /// Launch the OAuth flow for an OAuth provider and persist the tokens.
    Login {
        /// Catalog id of an OAuth-backed provider, e.g. "github-copilot" or
        /// "anthropic-oauth".
        provider: String,
        /// Account name to store the tokens under. Defaults to "default".
        #[arg(default_value = "default")]
        account: String,
    },
}

#[derive(Subcommand)]
pub enum PairSub {
    /// Daemon-side: show a 6-digit pairing code.
    Start {
        #[arg(long, default_value_t = 60)]
        ttl_secs: u32,
    },
    /// Client-side: redeem a code against a remote daemon.
    Redeem {
        /// Remote URL: `origin://host:port#fingerprint`.
        url: String,
        /// The 6-digit code shown on the daemon host.
        code: String,
        /// Stable device identifier (defaults to hostname).
        #[arg(long)]
        device_id: Option<String>,
    },
}

/// Build the top-level clap command for `origin` for introspection.
///
/// Used by `xtask manpages` to render man pages via `clap_mangen` without
/// having to depend on the binary crate.
#[must_use]
pub fn main_cli() -> clap::Command {
    <Cli as clap::CommandFactory>::command()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod gap4_rewind_path_tests {
    use super::{Cli, Cmd};
    use clap::Parser;

    #[test]
    fn rewind_accepts_repeatable_path() {
        let cli = Cli::try_parse_from([
            "origin", "rewind", "cafe", "--path", "src/a.rs", "--path", "src/b.rs",
        ])
        .expect("parse");
        let Some(Cmd::Rewind { id, files_only, path }) = cli.cmd else {
            panic!("expected Cmd::Rewind");
        };
        assert_eq!(id, "cafe");
        assert!(!files_only);
        assert_eq!(path, vec!["src/a.rs".to_string(), "src/b.rs".to_string()]);
    }

    #[test]
    fn rewind_path_conflicts_with_files_only() {
        let res = Cli::try_parse_from(["origin", "rewind", "cafe", "--files-only", "--path", "a.rs"]);
        assert!(res.is_err(), "--path and --files-only must be mutually exclusive");
    }
}
