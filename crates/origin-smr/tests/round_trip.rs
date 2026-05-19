//! Round-trip tests for the SPSC shared-memory ring.
//!
//! Per Phase 9 plan P9.4: one event in, one event out; alternating
//! fill/drain; and capacity-exceeded returns `WouldBlock`.

use origin_smr::{Ring, RingConfig, SwarmEvent};

fn unique_name(suffix: &str) -> String {
    format!("origin-smr-test-{}-{}", suffix, std::process::id())
}

#[test]
fn round_trips_a_single_event() {
    let name = unique_name("rt1");
    let producer = Ring::open(RingConfig {
        name: name.clone(),
        capacity_bytes: 4096,
        create: true,
    })
    .expect("open producer");
    let consumer = Ring::open(RingConfig {
        name,
        capacity_bytes: 4096,
        create: false,
    })
    .expect("open consumer");

    let evt = SwarmEvent::Heartbeat {
        sender: [7; 16],
        now_ms: 12345,
    };
    producer.try_send(&evt).expect("send");
    let got = consumer.try_recv().expect("recv").expect("Some");
    assert_eq!(got, evt);
    assert!(consumer.try_recv().expect("second").is_none());
}

#[test]
fn fills_then_drains_alternating() {
    let name = unique_name("rt2");
    let p = Ring::open(RingConfig {
        name: name.clone(),
        capacity_bytes: 8192,
        create: true,
    })
    .expect("p");
    let c = Ring::open(RingConfig {
        name,
        capacity_bytes: 8192,
        create: false,
    })
    .expect("c");
    for i in 0..100u64 {
        p.try_send(&SwarmEvent::Heartbeat {
            sender: [0; 16],
            now_ms: i,
        })
        .expect("send");
        let got = c.try_recv().expect("recv").expect("Some");
        assert!(matches!(got, SwarmEvent::Heartbeat { now_ms, .. } if now_ms == i));
    }
}

#[test]
fn capacity_exceeded_returns_would_block() {
    let name = unique_name("rt3");
    let p = Ring::open(RingConfig {
        name,
        capacity_bytes: 4096,
        create: true,
    })
    .expect("p");
    // Pack 200 large events; expect at least one WouldBlock once full.
    let payload = vec![0xAB; 256];
    let evt = SwarmEvent::DirectMessage {
        from: [0; 16],
        to: [1; 16],
        body: payload,
    };
    let mut hit = false;
    for _ in 0..200 {
        if matches!(p.try_send(&evt), Err(origin_smr::TrySendError::WouldBlock)) {
            hit = true;
            break;
        }
    }
    assert!(hit, "expected WouldBlock when ring fills");
}
