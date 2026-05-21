//! `origin init` — first-time setup flow.
//!
//! Greets the user, walks them through picking a primary provider (out of
//! the full catalog, with an "Other" escape hatch), captures its credential,
//! probes the credential against the provider's models endpoint, and lets
//! them pick a model from the probed list (falling back to free-text when
//! the wire format doesn't expose one). Repeats for an optional backup
//! and an optional subagent/swarm provider. Persists the (non-secret) role
//! mapping to `~/.origin/config.toml`; secrets stay in the OS keychain via
//! `origin-keyvault`.
//!
//! ## Per-role step order
//!
//! 1. Provider — full catalog menu (grouped by wire) + "Other (catalog id)".
//! 2. Credential — branches on [`AuthScheme`] (API key / OAuth / `SigV4` / none).
//! 3. Probe — issues a GET against the provider's `/models` endpoint;
//!    on auth failure offers a retry loop, on unreachable/skipped continues
//!    after a notice.
//! 4. Model — numbered menu of the probed list (default selected), or a
//!    free-text prompt with the catalog default when no list is available.
//!
//! Modeled on `tutorial.rs`: a pure data table + a runner that takes a
//! [`std::io::BufRead`] + [`std::io::Write`] pair so the flow is unit-testable
//! against scripted input. Tests inject a [`MockProbe`] from
//! [`crate::init_probe`] to avoid real HTTP.

use crate::config::{self, OriginConfig, RoleConfig, SCHEMA_VERSION};
use crate::init_probe::{ConnectivityProbe, LiveProbe, ProbeOutcome, ProbeResult};
use anyhow::{anyhow, Result};
use origin_keyvault::{KeyVault, Secret};
use origin_provider::catalog::{AuthScheme, Catalog, ProviderEntry, WireFormat};
use std::io::{BufRead, Write};

/// Slot the onboarding flow can collect. Used for menu labels and prompts.
#[derive(Debug, Clone, Copy)]
pub enum Role {
    Primary,
    Backup,
    Subagent,
}

impl Role {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Backup => "backup",
            Self::Subagent => "subagent / swarm",
        }
    }
}

/// Entry point used by `main.rs`.
///
/// Wraps the runner with real stdin/stdout, a freshly detected `KeyVault`,
/// the default config path, and a live HTTP connectivity probe; then runs
/// the post-init walkthrough (Toolbox, Skill Repository, port skills,
/// Workflows).
///
/// # Errors
/// Propagates failure from keyvault detection, config-path resolution, the
/// inner [`run_with`] flow, or the post-init walkthrough.
#[allow(clippy::future_not_send)] // CLI entry: stdin/stdout locks are inherently !Send and never crossed.
pub async fn run() -> Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let vault = KeyVault::detect().map_err(|e| anyhow!("keyvault detect: {e}"))?;
    let cfg_path = config::path().map_err(|e| anyhow!("config path: {e}"))?;
    let probe = LiveProbe::new();
    run_with(stdin.lock(), stdout.lock(), &vault, &cfg_path, &probe).await?;
    // Walkthrough takes over stdin/stdout after `run_with` has dropped its
    // locks. We don't pass the welcome flow through `run_with` so that the
    // existing test surface stays focused on the config-capture loop.
    crate::welcome::run()
}

/// Drive the flow against arbitrary reader/writer, an explicit config path,
/// and an injected connectivity probe.
///
/// Tests can stand up an in-memory vault (`KeyVault::in_memory`), a tempdir
/// config, and a [`MockProbe`](crate::init_probe::MockProbe) without touching
/// process-wide env vars (Rust 1.83 flags `set_var` as `unsafe`).
///
/// # Errors
/// Propagates I/O failures from `r`/`w`, vault writes, config persistence,
/// and probe execution.
#[allow(clippy::future_not_send)] // Generic over !Send readers/writers; only awaited from the CLI thread.
pub async fn run_with<R: BufRead, W: Write>(
    mut r: R,
    mut w: W,
    vault: &KeyVault,
    cfg_path: &std::path::Path,
    probe: &dyn ConnectivityProbe,
) -> Result<()> {
    let cat = Catalog::builtin();

    greet(&mut w)?;

    let primary = configure_role(&mut r, &mut w, &cat, vault, probe, Role::Primary).await?;

    let backup = if yes_no(&mut r, &mut w, "Configure a backup provider/model? [y/N]: ", false)? {
        Some(configure_role(&mut r, &mut w, &cat, vault, probe, Role::Backup).await?)
    } else {
        None
    };

    let subagent = if yes_no(
        &mut r,
        &mut w,
        "Configure a separate provider/model for subagents and swarm? [y/N]: ",
        false,
    )? {
        Some(configure_role(&mut r, &mut w, &cat, vault, probe, Role::Subagent).await?)
    } else {
        None
    };

    let cfg = OriginConfig {
        schema_version: SCHEMA_VERSION,
        primary,
        backup,
        subagent,
    };
    config::save_to(cfg_path, &cfg).map_err(|e| anyhow!("save config: {e}"))?;
    writeln!(w, "\nSaved to {}.", cfg_path.display())?;
    writeln!(
        w,
        "Secrets are stored in your OS keychain via `origin keyring`. \
         You can re-run `origin init` any time to overwrite, or use \
         `origin keyring add` / `origin keyring login` for finer control."
    )?;

    Ok(())
}

