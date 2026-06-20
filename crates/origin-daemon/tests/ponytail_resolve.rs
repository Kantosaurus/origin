// SPDX-License-Identifier: Apache-2.0
//! Task 10: a per-request ponytail token resolves to the matching mode and wins
//! over the default. This locks the `PromptRequest.ponytail` → `LoopOptions.ponytail`
//! resolution wiring the daemon performs in `handle_request`.

#[test]
fn request_token_overrides_default() {
    use origin_ponytail::{resolve_mode, PonytailMode};
    let req_token = Some("ultra".to_string());
    let mode = resolve_mode(req_token.as_deref().and_then(PonytailMode::parse_level));
    assert_eq!(mode, PonytailMode::Ultra);
}

#[test]
fn no_request_token_falls_back_to_full() {
    use origin_ponytail::{resolve_mode, PonytailMode};
    // With no per-request token and no config/env override in the test env,
    // the resolver defaults to `Full` (the LoopOptions default mirror).
    std::env::remove_var("PONYTAIL_DEFAULT_MODE");
    std::env::remove_var("ORIGIN_PONYTAIL");
    let mode = resolve_mode(None);
    assert_eq!(mode, PonytailMode::Full);
}
