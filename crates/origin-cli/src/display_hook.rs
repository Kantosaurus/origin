// SPDX-License-Identifier: Apache-2.0
//! CLI-side `MessageDisplay` shell hook.
//!
//! The daemon's [`origin_daemon::hooks_runtime`] fires lifecycle hooks at
//! agent-loop boundaries; the assistant-text render path lives in the CLI, so
//! the `MessageDisplay` hook has to fire here, next to the TUI scrollback. This
//! module mirrors `hooks_runtime`: it loads `~/.origin/hooks.json` once, keeps a
//! pre-spawned [`ShellPool`](origin_hooks::ShellPool) for any `MessageDisplay`
//! hook, and exposes [`message_display_action`] — fire the hook on the rendered
//! text and map its verdict onto an [`origin_outputstyle::DisplayAction`] that
//! feeds the existing `resolve_display` composition.
//!
//! **Default-off:** with no `hooks.json` (or no `MessageDisplay` entry) the
//! loader yields `None`, no child process is spawned, and the action is `None`
//! ⇒ the output-style transform alone decides the render ⇒ byte-identical to
//! the no-hook path. Every failure mode (config error, spawn failure, dispatch
//! error) collapses to `None` (identity): a misbehaving hook never loses the
//! assistant message.
//!
//! *Closes: claude-code `MessageDisplay` shell hook (CLI render side).*

use std::path::PathBuf;
use std::sync::Arc;

use origin_hooks::{dispatch_event, HookEventKind, HookOverride, HooksConfig, LifecycleEvent, ShellPool};
use origin_outputstyle::DisplayAction;
use tokio::sync::OnceCell;

/// A live, pre-spawned pool for the configured `MessageDisplay` hook.
///
/// Holds a single pool: the first `MessageDisplay` entry in `hooks.json` wins,
/// matching the one-message-one-verdict shape of the render path.
struct MessageDisplayHook {
    pool: ShellPool,
}

impl MessageDisplayHook {
    /// Build from `~/.origin/hooks.json`. `None` when the file is absent/empty,
    /// carries no `MessageDisplay` hook, or the pool fails to spawn — i.e. the
    /// hook is simply off, never an error.
    async fn build() -> Option<Self> {
        let cfg = load_config()?;
        let entry = cfg.entries_for(HookEventKind::MessageDisplay).next()?;
        match ShellPool::new(entry.spec(), entry.effective_pool_size()).await {
            Ok(pool) => {
                tracing::info!(program = %entry.program, "display-hook: MessageDisplay hook active");
                Some(Self { pool })
            }
            Err(e) => {
                tracing::warn!(program = %entry.program, error = %e, "display-hook: pool spawn failed; hook disabled");
                None
            }
        }
    }

    /// Fire the hook on `text`, returning the mapped [`DisplayAction`] — or
    /// `None` (passthrough / identity) on any error or no-opinion verdict.
    async fn action_for(&self, text: &str) -> Option<DisplayAction> {
        let event = LifecycleEvent::MessageDisplay {
            text: text.to_string(),
        };
        match dispatch_event(&self.pool, &event).await {
            Ok(over) => override_to_action(&over),
            Err(e) => {
                tracing::warn!(error = %e, "display-hook: dispatch failed; rendering original text");
                None
            }
        }
    }
}

/// Map a hook's [`HookOverride`] onto a render [`DisplayAction`].
///
/// `Deny` suppresses the message ([`DisplayAction::Hide`]); `Mutate` rewrites it
/// ([`DisplayAction::Replace`]); `Passthrough` / `Allow` are "no opinion" ⇒
/// `None` so the output-style transform keeps deciding (identity by default).
fn override_to_action(over: &HookOverride) -> Option<DisplayAction> {
    match over {
        HookOverride::Deny { .. } => Some(DisplayAction::Hide),
        HookOverride::Mutate { patch } => Some(DisplayAction::Replace(patch.clone())),
        HookOverride::Passthrough | HookOverride::Allow { .. } => None,
    }
}

/// Load + validate the hooks config; `None` when absent, empty, or unreadable.
fn load_config() -> Option<HooksConfig> {
    let path = hooks_path()?;
    match HooksConfig::load(&path) {
        Ok(c) if !c.is_empty() => Some(c),
        Ok(_) => None,
        Err(e) => {
            tracing::warn!(error = %e, "display-hook: failed to load hooks.json; hook disabled");
            None
        }
    }
}

