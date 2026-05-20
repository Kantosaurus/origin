#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Invariant: chunking arbitrary bytes never panics and the chunk
    // ranges cover the input contiguously without gaps or overlaps.
    let chunks = origin_cas::chunker::chunk(data);
    if !data.is_empty() {
        let total: usize = chunks.iter().map(|(_, l)| l).sum();
        debug_assert_eq!(total, data.len());
        let mut next = 0usize;
        for (off, len) in &chunks {
            debug_assert_eq!(*off, next);
            next = next.wrapping_add(*len);
        }
        debug_assert_eq!(next, data.len());
    }
});
