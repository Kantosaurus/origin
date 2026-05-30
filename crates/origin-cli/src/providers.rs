// SPDX-License-Identifier: Apache-2.0
//! `origin providers …` subcommand handlers.

use origin_modeldiscovery::{parse_models_response, ModelCache};
use origin_provider::catalog::{AuthScheme, Catalog};

/// Build the catalog the CLI should display, mirroring the daemon's merge of
/// `~/.origin/providers.toml` on top of the builtin entries.
///
/// Custom-providers IO and parse errors are surfaced on stderr but do not
/// abort the listing — the builtin catalog is always shown.
///
/// The home directory is normally resolved via [`dirs::home_dir`]; the
/// `ORIGIN_HOME` env var overrides it so integration tests can point the
/// merge at a scratch directory without racing `HOME` / `USERPROFILE` across
/// threads.
fn merged_catalog() -> Catalog {
    let mut cat = Catalog::builtin();
    let home = std::env::var_os("ORIGIN_HOME")
        .map(std::path::PathBuf::from)
        .or_else(dirs::home_dir);
    if let Some(home) = home {
        let path = home.join(".origin").join("providers.toml");
        match origin_provider::custom::load(&path) {
            Ok(custom) => {
                if !custom.is_empty() {
                    if let Err(e) = cat.merge_custom(custom) {
                        eprintln!("warning: providers.toml merge failed: {e}");
                    }
                }
            }
            Err(e) => {
                eprintln!("warning: failed to load {}: {e}", path.display());
            }
        }
    }
    cat
}

/// Print every catalog entry (builtin + custom) as a fixed-width table.
pub fn ls() {
    let cat = merged_catalog();
    println!("{:<20} {:<35} {:<14} AUTH", "ID", "DISPLAY NAME", "WIRE");
    for e in cat.entries() {
        let wire = format!("{:?}", e.wire);
        let auth = match &e.auth {
            AuthScheme::None => "none",
            AuthScheme::ApiKey { .. } => "api-key",
            AuthScheme::OAuth(_) => "oauth",
            AuthScheme::SigV4 { .. } => "sigv4",
            AuthScheme::Custom => "custom",
        };
        println!("{:<20} {:<35} {:<14} {auth}", e.id, e.display_name, wire);
    }
}

/// Best-effort refresh of the runtime model catalog from a custom provider.
///
/// Resolves a custom provider (explicit `provider`, else the first in
/// `~/.origin/providers.toml`), performs a single blocking `GET
/// {base_url}/models` with the provider's API key, parses the listing, and
/// persists it into the on-disk [`ModelCache`] at
/// `~/.origin/models-cache.json`. When no usable source is configured — or the
/// fetch/parse fails — it prints a clear message and leaves the cache as-is.
/// This never returns an error and never changes default `ls`/`describe`
/// behaviour.
pub fn refresh(provider: Option<&str>) {
    let Some(home) = std::env::var_os("ORIGIN_HOME")
        .map(std::path::PathBuf::from)
        .or_else(dirs::home_dir)
    else {
        println!("no refresh source configured: cannot resolve the home directory");
        return;
    };
    let toml_path = home.join(".origin").join("providers.toml");
    let custom = match origin_provider::custom::load(&toml_path) {
        Ok(c) => c,
        Err(e) => {
            println!("no refresh source configured: failed to load {}: {e}", toml_path.display());
            return;
        }
    };

    // Pick the target custom provider: explicit flag (matched by id), else the
    // first entry. `custom` is a `Vec<ProviderEntry>` (the loader does not retain
    // the raw `api_key_env`), so the API key is resolved below from the
    // conventional `<UPPER_ID>_API_KEY` environment variable.
    let entry = match provider {
        Some(name) => custom.into_iter().find(|p| p.id == name),
        None => custom.into_iter().next(),
    };
    let Some(pc) = entry else {
        println!(
            "no refresh source configured: define a custom provider (base_url) in {}",
            toml_path.display()
        );
        return;
    };
    let name = pc.id.to_string();

    // A refresh source needs both a base URL and a resolvable API key. The key
    // is read from `<UPPER_ID>_API_KEY` (e.g. provider `acme` → `ACME_API_KEY`).
    let key_env = format!("{}_API_KEY", name.to_ascii_uppercase().replace('-', "_"));
    let key = std::env::var(&key_env).ok().filter(|k| !k.is_empty());
    let (false, Some(key)) = (pc.base_url.is_empty(), key) else {
        println!(
            "no refresh source configured for `{name}`: a base_url and a resolvable `{key_env}` are required"
        );
        return;
    };

    // Best-effort live fetch. Any network/HTTP error is reported, not fatal.
    let url = format!("{}/models", pc.base_url.trim_end_matches('/'));
    let body = match fetch_models(&url, &key) {
        Ok(text) => text,
        Err(e) => {
            println!("refresh failed for `{name}`: {e}");
            return;
        }
    };

    let models = match parse_models_response(&body) {
        Ok(m) => m,
        Err(e) => {
            println!("refresh failed for `{name}`: could not parse model listing: {e}");
            return;
        }
    };

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let mut cache = load_cache(&home);
    let count = models.len();
    cache.put(&name, now, models);
    match persist_cache(&home, &cache) {
        Ok(()) => println!("refreshed `{name}`: {count} models cached"),
        Err(e) => println!("refreshed `{name}` ({count} models) but failed to persist cache: {e}"),
    }
}

