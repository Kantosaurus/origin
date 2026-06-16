// SPDX-License-Identifier: Apache-2.0
//! Onboarding orchestration: brand grouping, row builders, and the per-role
//! interactive driver.
//!
//! The pure helpers ([`group_by_brand`], [`auth_label`], [`provider_rows`],
//! [`model_rows`]) are unit-tested without a terminal. [`configure_role_interactive`]
//! and [`run_interactive`] wire those rows to the crossterm picker
//! ([`crate::onboarding::screen`]) and reuse [`crate::init`]'s credential
//! capture, `/models` probe, and config persistence verbatim — only the
//! interaction differs from the line-based wizard.

use anyhow::{anyhow, Result};
use origin_daemon::model_window::model_context_window;
use origin_keyvault::{KeyVault, Secret};
use origin_provider::catalog::{AuthScheme, Catalog, ProviderEntry};

use crate::config::{self, OriginConfig, RoleConfig, SCHEMA_VERSION};
use crate::init::{self, Role};
use crate::init_probe::{ConnectivityProbe, ProbeOutcome};
use crate::tui::tokens::Tokens;

use super::picker::{format_ctx, PickResult, PickerState, Row};
use super::screen::{run_picker, run_text_field, FieldOutcome};

/// Sentinel `Row::value` for the "type your own model id" escape hatch appended
/// to the model picker. Chosen so no real model id can collide with it.
const CUSTOM_MODEL_SENTINEL: &str = "\u{0}__origin_custom_model__";

/// A provider *brand*: one or more catalog entries that differ only by auth.
///
/// e.g. `anthropic` + `anthropic-oauth`. The brand `label` is the first entry's
/// display name; `entries` preserves catalog order.
#[derive(Debug, Clone)]
pub struct Brand {
    /// The brand key — the shared key produced by [`brand_key`].
    pub key: String,
    /// Human label (the brand's first entry's `display_name`).
    pub label: String,
    /// The concrete catalog entries under this brand, in catalog order.
    pub entries: Vec<ProviderEntry>,
}

/// Map a catalog id to its provider *brand* key, collapsing dual-auth providers
/// (an API-key entry + an OAuth entry under the same vendor) onto one key.
///
/// The catalog's dual-auth pairs do NOT all share a `-oauth` suffix — only
/// `anthropic`/`anthropic-oauth` do. `openai`+`openai-codex` (`ChatGPT` OAuth) and
/// `google`(Gemini API key)+`gemini-oauth` use different id stems, so a plain
/// `trim_end_matches("-oauth")` would never collapse them and their "OAuth vs
/// API key" step would never appear. This table maps each known pair to a shared
/// brand; everything else falls back to the suffix strip.
///
/// new dual-auth providers must be added here
#[must_use]
pub fn brand_key(id: &str) -> &str {
    match id {
        "anthropic" | "anthropic-oauth" => "anthropic",
        "openai" | "openai-codex" => "openai",
        "google" | "gemini" | "gemini-oauth" => "google",
        other => other.trim_end_matches("-oauth"),
    }
}

/// Group catalog entries into brands keyed by [`brand_key`].
///
/// Catalog order is preserved (both for the brands and the entries within each
/// brand). A brand's `label` comes from its first-seen entry's `display_name`.
/// This is what collapses each vendor's API-key and OAuth entries into a single
/// brand with two entries (the "OAuth vs API key" choice).
#[must_use]
pub fn group_by_brand(entries: &[ProviderEntry]) -> Vec<Brand> {
    let mut brands: Vec<Brand> = Vec::new();
    for entry in entries {
        let key = brand_key(&entry.id).to_string();
        if let Some(brand) = brands.iter_mut().find(|b| b.key == key) {
            brand.entries.push(entry.clone());
        } else {
            brands.push(Brand {
                key,
                label: entry.display_name.to_string(),
                entries: vec![entry.clone()],
            });
        }
    }
    brands
}

