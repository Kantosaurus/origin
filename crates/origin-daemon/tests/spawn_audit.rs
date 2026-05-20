//! Compile-time grep — no raw `tokio::spawn` calls remain in the daemon's
//! src directory. The `xtask lint-spawn` covers this too, but a unit-style
//! test catches it in `cargo test` before xtask runs.

const SRC: &[(&str, &str)] = &[
    ("agent.rs", include_str!("../src/agent.rs")),
    ("compactor.rs", include_str!("../src/compactor.rs")),
    ("main.rs", include_str!("../src/main.rs")),
    ("memory_wiring.rs", include_str!("../src/memory_wiring.rs")),
    ("runtime_launch.rs", include_str!("../src/runtime_launch.rs")),
    ("session.rs", include_str!("../src/session.rs")),
    ("session_store.rs", include_str!("../src/session_store.rs")),
    ("stream_relay.rs", include_str!("../src/stream_relay.rs")),
    ("tool_use_parser.rs", include_str!("../src/tool_use_parser.rs")),
];

#[test]
fn no_raw_tokio_spawn_in_daemon_src() {
    for (name, body) in SRC {
        for (lineno, line) in body.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            assert!(
                !line.contains("tokio::spawn(") && !line.contains("tokio::task::spawn("),
                "raw tokio::spawn at {name}:{} → use spawn_in(class, …): {}",
                lineno + 1,
                line.trim()
            );
        }
    }
}
