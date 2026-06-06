// SPDX-License-Identifier: Apache-2.0
use crate::source::{ImportedMessage, ImportedSession, MigrateBundle, Source, SourceError};
use rusqlite::Connection;
use std::path::Path;

#[allow(clippy::module_name_repetitions)]
#[derive(Default)]
pub struct JcodeSource;

impl Source for JcodeSource {
    fn name(&self) -> &'static str {
        "jcode"
    }

    fn scan(&self, root: &Path) -> Result<MigrateBundle, SourceError> {
        let db = root.join("sessions.sqlite");
        if !db.exists() {
            return Ok(MigrateBundle::default());
        }
        let c = Connection::open(&db).map_err(|e| SourceError::Parse {
            path: db.display().to_string(),
            reason: e.to_string(),
        })?;
        let mut bundle = MigrateBundle::default();

        let mut stmt = c
            .prepare("SELECT id, title, created_at FROM sessions ORDER BY created_at")
            .map_err(|e| SourceError::Parse {
                path: "sessions".into(),
                reason: e.to_string(),
            })?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })
            .map_err(|e| SourceError::Parse {
                path: "sessions".into(),
                reason: e.to_string(),
            })?;

        for row in rows {
            let (id, title, ts) = row.map_err(|e| SourceError::Parse {
                path: "sessions".into(),
                reason: e.to_string(),
            })?;
            let Ok(created_at_unix_ms) = u64::try_from(ts) else {
                tracing::warn!(
                    source = "jcode",
                    session_id = %id,
                    ts,
                    "skipping session with negative created_at timestamp"
                );
                continue;
            };
            let mut s = ImportedSession {
                source_id: id.clone(),
                title,
                created_at_unix_ms,
                messages: vec![],
            };
            let mut mstmt = c
                .prepare("SELECT role, body FROM messages WHERE session_id = ? ORDER BY ts")
                .map_err(|e| SourceError::Parse {
                    path: "messages".into(),
                    reason: e.to_string(),
                })?;
            let mrows = mstmt
                .query_map([&id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .map_err(|e| SourceError::Parse {
                    path: "messages".into(),
                    reason: e.to_string(),
                })?;
            for m in mrows {
                let (role, body) = m.map_err(|e| SourceError::Parse {
                    path: "messages".into(),
                    reason: e.to_string(),
                })?;
                s.messages.push(ImportedMessage { role, body });
            }
            bundle.sessions.push(s);
        }
        Ok(bundle)
    }
}
