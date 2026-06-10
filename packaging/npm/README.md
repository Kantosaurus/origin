# @kantosaurus/origin

**Origin** — a fast, terminal-native AI coding agent.

Installing this package puts an `origin` command on your `PATH`. Run it with no
arguments and the TUI comes up.

```sh
npm install -g @kantosaurus/origin
origin
```

> The npm package is scoped (`@kantosaurus/origin`) because the unscoped name
> was unavailable. The installed command is always **`origin`**.

## What gets installed

`@kantosaurus/origin` ships no JavaScript runtime of its own — it is a thin
launcher around a single prebuilt native binary. On install, npm pulls exactly
one small platform package for your OS/CPU (via `optionalDependencies`):

| Platform        | Package                              |
| --------------- | ------------------------------------ |
| Linux x64       | `@kantosaurus/origin-linux-x64`      |
| Linux arm64     | `@kantosaurus/origin-linux-arm64`    |
| macOS x64       | `@kantosaurus/origin-darwin-x64`     |
| macOS arm64     | `@kantosaurus/origin-darwin-arm64`   |
| Windows x64     | `@kantosaurus/origin-win32-x64`      |
| Windows arm64   | `@kantosaurus/origin-win32-arm64`    |

If that package is unavailable (e.g. you installed with `--omit=optional`), a
postinstall step downloads the matching binary from the
[GitHub release](https://github.com/Kantosaurus/origin/releases) instead, and as
a last resort the binary is fetched on first run.

## Usage

```sh
origin                 # launch the interactive TUI
origin --help          # list subcommands
origin run "…"         # one-shot headless run
```

## Updates

`origin` **auto-updates by default** via its built-in self-updater. On launch it
checks the npm registry (at most once per day, cached) and, when a newer version
is published, downloads the matching release binary, verifies its SHA-256 against
the release `SHA256SUMS` (no `cosign` CLI required), swaps itself in place, and
relaunches your command on the new version. The brief download happens only the
first time a new release is seen.

- It only updates **npm-installed** binaries (those under `node_modules`); a
  build from source or a `cargo install` is never touched. It rewrites only the
  installed copy in `node_modules`, so a project's committed dependency range and
  a later `npm ci` are unaffected.
- Prefer npm to own updates? Set `ORIGINX_ALLOW_SELF_UPDATE=0` to fall back to a
  non-blocking background `npm install` that applies on the next run, announced
  once (`origin: updated to vX.Y.Z`).
- Update on demand anytime: `npm update -g @kantosaurus/origin` (or
  `npm update @kantosaurus/origin` in a project).

Disable all auto-update with `ORIGIN_NO_UPDATE=1` (`ORIGINX_NO_UPDATE=1` too).

## Environment knobs

| Variable                      | Effect                                                                       |
| ----------------------------- | ---------------------------------------------------------------------------- |
| `ORIGINX_ALLOW_SELF_UPDATE=0` | Opt OUT of the binary's self-updater (ON by default); fall back to the npm-launcher channel. |
| `ORIGINX_NO_UPDATE=1`         | Disable the npm-launcher background update. (`ORIGIN_NO_UPDATE` is honored too.) |
| `ORIGIN_NO_UPDATE=1`          | Honored by both the launcher and the binary; disables all auto-update.       |
| `ORIGINX_SKIP_DOWNLOAD=1`     | Skip the postinstall binary download (air-gapped / source builds).           |

Auto-update state (last-check timestamp, lock, log) lives under
`${XDG_CACHE_HOME:-~/.cache}/originx/` (`%LOCALAPPDATA%\originx\` on Windows).

## Other install methods

`cargo install`, Homebrew, AUR, winget and `cargo binstall` are also supported —
see the [project README](https://github.com/Kantosaurus/origin#readme).

## License

Apache-2.0
