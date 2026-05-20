//! `origin providers …` subcommand handlers.

use origin_provider::catalog::{AuthScheme, Catalog};

/// Print every builtin catalog entry as a fixed-width table.
pub fn ls() {
    let cat = Catalog::builtin();
    println!("{:<20} {:<35} {:<14} {}", "ID", "DISPLAY NAME", "WIRE", "AUTH");
    for e in cat.entries() {
        let wire = format!("{:?}", e.wire);
        let auth = match &e.auth {
            AuthScheme::None => "none",
            AuthScheme::ApiKey { .. } => "api-key",
            AuthScheme::OAuth(_) => "oauth",
            AuthScheme::SigV4 { .. } => "sigv4",
            AuthScheme::Custom => "custom",
        };
        println!("{:<20} {:<35} {:<14} {}", e.id, e.display_name, wire, auth);
    }
}

/// Print the full config for a single provider by catalog id.
pub fn describe(id: &str) {
    let cat = Catalog::builtin();
    match cat.lookup(id) {
        Some(e) => {
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
                    println!("auth:          api-key (header={header}, prefix={:?})", prefix);
                }
                AuthScheme::SigV4 { service } => {
                    println!("auth:          sigv4 (service={service})");
                }
                AuthScheme::None => println!("auth:          none"),
                AuthScheme::Custom => println!("auth:          custom"),
            }
        }
        None => {
            eprintln!("unknown provider: {id}");
            std::process::exit(2);
        }
    }
}