fn greet<W: Write>(w: &mut W) -> std::io::Result<()> {
    writeln!(w, "Welcome to origin.")?;
    writeln!(
        w,
        "Let's pick the providers and models origin should talk to. \
         This runs once; we'll save the choices to ~/.origin/config.toml \
         and your secrets to your OS keychain."
    )?;
    writeln!(w)?;
    Ok(())
}

/// Per-role steps: provider → auth → probe → model. The probe step loops
/// on auth failure so a typo'd key can be re-entered without restarting
/// the whole flow.
#[allow(clippy::future_not_send)] // Generic over !Send readers/writers; only awaited from the CLI thread.
async fn configure_role<R: BufRead, W: Write>(
    r: &mut R,
    w: &mut W,
    cat: &Catalog,
    vault: &KeyVault,
    probe: &dyn ConnectivityProbe,
    role: Role,
) -> Result<RoleConfig> {
    writeln!(w, "── Configuring {} provider ──", role.label())?;
    let entry = pick_provider(r, w, cat)?;
    let account = "default".to_string();

    // Loop until either: probe passes / is skipped, or the user opts to
    // continue without a working credential. Each iteration re-captures
    // the credential so a wrong key is easy to correct.
    let probe_result = loop {
        capture_credentials(r, w, vault, &entry, &account).await?;
        let result = run_probe(w, probe, &entry, vault, &account).await?;
        if result.outcome.is_passing() {
            break result;
        }
        let retry = match &result.outcome {
            ProbeOutcome::AuthFailed { .. } => {
                yes_no(r, w, "Retry with a different credential? [Y/n]: ", true)?
            }
            ProbeOutcome::Unreachable { .. } => {
                // Bad network or wrong base_url — retrying the credential
                // is unlikely to help, so default to "no".
                yes_no(r, w, "Re-enter credential anyway? [y/N]: ", false)?
            }
            _ => false,
        };
        if !retry {
            writeln!(
                w,
                "  Continuing without a verified credential. Use \
                 `origin keyring add {} {account} <secret>` or \
                 `origin keyring login {}` to fix later.",
                entry.id, entry.id,
            )?;
            break result;
        }
    };

    let model = pick_model(r, w, &entry, &probe_result)?;
    Ok(RoleConfig {
        provider: entry.id.to_string(),
        account,
        model,
    })
}

/// Print the full catalog grouped by [`WireFormat`] and let the user pick
/// by index. The last option (`N+1`) is an "Other (enter catalog id)"
/// escape hatch that defers to [`Catalog::lookup`] — so even custom entries
/// added via `~/.origin/providers.toml` are reachable (the catalog the
/// caller passes in is the merged one).
fn pick_provider<R: BufRead, W: Write>(
    r: &mut R,
    w: &mut W,
    cat: &Catalog,
) -> Result<ProviderEntry> {
    let entries: Vec<&ProviderEntry> = cat.entries().iter().collect();
    writeln!(w, "Available providers:")?;
    let mut prev_wire: Option<WireFormat> = None;
    for (i, e) in entries.iter().enumerate() {
        if prev_wire != Some(e.wire) {
            writeln!(w, "  — {} —", wire_section_label(e.wire))?;
            prev_wire = Some(e.wire);
        }
        writeln!(
            w,
            "  {idx:>3}. {id:<22} {name:<38} ({auth})",
            idx = i + 1,
            id = e.id,
            name = e.display_name,
            auth = auth_label(&e.auth),
        )?;
    }
    let other_idx = entries.len() + 1;
    writeln!(w, "  {other_idx:>3}. Other (enter a catalog id)")?;

    loop {
        write!(w, "Choose [1-{other_idx}]: ")?;
        w.flush()?;
        let line = read_line(r)?;
        let trimmed = line.trim();
        let pick: usize = if let Ok(n) = trimmed.parse() {
            n
        } else {
            writeln!(w, "  (not a number; try again)")?;
            continue;
        };
        if pick >= 1 && pick <= entries.len() {
            return Ok(entries[pick - 1].clone());
        }
        if pick == other_idx {
            write!(w, "  Enter catalog id (e.g. 'deepseek'): ")?;
            w.flush()?;
            let id_line = read_line(r)?;
            let id = id_line.trim();
            if let Some(e) = cat.lookup(id) {
                return Ok(e.clone());
            }
            writeln!(w, "  unknown provider: {id}; try again")?;
            continue;
        }
        writeln!(w, "  (out of range; try again)")?;
    }
}

