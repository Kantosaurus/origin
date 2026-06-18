# origin-sandbox

> Per-tool sandbox profiles for Linux, macOS, and Windows.

## Purpose

`origin-sandbox` confines a child process spawned by a tool to a least-privilege
profile. It exposes one entry point, [`apply`], that mutates a
`std::process::Command` so the spawned child runs under the requested
[`SandboxProfile`]. Per-OS backends (Linux landlock + seccomp, macOS sandbox +
rlimits, Windows Job Objects) are compiled in behind cargo features; on
unsupported hosts a logging no-op backend is used. Profiles are addressed by a
stable `u8` ordinal so dispatch never touches a string table.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `apply` | fn | Mutate a `Command` to enforce a profile on its child. |
| `SandboxProfile` | enum | `Inherit`, `ReadFs`, `WriteCwd`, `Shell`, `Network`. |
| `ProfileOrdinal` | struct | Stable `u8` wire ordinal for a profile. |
| `SandboxProfile::ordinal` / `from_ordinal` | fn | Convert to/from the wire ordinal. |
| `SandboxError` | enum | `Unavailable`, `Apply(String)`, `Io`. |
| `backend_noop::apply` | fn | Fallback backend (warns so opt-out is visible). |
| `caps::apply_caps` | fn | Per-OS CPU/RAM rlimit helper. |

## Key types

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxProfile {
    #[default]
    Inherit,   // child inherits the daemon's privileges
    ReadFs,    // read-only fs scoped to workspace + std libs
    WriteCwd,  // read-only outside cwd, read+write inside session cwd
    Shell,     // read+write cwd, exec stdlib binaries, no network
    Network,   // read-only fs + outbound HTTPS + DNS; no write, no listen
}

pub struct ProfileOrdinal(pub u8);

pub fn apply(profile: SandboxProfile, cmd: &mut std::process::Command)
    -> Result<(), SandboxError>;
```

The ordinals (`Inherit=0 … Network=4`) are part of the public ABI: they ride on
`LifecycleEvent::PreTool`/`PostTool` and the hook IPC envelope, so renumbering
is a breaking change.

## How it works

```text
apply(profile, cmd)
   ├── cfg(linux,  feature=linux)   → backend_linux::apply
   ├── cfg(macos,  feature=macos)   → backend_macos::apply
   ├── cfg(windows,feature=windows) → backend_windows::apply
   └── otherwise / no-sandbox       → backend_noop::apply  (tracing::warn!)
```

The Linux backend builds a `LinuxPolicy::for_profile` in the parent, then
installs it inside the forked child's `pre_exec` hook (between `clone()` and
`execve()`) so the daemon's own thread is never poisoned. The policy has two
parts:

- **landlock** path rules — e.g. `ReadFs` grants read on `cwd`, `/usr/lib`,
  `/lib`, `/etc/ssl/certs`; `Shell` adds read+write `cwd` + `/tmp` and read on
  `/usr`, `/bin`, `/etc`; `Network` is read-only on `cwd` + cert/`resolv.conf`.
- **seccomp BPF** — `Network` allows `listen`/`accept`/`accept4`; non-network
  profiles install a `deny_network_filter` that `EPERM`s `socket(AF_INET/AF_INET6)`;
  `Inherit` uses an allow-all `empty_filter`.

`caps::apply_caps` adds `RLIMIT_CPU` (60 s) and `RLIMIT_AS` (1 GiB) via a second
`pre_exec` closure on Linux/macOS; Windows enforces quotas through a Job Object
in `backend_windows.rs` instead, and other targets get a no-op.

## Dependencies & features

- Features: `default = []`, `linux` (`landlock`, `seccompiler`, `caps`, `libc`),
  `macos` (`libc`), `windows`, and the `no-sandbox` escape hatch that forces the
  noop backend on every host (for debuggers / CI matrices lacking kernel support).
- The Linux-only crates are gated under
  `[target.'cfg(target_os = "linux")'.dependencies]` so `--all-features`
  resolves cleanly off-target. Windows uses `windows-sys` (`JobObjects`,
  `ToolHelp`, `Threading`).
- The crate sets `unsafe_code = "allow"` (per-OS FFI) but `deny`s
  `undocumented_unsafe_blocks` — every `unsafe` block carries a `// SAFETY:` note.

## Used by

`crates/*/Cargo.toml` matches for `origin-sandbox`:

- `crates/origin-hooks/Cargo.toml`
- `crates/origin-sandbox/Cargo.toml`
- `crates/origin-tools/Cargo.toml`

## Testing

Integration tests under `crates/origin-sandbox/tests/`: `backend_linux.rs`,
`backend_macos.rs`, `backend_windows.rs`, and `profile.rs` (ordinal round-trip).
The OS backends are exercised on their native targets; `profile.rs` is
host-independent. `tempfile` is a dev-dependency for filesystem-rule assertions.

## See also

- [Security model](../security/security-model.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
