use crate::source::{MigrateBundle, SourceError};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ApplyReport {
    pub sessions_inserted: usize,
    pub sessions_skipped_duplicate: usize,
    pub skills_inserted: usize,
    pub skills_skipped_duplicate: usize,
    pub memories_inserted: usize,
    pub memories_skipped_duplicate: usize,
}

/// Pure dry-run summary — no side effects.
#[must_use]
pub fn summarize(b: &MigrateBundle) -> ApplyReport {
    ApplyReport {
        sessions_inserted: b.sessions.len(),
        skills_inserted: b.skills.len(),
        memories_inserted: b.memories.len(),
        ..Default::default()
    }
}

/// Apply the bundle. Stub — real writes land in P14.B.6.
///
/// # Errors
/// Returns a [`SourceError`] when the underlying storage refuses the write.
pub fn apply(_b: &MigrateBundle) -> Result<ApplyReport, SourceError> {
    Ok(ApplyReport::default())
}
