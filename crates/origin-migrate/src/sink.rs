use crate::source::{MigrateBundle, SourceError};
use origin_store::Store;

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

/// Stub kept for back-compat with B.1's surface; returns a dry-run summary.
///
/// # Errors
/// Currently infallible; returns a [`SourceError`] only if extended later.
pub fn apply(b: &MigrateBundle) -> Result<ApplyReport, SourceError> {
    Ok(summarize(b))
}

/// Idempotent apply through a [`Store`]. Content-hash dedupe ensures
/// re-running `origin import` does not duplicate sessions or skills.
///
/// # Errors
/// Returns a [`SourceError`] when storage refuses a write.
pub fn apply_with_store(store: &Store, b: &MigrateBundle) -> Result<ApplyReport, SourceError> {
    let mut r = ApplyReport::default();

    for s in &b.sessions {
        let mut hasher = blake3::Hasher::new();
        hasher.update(s.source_id.as_bytes());
        for m in &s.messages {
            hasher.update(m.role.as_bytes());
            hasher.update(b":");
            hasher.update(m.body.as_bytes());
            hasher.update(b"\n");
        }
        let key = hasher.finalize().to_hex().to_string();

        if store.contains_migrated_session(&key).map_err(io_err)? {
            r.sessions_skipped_duplicate += 1;
            continue;
        }
        let body = serde_json::to_string(s).map_err(io_err)?;
        store.insert_migrated_session(&key, &body).map_err(io_err)?;
        r.sessions_inserted += 1;
    }

    for k in &b.skills {
        let key = blake3::hash(k.body.as_bytes()).to_hex().to_string();
        if store.contains_migrated_skill(&key).map_err(io_err)? {
            r.skills_skipped_duplicate += 1;
            continue;
        }
        let body = serde_json::to_string(k).map_err(io_err)?;
        store.insert_migrated_skill(&key, &body).map_err(io_err)?;
        r.skills_inserted += 1;
    }

    Ok(r)
}

fn io_err(e: impl std::fmt::Display) -> SourceError {
    SourceError::Parse {
        path: "store".into(),
        reason: e.to_string(),
    }
}
