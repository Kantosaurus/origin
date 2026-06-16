// SPDX-License-Identifier: Apache-2.0
//! GitHub provider crate.
//!
//! The wired production provider is GitHub **Copilot** ([`copilot::provider`]),
//! whose chat API is `OpenAI`-shaped but authenticated via a short-lived Copilot
//! *session token* exchanged from the stored GitHub OAuth token. The daemon's
//! [`origin_provider::catalog::WireFormat::GitHubCopilot`] arm builds it.
//!
//! A separate `GitHub Models` (`models.github.ai`) `Provider` impl previously
//! lived here but had no production caller — the factory only ever built the
//! Copilot path, and `github` / `github-models` ids alias to `github-copilot` in
//! the provider-id parser — so it was removed as dead code.
#![allow(clippy::module_name_repetitions)]

pub mod copilot;
