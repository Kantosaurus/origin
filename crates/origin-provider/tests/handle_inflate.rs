// SPDX-License-Identifier: Apache-2.0
//! `inflate_tool_result_handles`: a hit inlines the stored payload; a miss
//! degrades to `CAS_MISS_PLACEHOLDER` instead of failing the whole turn.
//!
//! The miss path is the recovery half of the "cas miss for tool result handle"
//! fix: when an offloaded payload is lost in a daemon restart (Hot tier not
//! flushed), the transcript handle dangles. Rather than hard-error, the provider
//! substitutes a placeholder so the loop continues.

use origin_cas::{Store, StoreConfig};
use origin_core::types::{Block, Message, Role};
use origin_provider::{inflate_tool_result_handles, CAS_MISS_PLACEHOLDER};
use std::sync::Arc;

fn store(dir: &std::path::Path) -> Arc<Store> {
    Arc::new(
        Store::open(StoreConfig {
            root: dir.to_path_buf(),
            hot_capacity: 8,
            warm_pack_target_bytes: 1 << 20,
            cold_zstd_level: 3,
        })
        .expect("open"),
    )
}

/// The assistant `tool_use` that must precede a `tool_result` for the pairing to
/// be valid (the inflate choke now strips orphaned results).
fn assistant_tool_use(id: &str) -> Message {
    Message {
        role: Role::Assistant,
        blocks: vec![Block::ToolUse {
            id: id.into(),
            name: "Read".into(),
            input_json: b"{}".to_vec(),
            cache_marker: None,
        }],
    }
}

fn tool_result(handle: [u8; 32]) -> Message {
    Message {
        role: Role::Tool,
        blocks: vec![Block::ToolResult {
            tool_use_id: "t1".into(),
            handle: Some(handle),
            inline: None,
            cache_marker: None,
            is_error: false,
        }],
    }
}

#[test]
fn cas_miss_inflates_to_placeholder_instead_of_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cas = store(dir.path());

    // A handle whose payload was never stored (lost when the daemon restarted).
    let dangling: [u8; 32] = [9; 32];
    let out = inflate_tool_result_handles(&[assistant_tool_use("t1"), tool_result(dangling)], Some(&cas))
        .expect("a cas miss must degrade, not error");

    let block = &out[1].blocks[0];
    // The handle is consumed (take()) and replaced with the inline placeholder.
    assert!(
        matches!(
            block,
            Block::ToolResult {
                inline: Some(_),
                handle: None,
                ..
            }
        ),
        "miss must inflate to an inline placeholder with the handle consumed: {block:?}"
    );
    if let Block::ToolResult {
        inline: Some(bytes), ..
    } = block
    {
        assert_eq!(bytes.as_slice(), CAS_MISS_PLACEHOLDER.as_bytes());
    }
}

#[test]
fn cas_hit_inflates_the_stored_payload() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cas = store(dir.path());
    let payload = b"the real tool output";
    let h = cas.put(payload).expect("put");

    let out =
        inflate_tool_result_handles(&[assistant_tool_use("t1"), tool_result(*h.as_bytes())], Some(&cas)).expect("ok");

    let block = &out[1].blocks[0];
    assert!(
        matches!(block, Block::ToolResult { inline: Some(_), .. }),
        "hit must inflate to the stored payload: {block:?}"
    );
    if let Block::ToolResult {
        inline: Some(bytes), ..
    } = block
    {
        assert_eq!(bytes.as_slice(), payload);
    }
}
