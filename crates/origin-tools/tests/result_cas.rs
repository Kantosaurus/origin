// SPDX-License-Identifier: Apache-2.0
#![allow(clippy::unwrap_used)]

use origin_tools::result_cas::{ref_token, ResultStore};
use serde_json::json;

#[test]
fn same_body_yields_same_ref() {
    let store = ResultStore::new();
    let body = serde_json::to_vec(&json!({"a": 1, "b": 2})).unwrap();
    let h1 = store.put(&body);
    let h2 = store.put(&body);
    assert_eq!(h1, h2);
}

#[test]
fn different_bodies_yield_different_refs() {
    let store = ResultStore::new();
    let a = store.put(b"abc");
    let b = store.put(b"def");
    assert_ne!(a, b);
}

#[test]
fn ref_token_round_trips_through_store() {
    let store = ResultStore::new();
    let body = b"hello world";
    let h = store.put(body);
    let token = ref_token(&h, body.len(), "hello world");
    assert!(token["tool_result_ref"].as_str().unwrap().starts_with("blake3:"));
    assert_eq!(token["bytes"], body.len());
    assert_eq!(token["preview"], "hello world");
    let fetched = store.get(&h).unwrap();
    assert_eq!(&*fetched, body);
}

#[test]
fn store_preview_truncates_to_80_chars() {
    let store = ResultStore::new();
    let long = "x".repeat(200);
    let h = store.put(long.as_bytes());
    let token = ref_token(&h, 200, &long);
    assert_eq!(token["preview"].as_str().unwrap().chars().count(), 80);
}
