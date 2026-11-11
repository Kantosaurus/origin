// SPDX-License-Identifier: Apache-2.0
//! Native ponytail: the lazy-senior-dev ruleset + a deterministic dependency gate.

pub mod commands;
pub mod config;
pub mod debt;
pub mod detect;
pub mod gate;
pub mod mode;
pub mod native_table;
pub mod ruleset;

pub use config::{allowlist, config_path, origin_home, remember, resolve_mode};
pub use debt::{DebtAction, DebtEvent};
pub use detect::{bash_installs, manifest_deps_added, manifest_deps_in_added_lines, Dep};
pub use gate::{classify, FlagKind, Flagged};
pub use mode::{parse_ponytail_command, PonytailCmd, PonytailMode};
pub use native_table::{lookup, Ecosystem, NativeReplacement};
pub use ruleset::system_block;
