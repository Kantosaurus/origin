// SPDX-License-Identifier: Apache-2.0
//! Active-skill stack + allowed-tools intersection mask.

use crate::frontmatter::SkillFrontmatter;
use std::collections::HashSet;

#[allow(clippy::module_name_repetitions)] // unambiguous from outside the crate
#[derive(Debug, Default)]
pub struct SkillRegistry {
    stack: Vec<SkillFrontmatter>,
}

impl SkillRegistry {
    #[must_use]
    pub const fn new() -> Self {
        Self { stack: Vec::new() }
    }

    pub fn activate(&mut self, front: SkillFrontmatter) {
        self.stack.push(front);
    }

    pub fn deactivate(&mut self, name: &str) {
        if let Some(pos) = self.stack.iter().rposition(|s| s.name == name) {
            self.stack.remove(pos);
        }
    }

    /// Iterate the currently-active skills in activation order (oldest
    /// first). Used by daemon snapshotting where we need to clone the
    /// stack without holding a lock across a turn.
    pub fn iter_active(&self) -> impl Iterator<Item = &SkillFrontmatter> {
        self.stack.iter()
    }

    /// Intersection of every active skill's `allowed-tools`. `None` means no
    /// narrowing is in effect (the permission engine should fall through to
    /// the default tier check). An empty set means *no tool is allowed*.
    #[must_use]
    pub fn allowed_tools(&self) -> Option<HashSet<String>> {
        // A skill that declares no `allowed-tools` imposes NO narrowing — it
        // must not collapse the intersection to the empty (deny-all) set. Only
        // skills with a non-empty list contribute to the restriction; if none
        // do, return `None` so the permission engine falls through to the
        // default tier check.
        let mut restricting = self.stack.iter().filter(|s| !s.allowed_tools.is_empty());
        let first = restricting.next()?;
        let mut acc: HashSet<String> = first.allowed_tools.iter().cloned().collect();
        for skill in restricting {
            let cur: HashSet<String> = skill.allowed_tools.iter().cloned().collect();
            acc = acc.intersection(&cur).cloned().collect();
        }
        Some(acc)
    }
}
