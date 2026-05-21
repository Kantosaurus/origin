//! Stdio-JSON verb protocol shared by `agent-browser` and `CloakBrowser` backends.
//!
//! Wire format: one JSON object per line in each direction.
//! Both subprocess clients speak this; the router never sees raw bytes.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "v", rename_all = "lowercase")]
pub enum Verb {
    Open    { url: String,  session: String },
    Click   { r#ref: String, session: String },
    Fill    { r#ref: String, value: String, session: String },
    Extract { r#ref: String, session: String },
    Snapshot{ session: String },
    Screenshot { session: String, path: String },
    Close   { session: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotResp {
    pub ok: bool,
    #[serde(default)]
    pub r#ref: Option<String>,
    #[serde(default)]
    pub snapshot: Option<String>,
    #[serde(default)]
    pub html: Option<String>,
    #[serde(default)]
    pub status: Option<u16>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}