/// Short, stable label for an auth scheme (used in provider/auth rows).
#[must_use]
pub const fn auth_label(scheme: &AuthScheme) -> &'static str {
    match scheme {
        AuthScheme::OAuth(_) => "OAuth",
        AuthScheme::ApiKey { .. } => "API key",
        AuthScheme::SigV4 { .. } => "AWS SigV4",
        AuthScheme::None => "none",
        AuthScheme::Custom => "custom",
    }
}

/// Build the provider-step rows: one per brand, `value` = brand key, `label` =
/// brand label, `note` = the brand's auth options joined (e.g. `OAuth / API key`).
#[must_use]
pub fn provider_rows(brands: &[Brand]) -> Vec<Row> {
    brands
        .iter()
        .map(|b| {
            let note = b
                .entries
                .iter()
                .map(|e| auth_label(&e.auth))
                .collect::<Vec<_>>()
                .join(" / ");
            Row::with_note(b.key.clone(), b.label.clone(), note)
        })
        .collect()
}

/// Build the auth-step rows for a brand with more than one scheme: `value` =
/// the concrete catalog entry id, `label` = the auth scheme label.
#[must_use]
pub fn auth_rows(brand: &Brand) -> Vec<Row> {
    brand
        .entries
        .iter()
        .map(|e| Row::new(e.id.to_string(), auth_label(&e.auth)))
        .collect()
}

/// Build the model-step rows: default-first, each annotated with its window.
///
/// Ordering shares [`init::order_models`]; the note is the shared resolver's
/// context window (e.g. `claude-opus-4-8` ⇒ `1M ctx`). A trailing escape-hatch
/// row lets the user type an id the probe did not list.
#[must_use]
pub fn model_rows(models: &[String], default: &str) -> Vec<Row> {
    let ordered = init::order_models(models, default);
    let mut rows: Vec<Row> = ordered
        .iter()
        .map(|m| {
            let note = format!("{} ctx", format_ctx(model_context_window(m)));
            Row::with_note(m.clone(), m.clone(), note)
        })
        .collect();
    rows.push(Row::new(CUSTOM_MODEL_SENTINEL, "Type your own model id\u{2026}"));
    rows
}

