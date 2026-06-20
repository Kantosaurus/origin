// SPDX-License-Identifier: Apache-2.0
//! The pure ponytail dependency classifier. No I/O, no prompting — it only
//! decides which added deps are flagged. The daemon turns flags into action.

use std::collections::BTreeSet;

use crate::detect::Dep;
use crate::mode::PonytailMode;
use crate::native_table::{lookup, NativeReplacement};

#[derive(Debug, Clone, Copy)]
pub enum FlagKind {
    /// Has a native/stdlib replacement (flagged in lite/full/ultra).
    Replaceable(&'static NativeReplacement),
    /// No replacement, but ultra challenges every new dependency (rung 4).
    Unjustified,
}

#[derive(Debug, Clone)]
pub struct Flagged {
    pub dep: Dep,
    pub kind: FlagKind,
}

impl Flagged {
    #[must_use]
    pub fn message(&self) -> String {
        match self.kind {
            FlagKind::Replaceable(r) => format!(
                "ponytail rung {}: `{}` — use {} ({}). Drop the dependency.",
                r.rung, self.dep.name, r.native, r.note
            ),
            FlagKind::Unjustified => format!(
                "ponytail rung 4: new dependency `{}` — does the task need it at all? \
                 Justify it or use what's already here.",
                self.dep.name
            ),
        }
    }
}

/// Classify added deps for a mode. `Off` ⇒ none. `Lite`/`Full` ⇒ replaceable
/// only. `Ultra` ⇒ every non-allowlisted new dep.
#[must_use]
pub fn classify(deps: &[Dep], mode: PonytailMode, allow: &BTreeSet<String>) -> Vec<Flagged> {
    if mode == PonytailMode::Off {
        return Vec::new();
    }
    deps.iter()
        .filter(|d| !allow.contains(&d.name.to_ascii_lowercase()))
        .filter_map(|d| match lookup(d.eco, &d.name) {
            Some(repl) => Some(Flagged { dep: d.clone(), kind: FlagKind::Replaceable(repl) }),
            None if mode == PonytailMode::Ultra => {
                Some(Flagged { dep: d.clone(), kind: FlagKind::Unjustified })
            }
            None => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_table::Ecosystem;
    use std::collections::BTreeSet;

    fn deps() -> Vec<Dep> {
        vec![
            Dep { eco: Ecosystem::Npm, name: "lodash".into() }, // replaceable
            Dep { eco: Ecosystem::Npm, name: "react".into() },  // no replacement
        ]
    }

    #[test]
    fn off_flags_nothing() {
        assert!(classify(&deps(), PonytailMode::Off, &BTreeSet::new()).is_empty());
    }

    #[test]
    fn full_flags_only_replaceable() {
        let f = classify(&deps(), PonytailMode::Full, &BTreeSet::new());
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].dep.name, "lodash");
        assert!(matches!(f[0].kind, FlagKind::Replaceable(_)));
    }

    #[test]
    fn lite_flags_same_as_full() {
        assert_eq!(classify(&deps(), PonytailMode::Lite, &BTreeSet::new()).len(), 1);
    }

    #[test]
    fn ultra_flags_every_new_dep() {
        let f = classify(&deps(), PonytailMode::Ultra, &BTreeSet::new());
        assert_eq!(f.len(), 2);
        assert!(f.iter().any(|x| x.dep.name == "react" && matches!(x.kind, FlagKind::Unjustified)));
    }

    #[test]
    fn allowlist_short_circuits() {
        let allow: BTreeSet<String> = ["lodash".into()].into_iter().collect();
        assert!(classify(&deps(), PonytailMode::Full, &allow).is_empty());
    }
}
