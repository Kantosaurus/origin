//! `origin-skills` — Skills loader, embedding upsert, and allowed-tools narrowing.
//!
//! Modules land per-task across P10.1–P10.4; this `lib.rs` collects them.

pub mod embed;
pub mod frontmatter;
pub mod import;
pub mod loader;
pub mod registry;

pub use embed::{SkillEmbedError, SkillEmbedder};
pub use frontmatter::{parse_frontmatter, FrontmatterError, SkillFrontmatter};
pub use import::{first_run_import, ImportDecision, ImportError, ImportReport};
pub use loader::{load_skills_dir, LoaderError, Skill, SkillHash};
pub use registry::SkillRegistry;