/// `~/.origin/hooks.json`, honoring `ORIGIN_HOME` (tests) then the home dir.
fn hooks_path() -> Option<PathBuf> {
    let home = std::env::var_os("ORIGIN_HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)?;
    Some(home.join(".origin").join("hooks.json"))
}

/// Process-wide `MessageDisplay` hook, built once on first access. `None` ⇒ no
/// hook configured ⇒ the render path fires nothing (byte-identical default).
async fn global() -> Option<Arc<MessageDisplayHook>> {
    static CELL: OnceCell<Option<Arc<MessageDisplayHook>>> = OnceCell::const_new();
    CELL.get_or_init(|| async { MessageDisplayHook::build().await.map(Arc::new) })
        .await
        .clone()
}

/// Resolve the [`DisplayAction`] a `MessageDisplay` hook decides for `text`.
///
/// Best-effort and default-off: with no `hooks.json` / no `MessageDisplay` hook
/// — or on any load/spawn/dispatch error — this returns `None`, leaving the
/// output-style transform to decide and keeping the render byte-identical to the
/// no-hook path. A `Some(action)` is fed straight into
/// `origin_outputstyle::resolve_display(text, style, Some(&action))`, where the
/// hook verdict wins over the style.
pub async fn message_display_action(text: &str) -> Option<DisplayAction> {
    let hook = global().await?;
    hook.action_for(text).await
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use origin_outputstyle::resolve_display;

    #[test]
    fn override_passthrough_is_identity() {
        // No opinion ⇒ `None` ⇒ the output-style transform keeps deciding.
        assert_eq!(override_to_action(&HookOverride::Passthrough), None);
        assert_eq!(
            override_to_action(&HookOverride::Allow { reason: "ok".into() }),
            None
        );
    }

    #[test]
    fn override_deny_hides() {
        assert_eq!(
            override_to_action(&HookOverride::Deny {
                reason: "secret".into()
            }),
            Some(DisplayAction::Hide)
        );
    }

    #[test]
    fn override_mutate_replaces() {
        assert_eq!(
            override_to_action(&HookOverride::Mutate {
                patch: "[redacted]".into()
            }),
            Some(DisplayAction::Replace("[redacted]".into()))
        );
    }

    #[test]
    fn absent_config_file_yields_no_hook() {
        // A missing hooks.json loads as an empty config ⇒ no hook ⇒ the loader
        // selects nothing (pure: no env mutation, no shell spawned). Mirrors the
        // `entries_for(MessageDisplay)` selection `build` performs.
        let cfg = HooksConfig::load(std::path::Path::new("/no/such/display-hook-xyz/hooks.json")).unwrap();
        assert!(cfg.is_empty());
        assert!(cfg.entries_for(HookEventKind::MessageDisplay).next().is_none());
    }

    #[test]
    fn config_without_message_display_entry_yields_no_action() {
        // A hooks.json with only a non-display hook: the loader finds a config
        // but `entries_for(MessageDisplay)` is empty, so no pool is built.
        let json = r#"{ "hooks": [ { "event": "pre_tool", "program": "guard" } ] }"#;
        let cfg = HooksConfig::from_json_str(json).unwrap();
        assert!(cfg.entries_for(HookEventKind::MessageDisplay).next().is_none());
    }

    #[test]
    fn config_with_message_display_entry_is_selected() {
        // Positive selection: a `MessageDisplay` hook is found by the same
        // `entries_for` lookup `build` uses before spawning a pool.
        let json = r#"{ "hooks": [ { "event": "message_display", "program": "redact.sh" } ] }"#;
        let cfg = HooksConfig::from_json_str(json).unwrap();
        let entry = cfg.entries_for(HookEventKind::MessageDisplay).next();
        assert!(entry.is_some());
        assert_eq!(entry.unwrap().program, "redact.sh");
    }

    #[test]
    fn action_composes_into_resolve_display() {
        // The composition contract the CLI relies on: a `Some(action)` from the
        // hook wins over the style; `None` ⇒ identity (default style).
        // Hide suppresses.
        let hide = override_to_action(&HookOverride::Deny {
            reason: String::new(),
        })
        .unwrap();
        assert_eq!(resolve_display("secret", None, Some(&hide)), None);
        // Replace rewrites.
        let repl = override_to_action(&HookOverride::Mutate { patch: "new".into() }).unwrap();
        assert_eq!(resolve_display("old", None, Some(&repl)), Some("new".to_string()));
        // Absent action ⇒ identity render.
        assert_eq!(resolve_display("keep", None, None), Some("keep".to_string()));
    }
}