/// Interactive analogue of [`init::configure_role`].
///
/// Steps: provider → (auth) → credential capture → probe (with retry) → model.
/// Reuses the same vault, probe, and credential helpers; only the prompts are a
/// picker instead of a numbered menu. `esc` at a step steps back one stage.
///
/// # Errors
/// Propagates credential-capture, probe, and vault failures, and returns an
/// error if the user backs all the way out of the (required) provider step.
pub async fn configure_role_interactive(
    cat: &Catalog,
    vault: &KeyVault,
    probe: &dyn ConnectivityProbe,
    role: Role,
    tok: &Tokens,
) -> Result<RoleConfig> {
    let brands = group_by_brand(cat.entries());
    let account = "default".to_string();

    loop {
        // ---- Step 1: provider brand ----
        let prov_rows = provider_rows(&brands);
        let brand_key = match run_picker(
            "origin",
            role.label(),
            &format!("Choose your {} provider", role.label()),
            PickerState::new(prov_rows),
            tok,
        )? {
            PickResult::Selected(key) => key,
            PickResult::Back => {
                // The provider step is the first step; backing out of it aborts
                // this role. For a required role this is an error the caller
                // surfaces; the wizard never silently invents a provider.
                return Err(anyhow!("onboarding cancelled at the provider step"));
            }
        };
        let Some(brand) = brands.iter().find(|b| b.key == brand_key) else {
            // Unreachable: the row value came from this same brand list.
            continue;
        };

        // ---- Step 2: auth type (only when the brand has >1 scheme) ----
        let entry: ProviderEntry = if brand.entries.len() > 1 {
            let breadcrumb = format!("{} \u{00b7} {}", role.label(), brand.label);
            match run_picker(
                "origin",
                &breadcrumb,
                "Choose how to sign in",
                PickerState::new(auth_rows(brand)),
                tok,
            )? {
                PickResult::Selected(entry_id) => match brand.entries.iter().find(|e| e.id == entry_id) {
                    Some(e) => e.clone(),
                    None => continue,
                },
                // esc ⇒ back to the provider step.
                PickResult::Back => continue,
            }
        } else {
            brand.entries[0].clone()
        };

        // ---- Step 3: credential capture + probe (retry on failure) ----
        // Reuses init::run_probe verbatim. The ApiKey paste and SigV4 entry use
        // masked/plain in-screen fields; OAuth (browser flow) and None/Custom
        // delegate to the exact same code the line-based wizard runs. An `esc`
        // on a credential field steps BACK to the auth/provider step (signalled
        // by `Ok(None)`), not an error that aborts onboarding.
        let probe_result = loop {
            match capture_credentials_interactive(vault, &entry, &account, tok).await? {
                Some(()) => {}
                // Cancelled the credential field ⇒ restart this role from the
                // provider step (mirrors the model-step `esc` behaviour).
                None => break None,
            }
            // run_probe writes a one-line summary; route it to a throwaway sink
            // so the raw-mode frame is not corrupted by interleaved prose.
            let mut sink: Vec<u8> = Vec::new();
            let result = init::run_probe(&mut sink, probe, &entry, vault, &account).await?;
            if result.outcome.is_passing() {
                break Some(result);
            }
            let retry = match &result.outcome {
                ProbeOutcome::AuthFailed { .. } => confirm("Credential rejected. Retry?", true, tok)?,
                ProbeOutcome::Unreachable { .. } => {
                    confirm("Provider unreachable. Re-enter credential anyway?", false, tok)?
                }
                _ => false,
            };
            if !retry {
                break Some(result);
            }
        };
        // The credential field was cancelled (`esc`) ⇒ restart this role from
        // the provider step rather than aborting onboarding.
        let Some(probe_result) = probe_result else {
            continue;
        };

        // ---- Step 4: model ----
        let default = entry.default_model.as_ref();
        let model = if probe_result.models.is_empty() {
            // No live list — go straight to the free-text field, pre-filled with
            // the catalog default by accepting an empty entry. `esc` (Cancelled)
            // steps back; a bare `⏎` (empty Submitted) accepts the default.
            match run_text_field(&format!("Model id [{default}]:"), false)? {
                FieldOutcome::Submitted(m) if m.is_empty() => default.to_string(),
                FieldOutcome::Submitted(m) => m,
                FieldOutcome::Cancelled => continue,
            }
        } else {
            let breadcrumb = format!("{} \u{00b7} {}", role.label(), entry.id);
            let rows = model_rows(&probe_result.models, default);
            match run_picker(
                "origin",
                &breadcrumb,
                "Choose a model",
                PickerState::new(rows),
                tok,
            )? {
                PickResult::Selected(v) if v == CUSTOM_MODEL_SENTINEL => {
                    match run_text_field("Model id:", false)? {
                        FieldOutcome::Submitted(m) if !m.is_empty() => m,
                        // Empty submit or `esc` ⇒ restart this role.
                        _ => continue,
                    }
                }
                PickResult::Selected(v) => v,
                // esc on the model step ⇒ restart this role from the provider.
                PickResult::Back => continue,
            }
        };

        return Ok(RoleConfig {
            provider: entry.id.to_string(),
            account,
            model,
        });
    }
}

