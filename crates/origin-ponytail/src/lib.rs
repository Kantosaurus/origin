// SPDX-License-Identifier: Apache-2.0
//! Native ponytail: the lazy-senior-dev ruleset + a deterministic dependency gate.

pub mod mode;
pub mod native_table;
pub mod detect;
pub mod gate;
pub mod config;
pub mod ruleset;
pub mod debt;
pub mod commands;

pub use mode::{parse_ponytail_command, PonytailCmd, PonytailMode};
pub use native_table::{lookup, Ecosystem, NativeReplacement};
pub use detect::{bash_installs, manifest_deps_added, manifest_deps_in_added_lines, Dep};
pub use gate::{classify, FlagKind, Flagged};
pub use config::{allowlist, config_path, origin_home, remember, resolve_mode};
pub use ruleset::system_block;
pub use debt::{DebtAction, DebtEvent};