/// Capture the credential matching the provider's [`AuthScheme`]. Persists
/// directly to the vault; `None` / `Custom` do nothing beyond a notice.
#[allow(clippy::future_not_send)] // Generic over !Send readers/writers; only awaited from the CLI thread.
async fn capture_credentials<R: BufRead, W: Write>(
    r: &mut R,
    w: &mut W,
    vault: &KeyVault,
    entry: &ProviderEntry,
    account: &str,
) -> Result<()> {
    match &entry.auth {
        AuthScheme::None => {
            writeln!(w, "  ({} needs no auth — skipping credential step.)", entry.id)?;
            Ok(())
        }
        AuthScheme::ApiKey { header, .. } => {
            writeln!(
                w,
                "  This provider authenticates with an API key (sent as `{header}`)."
            )?;
            write!(w, "  Paste API key: ")?;
            w.flush()?;
            let line = read_line(r)?;
            let key = line.trim().to_string();
            if key.is_empty() {
                return Err(anyhow!("empty API key"));
            }
            vault
                .set(&entry.id, account, Secret::new(key))
                .await
                .map_err(|e| anyhow!("vault set: {e}"))?;
            writeln!(w, "  Saved API key for {}:{account}.", entry.id)?;
            Ok(())
        }
        AuthScheme::OAuth(_) => {
            writeln!(
                w,
                "  This provider uses OAuth. Starting the authorization flow…"
            )?;
            crate::keyring_login::run(&entry.id, account).await
        }
        AuthScheme::SigV4 { service } => {
            writeln!(
                w,
                "  This provider authenticates with AWS SigV4 (service: {service})."
            )?;
            write!(w, "  AWS access key id: ")?;
            w.flush()?;
            let id_line = read_line(r)?;
            let access = id_line.trim().to_string();
            write!(w, "  AWS secret access key: ")?;
            w.flush()?;
            let sec_line = read_line(r)?;
            let secret = sec_line.trim().to_string();
            if access.is_empty() || secret.is_empty() {
                return Err(anyhow!("empty SigV4 credentials"));
            }
            let blob = serde_json::json!({
                "access_key_id": access,
                "secret_access_key": secret,
            });
            vault
                .set(&entry.id, account, Secret::new(blob.to_string()))
                .await
                .map_err(|e| anyhow!("vault set: {e}"))?;
            writeln!(w, "  Saved SigV4 credentials for {}:{account}.", entry.id)?;
            Ok(())
        }
        AuthScheme::Custom => {
            writeln!(
                w,
                "  {} uses a custom auth scheme. Run `origin keyring add {} {} <secret>` \
                 after init to attach a credential.",
                entry.id, entry.id, account,
            )?;
            Ok(())
        }
    }
}

