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

    /// Intersection of every active skill's `allowed-tools`. `None` means no
    /// narrowing is in effect (the permission engine should fall through to
    /// the default tier check). An empty set means *no tool is allowed*.
    #[must_use]
    pub fn allowed_tools(&self) -> Option<HashSet<String>> {
        let mut iter = self.stack.iter();
        let first = iter.next()?;
        let mut acc: HashSet<String> = first.allowed_tools.iter().cloned().collect();
        for skill in iter {
            let cur: HashSet<String> = skill.allowed_tools.iter().cloned().collect();
            acc = acc.intersection(&cur).cloned().collect();
        }
        Some(acc)
    }
}