/// Capture the credential for `entry` using the interactive surface.
///
/// Returns `Ok(Some(()))` when a credential was captured (or the scheme needs
/// none), and `Ok(None)` when the user pressed `esc` on a credential field —
/// the caller treats that as "step back", NOT an error that aborts onboarding.
///
/// `ApiKey` and `SigV4` are prompted with in-screen [`run_text_field`]s (so an
/// `esc` is a real cancel, not a stranded blank reader); OAuth (browser flow)
/// and `None`/`Custom` (no-ops) delegate to [`init::capture_credentials`]
/// unchanged — those paths read nothing from the reader.
async fn capture_credentials_interactive(
    vault: &KeyVault,
    entry: &ProviderEntry,
    account: &str,
    _tok: &Tokens,
) -> Result<Option<()>> {
    match &entry.auth {
        AuthScheme::ApiKey { .. } => {
            // Masked in-screen paste, then persist via the same vault path the
            // wizard uses. `esc` (Cancelled) steps back; an empty submit is the
            // existing hard error (mirrors the wizard's `empty API key`), so the
            // retry loop in the caller fires.
            let key = match run_text_field(&format!("Paste API key for {}:", entry.id), true)? {
                FieldOutcome::Cancelled => return Ok(None),
                FieldOutcome::Submitted(k) if k.is_empty() => return Err(anyhow!("empty API key")),
                FieldOutcome::Submitted(k) => k,
            };
            vault
                .set(&entry.id, account, Secret::new(key))
                .await
                .map_err(|e| anyhow!("vault set: {e}"))?;
            Ok(Some(()))
        }
        AuthScheme::SigV4 { .. } => {
            // Two in-screen fields (access key id unmasked, secret masked), then
            // persist the SAME JSON blob shape init.rs writes. Do NOT feed a
            // blank reader to init::capture_credentials' SigV4 arm — it reads two
            // lines and would fail with `empty SigV4 credentials`, aborting init.
            let access = match run_text_field("AWS access key id:", false)? {
                FieldOutcome::Cancelled => return Ok(None),
                FieldOutcome::Submitted(a) if a.is_empty() => return Err(anyhow!("empty SigV4 credentials")),
                FieldOutcome::Submitted(a) => a,
            };
            let secret = match run_text_field("AWS secret access key:", true)? {
                FieldOutcome::Cancelled => return Ok(None),
                FieldOutcome::Submitted(s) if s.is_empty() => return Err(anyhow!("empty SigV4 credentials")),
                FieldOutcome::Submitted(s) => s,
            };
            persist_sigv4(vault, &entry.id, account, &access, &secret).await?;
            Ok(Some(()))
        }
        // OAuth (browser flow), None, Custom: reuse the wizard's exact capture
        // against a throwaway reader/writer (these paths prompt via their own
        // channels — the OAuth browser flow — or are no-ops for None/Custom, so
        // they read nothing from the reader).
        AuthScheme::OAuth(_) | AuthScheme::None | AuthScheme::Custom => {
            let mut empty: &[u8] = b"";
            let mut sink: Vec<u8> = Vec::new();
            init::capture_credentials(&mut empty, &mut sink, vault, entry, account).await?;
            Ok(Some(()))
        }
    }
}

/// Persist a `SigV4` credential to the vault using the exact JSON blob shape
/// [`init::capture_credentials`] writes (`{"access_key_id", "secret_access_key"}`
/// at `provider:account`). Shared by the interactive `SigV4` branch and the flow
/// test so the two never drift apart.
async fn persist_sigv4(
    vault: &KeyVault,
    provider: &str,
    account: &str,
    access: &str,
    secret: &str,
) -> Result<()> {
    let blob = serde_json::json!({
        "access_key_id": access,
        "secret_access_key": secret,
    });
    vault
        .set(provider, account, Secret::new(blob.to_string()))
        .await
        .map_err(|e| anyhow!("vault set: {e}"))
}

/// A yes/no confirmation rendered as a two-row picker. `default_yes` selects
/// `Yes` as the initial cursor; `esc` returns `default_yes`.
fn confirm(question: &str, default_yes: bool, tok: &Tokens) -> Result<bool> {
    let mut state = PickerState::new(vec![Row::new("yes", "Yes"), Row::new("no", "No")]);
    state.cursor = usize::from(!default_yes);
    let result = run_picker("origin", "", question, state, tok)?;
    Ok(match result {
        PickResult::Selected(v) => v == "yes",
        PickResult::Back => default_yes,
    })
}