/// Run the probe and print a one-line summary. Returns the result; the
/// caller decides whether the retry loop fires.
#[allow(clippy::future_not_send)] // Generic over !Send writer; only awaited from the CLI thread.
async fn run_probe<W: Write>(
    w: &mut W,
    probe: &dyn ConnectivityProbe,
    entry: &ProviderEntry,
    vault: &KeyVault,
    account: &str,
) -> Result<ProbeResult> {
    write!(w, "  Testing credential against {}… ", entry.id)?;
    w.flush()?;
    let result = probe.probe(entry, vault, account).await;
    match &result.outcome {
        ProbeOutcome::Ok => {
            if result.models.is_empty() {
                writeln!(w, "OK (no model list returned)")?;
            } else {
                writeln!(w, "OK ({} models available)", result.models.len())?;
            }
        }
        ProbeOutcome::AuthFailed { status, detail } => {
            writeln!(w, "FAILED ({status})")?;
            if !detail.is_empty() {
                writeln!(w, "    {detail}")?;
            }
        }
        ProbeOutcome::Unreachable { detail } => {
            writeln!(w, "unreachable")?;
            writeln!(w, "    {detail}")?;
        }
        ProbeOutcome::Skipped { reason } => {
            writeln!(w, "skipped")?;
            writeln!(w, "    {reason}")?;
        }
    }
    Ok(result)
}

/// Show a model picker. If the probe returned a non-empty list, present a
/// numbered menu with the provider's catalog default highlighted as the
/// suggested choice (and accept-on-empty default). Otherwise prompt for
/// free text with the catalog default pre-filled.
fn pick_model<R: BufRead, W: Write>(
    r: &mut R,
    w: &mut W,
    entry: &ProviderEntry,
    probe_result: &ProbeResult,
) -> Result<String> {
    let default = entry.default_model.as_ref();

    if probe_result.models.is_empty() {
        write!(w, "Model [{default}]: ")?;
        w.flush()?;
        let line = read_line(r)?;
        let trimmed = line.trim();
        return Ok(if trimmed.is_empty() {
            default.to_string()
        } else {
            trimmed.to_string()
        });
    }

    // Sort the list with the catalog default first so a bare Enter selects
    // it. Other models follow in the order the provider returned them.
    let mut ordered: Vec<String> = Vec::with_capacity(probe_result.models.len());
    if probe_result.models.iter().any(|m| m == default) {
        ordered.push(default.to_string());
        for m in &probe_result.models {
            if m != default {
                ordered.push(m.clone());
            }
        }
    } else {
        ordered.extend(probe_result.models.iter().cloned());
    }

    writeln!(w, "Available models for {}:", entry.id)?;
    for (i, m) in ordered.iter().enumerate() {
        let marker = if m == default { "  (default)" } else { "" };
        writeln!(w, "  {idx:>3}. {m}{marker}", idx = i + 1)?;
    }
    let other_idx = ordered.len() + 1;
    writeln!(w, "  {other_idx:>3}. Other (enter a model id)")?;

    loop {
        write!(w, "Choose [1-{other_idx}] (Enter = 1): ")?;
        w.flush()?;
        let line = read_line(r)?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Ok(ordered[0].clone());
        }
        let pick: usize = if let Ok(n) = trimmed.parse() {
            n
        } else {
            writeln!(w, "  (not a number; try again)")?;
            continue;
        };
        if pick >= 1 && pick <= ordered.len() {
            return Ok(ordered[pick - 1].clone());
        }
        if pick == other_idx {
            write!(w, "  Enter model id: ")?;
            w.flush()?;
            let id_line = read_line(r)?;
            let id = id_line.trim();
            if !id.is_empty() {
                return Ok(id.to_string());
            }
            writeln!(w, "  empty; try again")?;
            continue;
        }
        writeln!(w, "  (out of range; try again)")?;
    }
}

const fn wire_section_label(w: WireFormat) -> &'static str {
    match w {
        WireFormat::Anthropic => "Anthropic native",
        WireFormat::Gemini => "Google Gemini native",
        WireFormat::Bedrock => "AWS Bedrock",
        WireFormat::Ollama => "Ollama (local)",
        WireFormat::GitHubCopilot => "GitHub Copilot",
        WireFormat::OpenAIChat => "OpenAI-compatible (chat/completions)",
    }
}

const fn auth_label(a: &AuthScheme) -> &'static str {
    match a {
        AuthScheme::None => "no auth",
        AuthScheme::ApiKey { .. } => "API key",
        AuthScheme::OAuth(_) => "OAuth",
        AuthScheme::SigV4 { .. } => "AWS SigV4",
        AuthScheme::Custom => "custom",
    }
}

fn read_line<R: BufRead>(r: &mut R) -> Result<String> {
    let mut buf = String::new();
    let n = r.read_line(&mut buf).map_err(|e| anyhow!("read stdin: {e}"))?;
    if n == 0 {
        return Err(anyhow!("unexpected EOF"));
    }
    Ok(buf)
}

