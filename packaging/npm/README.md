# originx

**Origin** — a fast, terminal-native AI coding agent.

Installing this package puts an `origin` command on your `PATH`. Run it with no
arguments and the TUI comes up.

```sh
npm install -g originx
origin
```

> The npm package is named `originx` because `origin` was already taken on the
> registry. The installed command is always **`origin`**.

## What gets installed

`originx` ships no JavaScript runtime of its own — it is a thin launcher around a
single prebuilt native binary. On install, npm pulls exactly one small
platform package for your OS/CPU (via `optionalDependencies`):

| Platform        | Package                |
| --------------- | ---------------------- |
| Linux x64       | `originx-linux-x64`    |
| Linux arm64     | `originx-linux-arm64`  |
| macOS x64       | `originx-darwin-x64`   |
| macOS arm64     | `originx-darwin-arm64` |
| Windows x64     | `originx-win32-x64`    |
| Windows arm64   | `originx-win32-arm64`  |

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

`origin` **auto-updates by default**. On launch it checks the npm registry in
the background (at most once per day, never blocking startup) and, when a newer
version is published, installs it via npm. The update applies the next time you
run `origin`, which announces it once (`origin: updated to vX.Y.Z`).

- **Global installs** (`npm i -g`): updated with `npm install -g originx@latest`.
- **Local installs** (a project dependency): updated with
  `npm install originx@latest --no-save` in the project root — this refreshes the
  working copy in `node_modules` **without** rewriting the version your
  `package.json` declares, so your committed dependency range is never touched. A
  clean `npm ci` later restores the pinned version. (Skipped for exotic layouts
  such as a pnpm virtual store, where a plain `npm install` would be unsafe.)
- It uses npm (registry integrity, no extra tooling), not the binary's built-in
  cosign-verified self-updater — that would require the `cosign` CLI most npm
  machines lack.
- Update on demand anytime: `npm update -g originx` (or `npm update originx` in a
  project).

Disable it with `ORIGINX_NO_UPDATE=1`.

## Environment knobs

| Variable                      | Effect                                                                       |
| ----------------------------- | ---------------------------------------------------------------------------- |
| `ORIGINX_NO_UPDATE=1`         | Disable the npm-channel auto-update. (`ORIGIN_NO_UPDATE` is honored too.)     |
| `ORIGINX_ALLOW_SELF_UPDATE=1` | Use the binary's own cosign-verified self-updater instead of the npm channel. |
| `ORIGIN_NO_UPDATE=1`          | Honored by both the launcher and the binary; disables all auto-update.       |
| `ORIGINX_SKIP_DOWNLOAD=1`     | Skip the postinstall binary download (air-gapped / source builds).           |

Auto-update state (last-check timestamp, lock, log) lives under
`${XDG_CACHE_HOME:-~/.cache}/originx/` (`%LOCALAPPDATA%\originx\` on Windows).

## Other install methods

`cargo install`, Homebrew, AUR, winget and `cargo binstall` are also supported —
see the [project README](https://github.com/Kantosaurus/origin#readme).

## License

Apache-2.0
