// SPDX-License-Identifier: Apache-2.0
//! Active-skill stack + allowed-tools intersection mask.

use crate::frontmatter::SkillFrontmatter;
use std::collections::HashSet;

/// One entry on the active-skill stack: parsed frontmatter plus the skill body.
///
/// The frontmatter drives the `allowed-tools` mask and the catalog marker; the
/// body is the actual `SKILL.md` instructions the model must follow once the
/// skill is active. The body is what makes an activated skill *do* anything
/// beyond narrowing tools: it is injected verbatim into the per-turn system
/// prompt under `<origin-active-skills>`. Activations that only carry
/// frontmatter (e.g. a workflow step that hands us a bare `SkillFrontmatter`)
/// leave `body` empty.
#[derive(Debug, Clone)]
pub struct ActiveSkill {
    pub front: SkillFrontmatter,
    pub body: String,
}

#[allow(clippy::module_name_repetitions)] // unambiguous from outside the crate
#[derive(Debug, Default)]
pub struct SkillRegistry {
    stack: Vec<ActiveSkill>,
}

impl SkillRegistry {
    #[must_use]
    pub const fn new() -> Self {
        Self { stack: Vec::new() }
    }

    /// Activate a skill from frontmatter alone. The body is empty, so the
    /// skill contributes its `allowed-tools` mask and catalog marker but no
    /// prompt instructions. Prefer [`SkillRegistry::activate_with_body`] when
    /// the `SKILL.md` body is available — without it the model never receives
    /// the skill's directives and so cannot carry them out.
    pub fn activate(&mut self, front: SkillFrontmatter) {
        self.stack.push(ActiveSkill {
            front,
            body: String::new(),
        });
    }

    /// Activate a skill carrying its full `SKILL.md` body. The body is what the
    /// daemon injects into the per-turn system prompt so the model actually
    /// executes the skill's instructions.
    pub fn activate_with_body(&mut self, front: SkillFrontmatter, body: String) {
        self.stack.push(ActiveSkill { front, body });
    }

    pub fn deactivate(&mut self, name: &str) {
        if let Some(pos) = self.stack.iter().rposition(|s| s.front.name == name) {
            self.stack.remove(pos);
        }
    }

    /// Iterate the currently-active skills' frontmatter in activation order
    /// (oldest first). Used by daemon snapshotting and the catalog `*` marker.
    pub fn iter_active(&self) -> impl Iterator<Item = &SkillFrontmatter> {
        self.stack.iter().map(|s| &s.front)
    }

    /// Iterate the full active-skill entries (frontmatter + body) in
    /// activation order. The daemon uses this to (a) snapshot the stack
    /// without losing bodies and (b) build the `<origin-active-skills>`
    /// system-prompt block.
    pub fn iter_active_entries(&self) -> impl Iterator<Item = &ActiveSkill> {
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
        let mut restricting = self
            .stack
            .iter()
            .map(|s| &s.front)
            .filter(|s| !s.allowed_tools.is_empty());
        let first = restricting.next()?;
        let mut acc: HashSet<String> = first.allowed_tools.iter().cloned().collect();
        for skill in restricting {
            let cur: HashSet<String> = skill.allowed_tools.iter().cloned().collect();
            acc = acc.intersection(&cur).cloned().collect();
        }
        Some(acc)
    }
}