fn yes_no<R: BufRead, W: Write>(
    r: &mut R,
    w: &mut W,
    prompt: &str,
    default_yes: bool,
) -> Result<bool> {
    loop {
        write!(w, "{prompt}")?;
        w.flush()?;
        let line = read_line(r)?;
        match line.trim() {
            "" => return Ok(default_yes),
            s if s.starts_with('y') || s.starts_with('Y') => return Ok(true),
            s if s.starts_with('n') || s.starts_with('N') => return Ok(false),
            _ => writeln!(w, "  (please answer y or n)")?,
        }
    }
}

#[cfg(test)]
#[allow(clippy::panic, clippy::map_unwrap_or, clippy::future_not_send)] // unit-test ergonomics
mod tests {
    use super::*;
    use crate::init_probe::MockProbe;

    /// 1-based index of the catalog entry with the given id, for scripted
    /// menu picks. Panics if the id isn't in the builtin catalog so changes
    /// to the catalog surface immediately.
    fn catalog_index(id: &str) -> usize {
        Catalog::builtin()
            .entries()
            .iter()
            .position(|e| e.id == id)
            .map(|p| p + 1)
            .unwrap_or_else(|| panic!("catalog id not found: {id}"))
    }

    #[tokio::test]
    async fn ollama_primary_no_auth_no_probe_models_picks_default() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg_path = dir.path().join("config.toml");

        let ollama_idx = catalog_index("ollama");
        // Script: pick ollama -> (no credential prompt) -> probe Ok no models
        // -> Enter accepts default model -> n -> n.
        let script = format!("{ollama_idx}\n\nn\nn\n");
        let input = std::io::Cursor::new(script.into_bytes());
        let mut output: Vec<u8> = Vec::new();

        let vault = KeyVault::in_memory();
        let probe = MockProbe::ok_no_models();
        run_with(input, &mut output, &vault, &cfg_path, &probe)
            .await
            .expect("run_with ok");

