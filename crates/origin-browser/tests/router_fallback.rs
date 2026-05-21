#![allow(clippy::unwrap_used)]

use origin_browser::router::BrowserRouter;
use origin_browser::protocol::Verb;

fn fake(name: &str) -> String {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("tests/fakes/{name}"))
        .to_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn falls_back_to_cloak_when_primary_signals_bot() {
    let mut router = BrowserRouter::with_commands(
        ("node", vec![fake("agent_browser_bot.mjs")]),
        ("node", vec![fake("cloak_fake.mjs")]),
    ).await.unwrap();
    let resp = router.run(&Verb::Open { url: "u".into(), session: "s1".into() }).await.unwrap();
    // The Cloak fake marks itself with title "cloak"; the bot fake uses "Just a moment..."
    assert_eq!(resp.title.as_deref(), Some("cloak"), "fallback should have taken over");
}

#[tokio::test]
async fn primary_used_when_clean() {
    let mut router = BrowserRouter::with_commands(
        ("node", vec![fake("agent_browser_fake.mjs")]),
        ("node", vec![fake("cloak_fake.mjs")]),
    ).await.unwrap();
    let resp = router.run(&Verb::Open { url: "u".into(), session: "s1".into() }).await.unwrap();
    assert_eq!(resp.title.as_deref(), Some("fake"));
}

#[tokio::test]
async fn sticks_to_cloak_after_two_successful_fallbacks() {
    let mut router = BrowserRouter::with_commands(
        ("node", vec![fake("agent_browser_bot.mjs")]),
        ("node", vec![fake("cloak_fake.mjs")]),
    ).await.unwrap();
    let _ = router.run(&Verb::Open { url: "a".into(), session: "s2".into() }).await.unwrap();
    let _ = router.run(&Verb::Open { url: "b".into(), session: "s2".into() }).await.unwrap();
    // After two fallbacks, the router must not call primary again. We can't
    // observe that directly without instrumentation; instead we check it
    // never re-queries primary by asserting only cloak responses come back.
    let r = router.run(&Verb::Open { url: "c".into(), session: "s2".into() }).await.unwrap();
    assert_eq!(r.title.as_deref(), Some("cloak"));
    assert!(router.sticky_cloak("s2"), "sticky bit should be set");
}