/// Run the full interactive onboarding.
///
/// Primary role, then optional backup and subagent roles (each gated by a
/// yes/no confirm), then persist to `~/.origin/config.toml` via
/// [`config::save_to`] (unchanged).
///
/// # Errors
/// Propagates per-role configuration failures and config persistence failures.
pub async fn run_interactive(
    vault: &KeyVault,
    cfg_path: &std::path::Path,
    probe: &dyn ConnectivityProbe,
    tok: &Tokens,
) -> Result<()> {
    let cat = Catalog::builtin();

    let primary = configure_role_interactive(&cat, vault, probe, Role::Primary, tok).await?;

    let backup = if confirm("Configure a backup provider/model?", false, tok)? {
        Some(configure_role_interactive(&cat, vault, probe, Role::Backup, tok).await?)
    } else {
        None
    };

    let subagent = if confirm(
        "Configure a separate provider for subagents and swarm?",
        false,
        tok,
    )? {
        Some(configure_role_interactive(&cat, vault, probe, Role::Subagent, tok).await?)
    } else {
        None
    };

    let cfg = OriginConfig {
        schema_version: SCHEMA_VERSION,
        primary,
        backup,
        subagent,
        aliases: std::collections::BTreeMap::new(),
    };
    config::save_to(cfg_path, &cfg).map_err(|e| anyhow!("save config: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_by_brand_collapses_anthropic_pair() {
        let cat = Catalog::builtin();
        let brands = group_by_brand(cat.entries());
        let anthropic = brands
            .iter()
            .find(|b| b.key == "anthropic")
            .expect("anthropic brand present");
        assert_eq!(
            anthropic.entries.len(),
            2,
            "anthropic + anthropic-oauth collapse into one brand with two entries"
        );
        // The two entries carry the two schemes (one ApiKey, one OAuth).
        let has_apikey = anthropic
            .entries
            .iter()
            .any(|e| matches!(e.auth, AuthScheme::ApiKey { .. }));
        let has_oauth = anthropic
            .entries
            .iter()
            .any(|e| matches!(e.auth, AuthScheme::OAuth(_)));
        assert!(has_apikey && has_oauth, "brand holds both auth schemes");
        // The label comes from the first entry's display name.
        assert_eq!(anthropic.label, "Anthropic (API key)");
    }

    #[test]
    fn single_scheme_provider_is_one_entry_brand() {
        let cat = Catalog::builtin();
        let brands = group_by_brand(cat.entries());
        let ollama = brands
            .iter()
            .find(|b| b.key == "ollama")
            .expect("ollama brand present");
        assert_eq!(ollama.entries.len(), 1, "single-scheme provider ⇒ one entry");
    }

    #[test]
    fn brand_keys_strip_oauth_suffix_and_preserve_order() {
        let cat = Catalog::builtin();
        let brands = group_by_brand(cat.entries());
        // No brand key ends in -oauth.
        assert!(brands.iter().all(|b| !b.key.ends_with("-oauth")));
        // Catalog order preserved: anthropic appears before openai.
        let pos = |k: &str| brands.iter().position(|b| b.key == k);
        assert!(pos("anthropic") < pos("openai"), "catalog order preserved");
    }

    #[test]
    fn auth_label_maps_each_scheme() {
        use std::borrow::Cow;
        assert_eq!(
            auth_label(&AuthScheme::ApiKey {
                header: Cow::Borrowed("x"),
                prefix: Cow::Borrowed(""),
            }),
            "API key"
        );
        assert_eq!(auth_label(&AuthScheme::None), "none");
        assert_eq!(
            auth_label(&AuthScheme::SigV4 {
                service: Cow::Borrowed("bedrock")
            }),
            "AWS SigV4"
        );
    }

    #[test]
    fn provider_rows_join_auth_options() {
        let cat = Catalog::builtin();
        let brands = group_by_brand(cat.entries());
        let rows = provider_rows(&brands);
        let anthropic = rows
            .iter()
            .find(|r| r.value == "anthropic")
            .expect("anthropic row");
        // ApiKey entry is first, OAuth second ⇒ "API key / OAuth".
        assert_eq!(anthropic.note.as_deref(), Some("API key / OAuth"));
    }

    #[test]
    fn model_rows_annotate_context_and_default_first() {
        let models = vec!["claude-sonnet-4-6".to_string(), "claude-opus-4-8".to_string()];
        let rows = model_rows(&models, "claude-opus-4-8");
        // Default sorts to the top.
        assert_eq!(rows[0].value, "claude-opus-4-8");
        assert_eq!(
            rows[0].note.as_deref(),
            Some("1M ctx"),
            "opus-4-8 annotated 1M ctx"
        );
        // Sonnet keeps its 200K window.
        let sonnet = rows
            .iter()
            .find(|r| r.value == "claude-sonnet-4-6")
            .expect("sonnet row");
        assert_eq!(sonnet.note.as_deref(), Some("200K ctx"));
        // The trailing escape-hatch row is present.
        assert!(
            rows.last().is_some_and(|r| r.value == CUSTOM_MODEL_SENTINEL),
            "type-your-own escape hatch appended"
        );
    }

    #[test]
    fn auth_rows_use_entry_ids_as_values() {
        let cat = Catalog::builtin();
        let brands = group_by_brand(cat.entries());
        let anthropic = brands.iter().find(|b| b.key == "anthropic").expect("brand");
        let rows = auth_rows(anthropic);
        assert_eq!(rows.len(), 2);
        assert!(rows
            .iter()
            .any(|r| r.value == "anthropic" && r.label == "API key"));
        assert!(rows
            .iter()
            .any(|r| r.value == "anthropic-oauth" && r.label == "OAuth"));
    }

    #[test]
    fn brand_key_collapses_all_dual_auth_pairs() {
        // The three known dual-auth pairs each map to one shared brand key.
        assert_eq!(brand_key("anthropic"), "anthropic");
        assert_eq!(brand_key("anthropic-oauth"), "anthropic");
        assert_eq!(brand_key("openai"), "openai");
        assert_eq!(brand_key("openai-codex"), "openai");
        assert_eq!(brand_key("google"), "google");
        assert_eq!(brand_key("gemini-oauth"), "google");
        // A single-auth provider keeps its own id; a stray `-oauth` still strips.
        assert_eq!(brand_key("ollama"), "ollama");
        assert_eq!(brand_key("something-oauth"), "something");
    }

    #[test]
    fn group_by_brand_collapses_openai_pair() {
        // Mirrors the anthropic test: openai (API key) + openai-codex (OAuth)
        // collapse into ONE brand with two entries so the auth step appears.
        let cat = Catalog::builtin();
        let brands = group_by_brand(cat.entries());
        let openai = brands
            .iter()
            .find(|b| b.key == "openai")
            .expect("openai brand present");
        assert_eq!(
            openai.entries.len(),
            2,
            "openai + openai-codex collapse into one brand with two entries"
        );
        let ids: Vec<&str> = openai.entries.iter().map(|e| e.id.as_ref()).collect();
        assert!(ids.contains(&"openai") && ids.contains(&"openai-codex"));
        let has_apikey = openai
            .entries
            .iter()
            .any(|e| matches!(e.auth, AuthScheme::ApiKey { .. }));
        let has_oauth = openai
            .entries
            .iter()
            .any(|e| matches!(e.auth, AuthScheme::OAuth(_)));
        assert!(has_apikey && has_oauth, "brand holds both auth schemes");
    }

    #[test]
    fn group_by_brand_collapses_google_gemini_pair() {
        // google (Gemini API key) + gemini-oauth (OAuth) collapse to ONE brand.
        let cat = Catalog::builtin();
        let brands = group_by_brand(cat.entries());
        let google = brands
            .iter()
            .find(|b| b.key == "google")
            .expect("google brand present");
        assert_eq!(
            google.entries.len(),
            2,
            "google + gemini-oauth collapse into one brand with two entries"
        );
        let ids: Vec<&str> = google.entries.iter().map(|e| e.id.as_ref()).collect();
        assert!(ids.contains(&"google") && ids.contains(&"gemini-oauth"));
        let has_apikey = google
            .entries
            .iter()
            .any(|e| matches!(e.auth, AuthScheme::ApiKey { .. }));
        let has_oauth = google
            .entries
            .iter()
            .any(|e| matches!(e.auth, AuthScheme::OAuth(_)));
        assert!(has_apikey && has_oauth, "brand holds both auth schemes");
    }

    /// Stub probe returning a fixed `Skipped` outcome with no model list — the
    /// outcome a `SigV4` (Bedrock) credential gets (live probing is unimplemented),
    /// which is `is_passing()` so the flow proceeds to the model step.
    #[derive(Debug)]
    struct SkippedProbe;

    #[async_trait::async_trait]
    impl ConnectivityProbe for SkippedProbe {
        async fn probe(
            &self,
            _entry: &ProviderEntry,
            _vault: &KeyVault,
            _account: &str,
        ) -> crate::init_probe::ProbeResult {
            crate::init_probe::ProbeResult {
                outcome: ProbeOutcome::Skipped {
                    reason: "SigV4 probing not implemented".into(),
                },
                models: Vec::new(),
            }
        }
    }

    #[tokio::test]
    async fn sigv4_bedrock_captures_and_reaches_roleconfig() {
        // Drives the SigV4 path the picker now owns: persist the AWS credential
        // via the SAME helper the interactive branch uses, run the (Skipped)
        // probe, and assemble the RoleConfig — proving the bedrock/SigV4 entry
        // produces a usable config instead of aborting onboarding.
        let cat = Catalog::builtin();
        let bedrock = cat
            .entries()
            .iter()
            .find(|e| e.id == "bedrock")
            .expect("bedrock entry present")
            .clone();
        assert!(
            matches!(bedrock.auth, AuthScheme::SigV4 { .. }),
            "bedrock ships a SigV4 scheme"
        );

        let vault = KeyVault::in_memory();
        let account = "default";

        // Step 3 (credential capture): persist the AWS keys via the shared
        // helper — the same blob the interactive SigV4 field writes.
        persist_sigv4(&vault, &bedrock.id, account, "AKIAEXAMPLE", "wsecret/EXAMPLE")
            .await
            .expect("persist sigv4");

        // The vault holds the exact JSON blob init.rs uses.
        let stored = vault.get(&bedrock.id, account).await.expect("vault get");
        let parsed: serde_json::Value = serde_json::from_str(stored.expose()).expect("blob is valid json");
        assert_eq!(parsed["access_key_id"], "AKIAEXAMPLE");
        assert_eq!(parsed["secret_access_key"], "wsecret/EXAMPLE");

        // Step 3 (probe): SigV4 probing is Skipped, which is passing, so the
        // flow proceeds rather than looping on a credential rejection.
        let probe = SkippedProbe;
        let mut sink: Vec<u8> = Vec::new();
        let result = init::run_probe(&mut sink, &probe, &bedrock, &vault, account)
            .await
            .expect("probe runs");
        assert!(
            result.outcome.is_passing(),
            "Skipped SigV4 probe lets the flow continue"
        );

        // Step 4 (model): no live list ⇒ catalog default. Assemble the config
        // exactly as configure_role_interactive does on its return path.
        let model = if result.models.is_empty() {
            bedrock.default_model.to_string()
        } else {
            result.models[0].clone()
        };
        let cfg = RoleConfig {
            provider: bedrock.id.to_string(),
            account: account.to_string(),
            model,
        };
        assert_eq!(cfg.provider, "bedrock");
        assert_eq!(cfg.model, bedrock.default_model.as_ref());
    }

    #[tokio::test]
    async fn capture_credentials_interactive_persists_sigv4_blob() {
        // Lower-level check on the persistence helper the SigV4 branch calls:
        // the blob shape and vault location match init.rs's SigV4 arm exactly.
        let vault = KeyVault::in_memory();
        persist_sigv4(&vault, "bedrock", "default", "id-123", "sec-456")
            .await
            .expect("persist");
        let stored = vault.get("bedrock", "default").await.expect("get");
        assert_eq!(
            stored.expose(),
            r#"{"access_key_id":"id-123","secret_access_key":"sec-456"}"#,
        );
    }
}