        let saved = config::load_from(&cfg_path).expect("load").expect("present");
        assert_eq!(saved.primary.provider, "ollama");
        assert_eq!(saved.primary.model, "llama3.2");
        assert!(saved.backup.is_none());
        assert!(saved.subagent.is_none());
    }

    #[tokio::test]
    async fn api_key_provider_persists_after_probe_ok_and_picks_from_list() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg_path = dir.path().join("config.toml");

        let anthropic_idx = catalog_index("anthropic");
        // Script: anthropic -> paste key -> probe Ok with 2 models ->
        // model menu, pick #2 (non-default) -> n -> n.
        let script = format!("{anthropic_idx}\nsk-ant-test\n2\nn\nn\n");
        let input = std::io::Cursor::new(script.into_bytes());
        let mut output: Vec<u8> = Vec::new();

        let vault = KeyVault::in_memory();
        let probe = MockProbe::ok_with_models(vec![
            "claude-sonnet-4-6".into(),
            "claude-opus-4-7".into(),
        ]);
        run_with(input, &mut output, &vault, &cfg_path, &probe)
            .await
            .expect("run_with ok");

        let saved = config::load_from(&cfg_path).expect("load").expect("present");
        assert_eq!(saved.primary.provider, "anthropic");
        // Default (claude-sonnet-4-6) sorts to index 1, so index 2 is opus.
        assert_eq!(saved.primary.model, "claude-opus-4-7");

        let stored = vault.get("anthropic", "default").await.expect("vault get");
        assert_eq!(stored.expose(), "sk-ant-test");
    }

    #[tokio::test]
    async fn auth_fail_retry_succeeds_on_second_attempt() {
        // The flow must offer a retry loop after AuthFailed. We can't drive
        // that loop with a single MockProbe (every call returns the same
        // outcome), so we use a small custom probe that flips after the
        // first call.
        use crate::init_probe::ProbeResult;
        use async_trait::async_trait;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        #[derive(Debug)]
        struct FlipProbe {
            call_count: Arc<AtomicUsize>,
        }
        #[async_trait]
        impl ConnectivityProbe for FlipProbe {
            async fn probe(
                &self,
                _entry: &ProviderEntry,
                _vault: &KeyVault,
                _account: &str,
            ) -> ProbeResult {
                let n = self.call_count.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    ProbeResult {
                        outcome: ProbeOutcome::AuthFailed {
                            status: 401,
                            detail: "unauthorized".into(),
                        },
                        models: Vec::new(),
                    }
                } else {
                    ProbeResult {
                        outcome: ProbeOutcome::Ok,
                        models: Vec::new(),
                    }
                }
            }
        }

        let dir = tempfile::tempdir().expect("tempdir");
        let cfg_path = dir.path().join("config.toml");
        let anthropic_idx = catalog_index("anthropic");

        // Script: anthropic -> bad key -> retry y -> good key ->
        // accept default model -> n -> n.
        let script = format!(
            "{anthropic_idx}\nbad-key\ny\ngood-key\n\nn\nn\n"
        );
        let input = std::io::Cursor::new(script.into_bytes());
        let mut output: Vec<u8> = Vec::new();

        let vault = KeyVault::in_memory();
        let calls = Arc::new(AtomicUsize::new(0));
        let probe = FlipProbe {
            call_count: Arc::clone(&calls),
        };
        run_with(input, &mut output, &vault, &cfg_path, &probe)
            .await
            .expect("run_with ok");

        assert_eq!(calls.load(Ordering::SeqCst), 2, "probe called twice");
        let saved = config::load_from(&cfg_path).expect("load").expect("present");
        assert_eq!(saved.primary.provider, "anthropic");
        // Final stored key should be "good-key", not "bad-key".
        let stored = vault.get("anthropic", "default").await.expect("vault get");
        assert_eq!(stored.expose(), "good-key");

        let out = String::from_utf8(output).expect("utf8");
        assert!(out.contains("FAILED (401)"));
    }

    #[tokio::test]
    async fn other_picker_resolves_custom_catalog_id() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg_path = dir.path().join("config.toml");

        let cat = Catalog::builtin();
        let other_idx = cat.entries().len() + 1;
        // Script: choose "Other" -> type "deepseek" -> paste key ->
        // probe ok -> accept default -> n -> n.
        let script = format!(
            "{other_idx}\ndeepseek\nsk-deepseek-test\n\nn\nn\n"
        );
        let input = std::io::Cursor::new(script.into_bytes());
        let mut output: Vec<u8> = Vec::new();

        let vault = KeyVault::in_memory();
        let probe = MockProbe::ok_no_models();
        run_with(input, &mut output, &vault, &cfg_path, &probe)
            .await
            .expect("run_with ok");

        let saved = config::load_from(&cfg_path).expect("load").expect("present");
        assert_eq!(saved.primary.provider, "deepseek");
    }

    #[tokio::test]
    async fn all_three_roles_persist() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg_path = dir.path().join("config.toml");

        let ollama_idx = catalog_index("ollama");
        // Primary, backup, subagent all = ollama (no auth). Probe Ok with
        // no models for each → bare Enter accepts default model.
        let script = format!(
            "{ollama_idx}\n\
             \n\
             y\n\
             {ollama_idx}\n\
             \n\
             y\n\
             {ollama_idx}\n\
             \n"
        );
        let input = std::io::Cursor::new(script.into_bytes());
        let mut output: Vec<u8> = Vec::new();

        let vault = KeyVault::in_memory();
        let probe = MockProbe::ok_no_models();
        run_with(input, &mut output, &vault, &cfg_path, &probe)
            .await
            .expect("run_with ok");

        let saved = config::load_from(&cfg_path).expect("load").expect("present");
        assert!(saved.backup.is_some());
        assert!(saved.subagent.is_some());
    }

    #[test]
    fn yes_no_default_no_on_empty_line() {
        let input = std::io::Cursor::new(b"\n".as_slice());
        let mut output: Vec<u8> = Vec::new();
        let ans = yes_no(&mut std::io::BufReader::new(input), &mut output, "x? ", false).expect("ok");
        assert!(!ans);
    }

    #[test]
    fn yes_no_default_yes_on_empty_line() {
        let input = std::io::Cursor::new(b"\n".as_slice());
        let mut output: Vec<u8> = Vec::new();
        let ans = yes_no(&mut std::io::BufReader::new(input), &mut output, "x? ", true).expect("ok");
        assert!(ans);
    }

    #[test]
    fn yes_no_re_asks_on_garbage() {
        let input = std::io::Cursor::new(b"maybe\ny\n".as_slice());
        let mut output: Vec<u8> = Vec::new();
        let ans = yes_no(&mut std::io::BufReader::new(input), &mut output, "x? ", false).expect("ok");
        assert!(ans);
        let out = String::from_utf8(output).expect("utf8");
        assert!(out.contains("please answer y or n"));
    }
}