/// Blocking `GET` of a provider's models endpoint with a bearer token.
fn fetch_models(url: &str, key: &str) -> Result<String, Box<dyn std::error::Error>> {
    let resp = reqwest::blocking::Client::new()
        .get(url)
        .bearer_auth(key)
        .send()?;
    let status = resp.status();
    let text = resp.text()?;
    if !status.is_success() {
        return Err(format!("models endpoint returned {status}").into());
    }
    Ok(text)
}

/// Path to the on-disk model cache under `home/.origin/models-cache.json`.
fn cache_file(home: &std::path::Path) -> std::path::PathBuf {
    home.join(".origin").join("models-cache.json")
}

/// Load the persisted [`ModelCache`], or an empty cache when absent/unreadable.
fn load_cache(home: &std::path::Path) -> ModelCache {
    std::fs::read_to_string(cache_file(home))
        .ok()
        .and_then(|s| ModelCache::from_json(&s).ok())
        .unwrap_or_default()
}

/// Persist the [`ModelCache`] as JSON to `home/.origin/models-cache.json`.
fn persist_cache(home: &std::path::Path, cache: &ModelCache) -> Result<(), Box<dyn std::error::Error>> {
    let path = cache_file(home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, cache.to_json()?)?;
    Ok(())
}

/// Print the full config for a single provider by catalog id.
pub fn describe(id: &str) {
    let cat = merged_catalog();
    if let Some(e) = cat.lookup(id) {
        println!("id:            {}", e.id);
        println!("display_name:  {}", e.display_name);
        println!("wire:          {:?}", e.wire);
        println!("base_url:      {}", e.base_url);
        println!("chat_path:     {}", e.chat_path);
        println!("default_model: {}", e.default_model);
        println!("streaming:     {}", e.capabilities.streaming);
        println!("tools:         {}", e.capabilities.tools);
        println!("prompt_cache:  {}", e.capabilities.prompt_cache);
        println!("thinking:      {}", e.capabilities.thinking);
        match &e.auth {
            AuthScheme::OAuth(s) => {
                println!(
                    "auth:          oauth (pkce={}, device_flow={})",
                    s.pkce, s.device_flow
                );
                println!("  authorize_url: {}", s.authorize_url);
                println!("  token_url:     {}", s.token_url);
                println!("  client_id:     {}", s.client_id);
            }
            AuthScheme::ApiKey { header, prefix } => {
                println!("auth:          api-key (header={header}, prefix={prefix:?})");
            }
            AuthScheme::SigV4 { service } => {
                println!("auth:          sigv4 (service={service})");
            }
            AuthScheme::None => println!("auth:          none"),
            AuthScheme::Custom => println!("auth:          custom"),
        }
    } else {
        eprintln!("unknown provider: {id}");
        std::process::exit(2);
    }
}

#[cfg(test)]
mod tests {
    use super::{cache_file, load_cache, persist_cache};
    use origin_modeldiscovery::{ModelCache, ModelInfo};

    #[test]
    fn model_cache_persist_then_load_round_trips() {
        // The refresh path persists a `ModelCache` as JSON via `persist_cache`
        // and reads it back via `load_cache`; verify that round-trip end to end
        // against a scratch home directory.
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path();

        let mut cache = ModelCache::new();
        cache.put(
            "acme",
            1_000,
            vec![ModelInfo::new("gpt-4o".to_string(), Some(128_000), true)],
        );
        persist_cache(home, &cache).expect("persist");
        assert!(cache_file(home).exists(), "cache file should be written");

        let restored = load_cache(home);
        assert_eq!(restored.get("acme").map(<[_]>::len), Some(1));
    }

    #[test]
    fn load_cache_missing_is_empty() {
        // No cache file yet => an empty cache, not an error.
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = load_cache(dir.path());
        assert!(cache.get("anything").is_none());
    }
}
