# origin-vcs

> Agent-native git safety layer: shadow-git checkpoints, restore, rewind, and a lane/draft-patch model

## Purpose

origin's agents edit the user's working tree directly, so one bad turn can
clobber real work. `origin-vcs` adds a *shadow* git history (cline/kilocode-style
checkpoints, aider git-as-undo, gemini `/rewind`) plus an isolated-worktree
helper ("lanes") so destructive work can run off the user's tree. Every git
effect is routed through an injected `GitRunner`, so the whole crate is
unit-tested offline with a recording mock — no subprocess, no repo, no network.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `GitRunner` | trait | `run(args) -> stdout`; owns *all* process/filesystem side effects. |
| `ShadowGit<'a>` | struct | Shadow history over a separate `--git-dir`. |
| `ShadowGit::snapshot` | fn | Stage + commit a checkpoint with a label and `now_ms`. |
| `ShadowGit::list` / `restore` / `diff` | fn | Enumerate, restore (per `RestoreMode`), or diff a checkpoint. |
| `Checkpoint` | struct | `{ id, label, created_at_unix_ms, files_changed }`. |
| `RestoreMode` | enum | `WorkingTree` / `Files(Vec<String>)` / `Full`. |
| `Worktree<'a>` | struct | Lane helper: `add` / `add_existing` / `remove` / `prune` / `list`. |
| `parse_checkpoints` | fn | Parse `git log --format=LOG_FORMAT` output into `Vec<Checkpoint>`. |
| `LOG_FORMAT` | const | The exact `--format=…` argument the parser understands. |
| `VcsError` | enum | `Git(String)` / `NotFound(String)`. |

## Key types

```rust
pub trait GitRunner {
    fn run(&self, args: &[&str]) -> Result<String, VcsError>;
}

pub struct Checkpoint {
    pub id: String,                 // shadow-repo commit hash
    pub label: String,              // turn summary
    pub created_at_unix_ms: u64,
    pub files_changed: u32,
}

pub enum RestoreMode {
    WorkingTree,        // overwrite tree, leave HEAD (gemini /rewind of files)
    Files(Vec<String>),// restore only listed paths
    Full,               // hard reset HEAD + tree (full rewind)
}
```

## How it works

`ShadowGit` always prefixes `--git-dir <shadow_dir>` so checkpoints never touch
the user's real `.git`. `snapshot` stages the tree and commits with the label
plus machine-readable `ms=<unix_ms> files=<n>` metadata in the body. The log is
read back with the fixed `LOG_FORMAT` (`%H\x1f%s\x1f%b\x1e` — `\x1f` between
fields, `\x1e` between records), and `parse_checkpoints` reconstructs each
`Checkpoint` so a round-trip preserves the timestamp and file count. `restore`
maps `RestoreMode` to the right git verbs (checkout subset, `checkout .`, or
`reset --hard`). `Worktree` wraps `git worktree add/remove/prune/list` for lanes
that run risky edits on a throwaway branch.

```
turn end ─▶ ShadowGit.snapshot(label, now_ms) ─▶ Checkpoint{id,…}
            (commits into --git-dir shadow_dir, NOT the real .git)
list ─▶ git log --format=LOG_FORMAT ─▶ parse_checkpoints ─▶ [Checkpoint]
restore(id, RestoreMode) ─▶ checkout/reset ;  GitRunner runs every git call
```

## Dependencies & features

`#![forbid(unsafe_code)]`. Only `serde` (derive on `Checkpoint`/`RestoreMode`)
and `thiserror` (`VcsError`). All git execution is delegated to the caller's
`GitRunner` implementation, keeping the crate pure. Dev-dep `serde_json` backs
serialization tests.

## Used by

```
crates/origin-cli/Cargo.toml
crates/origin-daemon/Cargo.toml
crates/origin-vcs/Cargo.toml
```

## Testing

A recording mock `GitRunner` captures the exact argument vectors each operation
emits, so tests assert the precise git commands for `snapshot`, `restore` (all
three modes), `diff`, and every `Worktree` verb without a real repository. The
doctest in `lib.rs` checks `LOG_FORMAT` shape and the empty-input contract of
`parse_checkpoints`; round-trip tests confirm metadata survives the log format.

## See also

- [Tools subsystem](../subsystems/tools.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
