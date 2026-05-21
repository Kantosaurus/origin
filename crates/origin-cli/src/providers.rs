//! `origin providers …` subcommand handlers.

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
