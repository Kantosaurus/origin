//! `bearer_ttl_secs` defaults to one day, but operators can tighten or
//! extend remote-session lifetimes via `ORIGIN_BEARER_TTL_SECS`.

use origin_daemon::config::bearer_ttl_secs;

// Tests serialize via this mutex because `bearer_ttl_secs` reads a
// process-wide env var. Removed-then-restored is the only way to be
// deterministic across multiple tests in this binary.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn defaults_to_one_day_when_env_unset() {
    // The mutex may be poisoned if a sibling test panicked; that's fine —
    // we only need serialization, not state preservation.
    let _g = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    std::env::remove_var("ORIGIN_BEARER_TTL_SECS");
    assert_eq!(bearer_ttl_secs(), 86_400);
}

#[test]
fn env_var_overrides_default() {
    // The mutex may be poisoned if a sibling test panicked; that's fine —
    // we only need serialization, not state preservation.
    let _g = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    std::env::set_var("ORIGIN_BEARER_TTL_SECS", "3600");
    let ttl = bearer_ttl_secs();
    std::env::remove_var("ORIGIN_BEARER_TTL_SECS");
    assert_eq!(ttl, 3600);
}

#[test]
fn invalid_env_var_falls_back_to_default() {
    // The mutex may be poisoned if a sibling test panicked; that's fine —
    // we only need serialization, not state preservation.
    let _g = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    std::env::set_var("ORIGIN_BEARER_TTL_SECS", "not-a-number");
    let ttl = bearer_ttl_secs();
    std::env::remove_var("ORIGIN_BEARER_TTL_SECS");
    assert_eq!(ttl, 86_400);
}
