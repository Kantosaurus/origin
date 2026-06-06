// SPDX-License-Identifier: Apache-2.0
//! Process-wide hooks runtime: lazily-built shell pools fired at agent-loop
//! lifecycle points.
//!
//! [`origin_hooks`] provides the typed [`LifecycleEvent`], the
//! [`ShellPool`](origin_hooks::ShellPool) responder model, and a JSON config
//! ([`HooksConfig`](origin_hooks::HooksConfig)). This module makes them **live**:
//! on first use it loads `~/.origin/hooks.json`, pre-spawns one pool per
//! configured hook, and exposes [`global`] so the agent loop can [`fire`] events
//! at `PrePrompt` / `PreTool` / `PostTool` / `PostPrompt` boundaries.
//!
//! **Default-off:** with no `hooks.json` (or an empty one) [`global`] returns
//! `None`, no child processes are spawned, and the agent loop is byte-identical
//! to running without hooks. A `PreTool` hook that returns a `Deny` override is
//! the security-relevant case — it downgrades the tool's permission decision to
//! Deny, mirroring the static permission/policy overlays.
//!
//! *Closes: gemini lifecycle hooks (the firing wire); foundation for
//! claude-code `MessageDisplay` + opencode plugin event bus.*

use std::path::PathBuf;
use std::sync::Arc;

use origin_hooks::{dispatch_event, HookEventKind, HookOverride, HooksConfig, LifecycleEvent, ShellPool};
use tokio::sync::OnceCell;

/// Live hook pools, one per configured [`HookEntry`](origin_hooks::HookEntry).
pub struct HooksState {
    /// `(kind, pool)` pairs; multiple pools may share a kind (all fire).
    pools: Vec<(HookEventKind, ShellPool)>,
}

impl HooksState {
    /// Build the runtime from `~/.origin/hooks.json`. Returns `None` when the
    /// file is absent/empty or every configured pool fails to spawn — i.e.
    /// hooks are simply off, never an error.
    async fn build() -> Option<Arc<Self>> {
        let cfg = load_config()?;
        let pools = build_pools(&cfg).await;
        if pools.is_empty() {
            return None;
        }
        tracing::info!(count = pools.len(), "hooks: runtime active");
        Some(Arc::new(Self { pools }))
    }

    /// Dispatch `event` to every pool subscribed to its kind, returning the
    /// strongest override: a `Deny` from any hook short-circuits and wins; a
    /// `Mutate` is retained if no hook denied; otherwise `Passthrough`.
    pub async fn fire(&self, event: &LifecycleEvent) -> HookOverride {
        let kind = event.kind();
        let mut result = HookOverride::Passthrough;
        for (k, pool) in &self.pools {
            if *k != kind {
                continue;
            }
            match dispatch_event(pool, event).await {
                Ok(HookOverride::Deny { reason }) => return HookOverride::Deny { reason },
                Ok(HookOverride::Mutate { patch }) => result = HookOverride::Mutate { patch },
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(?kind, error = %e, "hooks: dispatch failed; treating as passthrough");
                }
            }
        }
        result
    }
}

/// Load + validate the hooks config; `None` when absent, empty, or unreadable.
fn load_config() -> Option<HooksConfig> {
    let path = hooks_path()?;
    match HooksConfig::load(&path) {
        Ok(c) if !c.is_empty() => Some(c),
        Ok(_) => None,
        Err(e) => {
            tracing::warn!(error = %e, "hooks: failed to load hooks.json; hooks disabled");
            None
        }
    }
}

/// Pre-spawn one pool per configured hook, skipping any that fail to start.
async fn build_pools(cfg: &HooksConfig) -> Vec<(HookEventKind, ShellPool)> {
    let mut pools = Vec::new();
    for entry in &cfg.hooks {
        match ShellPool::new(entry.spec(), entry.effective_pool_size()).await {
            Ok(p) => pools.push((entry.event, p)),
            Err(e) => {
                tracing::warn!(program = %entry.program, error = %e, "hooks: pool spawn failed; skipping this hook");
            }
        }
    }
    pools
}

/// `~/.origin/hooks.json`, honoring `ORIGIN_HOME` (tests) then the home dir.
fn hooks_path() -> Option<PathBuf> {
    let home = std::env::var_os("ORIGIN_HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)?;
    Some(home.join(".origin").join("hooks.json"))
}

/// Process-wide hooks runtime, built once on first access. `None` ⇒ no hooks
/// configured ⇒ the agent loop fires nothing (byte-identical default).
pub async fn global() -> Option<Arc<HooksState>> {
    static CELL: OnceCell<Option<Arc<HooksState>>> = OnceCell::const_new();
    CELL.get_or_init(HooksState::build).await.clone()
}

/// Fire `event` through the process-wide hooks runtime, ignoring the override.
///
/// Convenience for the informational lifecycle points (`SessionStart`,
/// `SessionEnd`, `PreCommit`, `PostCommit`) whose callers do not act on a hook's
/// `Allow`/`Deny`/`Mutate` decision. It resolves [`global`] and dispatches via
/// the same [`HooksState::fire`] mechanism the agent loop uses for `PrePrompt` /
/// `PreTool`; with no `hooks.json` configured (the default) [`global`] is `None`
/// and this is a no-op — byte-identical to never calling it.
pub async fn fire_global(event: &LifecycleEvent) {
    if let Some(h) = global().await {
        let _ = h.fire(event).await;
    }
}
