// SPDX-License-Identifier: Apache-2.0
use origin_core::ir::{CacheKind, ProviderCaps};

#[allow(clippy::assertions_on_constants)]
#[test]
fn provider_caps_compile_time_const() {
    const X: ProviderCaps = ProviderCaps {
        prompt_cache: CacheKind::Explicit,
        thinking: true,
        parallel_tools: true,
        vision: true,
        audio: false,
    };
    assert!(X.parallel_tools);
}
