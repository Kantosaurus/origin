// SPDX-License-Identifier: Apache-2.0
//! Stdio-JSON verb protocol shared by `agent-browser` and `CloakBrowser` backends.
//!
//! Wire format: one JSON object per line in each direction.
//! Both subprocess clients speak this; the router never sees raw bytes.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "v", rename_all = "lowercase")]
pub enum Verb {
    Open {
        url: String,
        session: String,
    },
    Click {
        r#ref: String,
        session: String,
    },
    Fill {
        r#ref: String,
        value: String,
        session: String,
    },
    Extract {
        r#ref: String,
        session: String,
    },
    Snapshot {
        session: String,
    },
    Screenshot {
        session: String,
        path: String,
    },
    Close {
        session: String,
    },
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
    /// Recent browser-console log lines captured during the action, newest
    /// last. Populated only by backends that opt in to console capture; older
    /// backends omit the field entirely.
    ///
    /// Serialized with `skip_serializing_if` so a `None` value emits **no**
    /// `console` key at all — keeping the wire byte-identical to pre-visual
    /// `SnapshotResp` responses for every backend that does not set it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub console: Option<Vec<String>>,
}
