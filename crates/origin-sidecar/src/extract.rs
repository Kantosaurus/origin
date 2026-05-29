// SPDX-License-Identifier: Apache-2.0
//! Tool-output structure extraction (N2.5.c).

use origin_cas::{Hash, Store};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::job::ExtractDeliverer;

pub const EXTRACT_THRESHOLD_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Outline {
    pub byte_count: u64,
    pub line_count: u32,
    pub first_120_chars: String,
}

pub async fn run(cas: &Arc<Store>, source: Hash, deliver_to: &dyn ExtractDeliverer) {
    let Ok(Some(bytes)) = cas.get(source) else { return };
    let outline = Outline {
        byte_count: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        #[allow(
            clippy::naive_bytecount,
            reason = "bytecount crate not in workspace deps; newline scan over bounded 16 KB+ blobs is acceptable"
        )]
        line_count: u32::try_from(bytes.iter().filter(|b| **b == b'\n').count()).unwrap_or(u32::MAX),
        first_120_chars: {
            let cut = bytes.len().min(120);
            String::from_utf8_lossy(&bytes[..cut]).to_string()
        },
    };
    let Ok(json) = serde_json::to_vec(&outline) else {
        return;
    };
    let Ok(outline_handle) = cas.put(&json) else {
        return;
    };
    deliver_to.deliver(source, outline_handle).await;
}
