// SPDX-License-Identifier: Apache-2.0
//! `origin oidc-exchange` — Workload Identity Federation token exchange.
//!
//! Mints a short-lived bearer token from a CI OIDC id token via an RFC 8693
//! token exchange (the [`origin_oidc`] building blocks). This is the keyless-CI
//! auth path: the runner's OIDC id token is presented as the subject, and the
//! STS returns an access token usable as the provider credential — origin's
//! `KeyVault` stores secrets but, before this, could not *mint* federated tokens.
//!
//! The minted token is printed to stdout (so CI can capture it into a secret /
//! `origin keyring add`); `--json` prints the full `ExchangedToken`.
//! *Closes: claude-code Workload Identity Federation auth.*
#![allow(clippy::module_name_repetitions)]

use anyhow::{anyhow, Result};
use origin_oidc::{build_exchange_form, parse_token_response, ExchangeRequest};

/// Arguments for the `oidc-exchange` subcommand.
pub struct OidcArgs {
    /// STS endpoint that performs the exchange.
    pub token_url: String,
    /// Subject token source: a literal JWT, `@<path>` to read a file, or
    /// `env:<NAME>` to read an environment variable.
    pub subject_token: String,
    /// Target audience for the exchanged token.
    pub audience: String,
    /// Optional Anthropic workspace id (`ANTHROPIC_WORKSPACE_ID`).
    pub workspace_id: Option<String>,
    /// Optional federation rule id (`anthropic_federation_rule_id`).
    pub federation_rule_id: Option<String>,
    /// Emit the full `ExchangedToken` as JSON instead of just the access token.
    pub json: bool,
}

/// Perform the token exchange and print the minted credential.
///
/// # Errors
/// Returns when the subject token cannot be resolved, the STS request fails, or
/// the response cannot be parsed into an access token.
pub async fn run(args: OidcArgs) -> Result<()> {
    let subject_token = resolve_subject_token(&args.subject_token)?;
    let req = ExchangeRequest {
        token_url: args.token_url.clone(),
        subject_token,
        audience: args.audience,
        workspace_id: args.workspace_id,
        federation_rule_id: args.federation_rule_id,
    };
    let form = build_exchange_form(&req);

    let client = reqwest::Client::new();
    let resp = client
        .post(&args.token_url)
        .form(&form)
        .send()
        .await
        .map_err(|e| anyhow!("token exchange request failed: {e}"))?;
    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| anyhow!("reading token-exchange response: {e}"))?;
    if !status.is_success() {
        return Err(anyhow!("STS returned {status}: {body}"));
    }

    let token = parse_token_response(&body).map_err(|e| anyhow!("{e}"))?;
    if args.json {
        println!("{}", serde_json::to_string(&token)?);
    } else {
        // The access token only — so `origin oidc-exchange … | origin keyring add`
        // style piping captures exactly the credential.
        println!("{}", token.access_token);
    }
    Ok(())
}

/// Resolve a subject-token argument into the actual JWT string.
///
/// - `@<path>`: read the file at `<path>` (trimmed).
/// - `env:<NAME>`: read environment variable `<NAME>`.
/// - anything else: used literally.
fn resolve_subject_token(arg: &str) -> Result<String> {
    if let Some(path) = arg.strip_prefix('@') {
        let s =
            std::fs::read_to_string(path).map_err(|e| anyhow!("reading subject token file `{path}`: {e}"))?;
        return Ok(s.trim().to_string());
    }
    if let Some(name) = arg.strip_prefix("env:") {
        let s = std::env::var(name).map_err(|_| anyhow!("subject-token env var `{name}` is not set"))?;
        return Ok(s.trim().to_string());
    }
    Ok(arg.to_string())
}

#[cfg(test)]
mod tests {
    use super::resolve_subject_token;

    #[test]
    fn literal_token_passes_through() {
        assert_eq!(
            resolve_subject_token("eyJhbGci.x.y").expect("literal"),
            "eyJhbGci.x.y"
        );
    }

    #[test]
    fn env_token_is_read_and_trimmed() {
        std::env::set_var("ORIGIN_TEST_OIDC_SUBJECT", "  tok-123\n");
        assert_eq!(
            resolve_subject_token("env:ORIGIN_TEST_OIDC_SUBJECT").expect("env"),
            "tok-123"
        );
        std::env::remove_var("ORIGIN_TEST_OIDC_SUBJECT");
    }

    #[test]
    fn missing_env_token_errors() {
        assert!(resolve_subject_token("env:ORIGIN_TEST_OIDC_MISSING").is_err());
    }

    #[test]
    fn file_token_is_read_and_trimmed() {
        let dir = std::env::temp_dir();
        let path = dir.join("origin_oidc_subject_test.jwt");
        std::fs::write(&path, "  file-tok-456  \n").expect("write");
        let arg = format!("@{}", path.display());
        assert_eq!(resolve_subject_token(&arg).expect("file"), "file-tok-456");
        let _ = std::fs::remove_file(&path);
    }
}
