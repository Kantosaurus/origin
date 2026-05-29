// SPDX-License-Identifier: Apache-2.0
use origin_mcp::{JsonRpcId, JsonRpcRequest, JsonRpcResponse};
use serde_json::json;

#[test]
fn request_serializes_with_jsonrpc_field() {
    let req = JsonRpcRequest::new(JsonRpcId::Num(1), "tools/list", json!({}));
    let s = serde_json::to_string(&req).expect("ser");
    assert!(s.contains("\"jsonrpc\":\"2.0\""));
    assert!(s.contains("\"method\":\"tools/list\""));
    assert!(s.contains("\"id\":1"));
}

#[test]
fn response_ok_round_trip() {
    let json_resp = json!({"jsonrpc":"2.0","id":1,"result":{"tools":[]}}).to_string();
    let resp: JsonRpcResponse = serde_json::from_str(&json_resp).expect("de");
    let result = resp.into_result().expect("ok");
    assert!(result.get("tools").is_some());
}

#[test]
fn response_err_round_trip() {
    let json_resp =
        json!({"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"method not found"}}).to_string();
    let resp: JsonRpcResponse = serde_json::from_str(&json_resp).expect("de");
    let err = resp.into_result().expect_err("should be err");
    assert_eq!(err.code, -32601);
}
