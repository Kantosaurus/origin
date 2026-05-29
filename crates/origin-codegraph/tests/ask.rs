// SPDX-License-Identifier: Apache-2.0
use origin_codegraph::ask::{classify, MemRouter, NullMemRouter, Route};

#[test]
fn code_shaped_routes_to_codegraph() {
    assert_eq!(classify("where is `fn parse_request` defined"), Route::Code);
    assert_eq!(classify("show me the callers of insert_node"), Route::Code);
    assert_eq!(
        classify("which struct implements Iterator for ChunkRef"),
        Route::Code
    );
}

#[test]
fn memory_shaped_routes_to_mem() {
    assert_eq!(
        classify("what did we decide about pinning rusqlite earlier"),
        Route::Mem
    );
    assert_eq!(
        classify("remember when we discussed the V2 migration"),
        Route::Mem
    );
}

#[test]
fn hybrid_shaped_routes_to_both() {
    assert_eq!(
        classify("the function I worked on last week that handled tree-sitter parsing"),
        Route::Both,
    );
}

#[test]
fn null_mem_returns_no_hits() {
    let r = NullMemRouter;
    assert!(r.search("anything").is_empty());
}
