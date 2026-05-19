//! Latency smoke test for the SPSC ring.
//!
//! Informational benchmark with a generous 1 ms per-iter ceiling so it
//! never flakes on slow CI hosts. Phase 9 plan P9.4.

use origin_smr::{Ring, RingConfig, SwarmEvent};

const N: u64 = 1_000;

#[test]
fn round_trip_completes_under_1ms() {
    let name = format!("origin-smr-lat-{}", std::process::id());
    let p = Ring::open(RingConfig {
        name: name.clone(),
        capacity_bytes: 4096,
        create: true,
    })
    .expect("p");
    let c = Ring::open(RingConfig {
        name,
        capacity_bytes: 4096,
        create: false,
    })
    .expect("c");
    let evt = SwarmEvent::Heartbeat {
        sender: [0; 16],
        now_ms: 0,
    };
    // Warm-up
    for _ in 0..10 {
        p.try_send(&evt).expect("warm send");
        let _ = c.try_recv().expect("warm recv");
    }
    let start = std::time::Instant::now();
    for _ in 0..N {
        p.try_send(&evt).expect("send");
        let _ = c.try_recv().expect("recv").expect("Some");
    }
    let elapsed = start.elapsed();
    let per = elapsed / u32::try_from(N).expect("N fits u32");
    eprintln!("round-trip avg: {per:?}");
    assert!(
        per < std::time::Duration::from_millis(1),
        "round-trip per iter {per:?} too slow"
    );
}
