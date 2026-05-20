//! Dirty fixture — calls raw tokio::spawn.

#[allow(dead_code)]
fn bad() {
    tokio::spawn(async {});
}
