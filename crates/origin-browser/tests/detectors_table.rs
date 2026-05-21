use origin_browser::protocol::SnapshotResp;
use origin_browser::detectors::{classify, Verdict};

fn resp(status: Option<u16>, html: &str, title: &str) -> SnapshotResp {
    SnapshotResp {
        ok: true, r#ref: None, snapshot: None,
        html: Some(html.into()),
        status, title: Some(title.into()), error: None,
    }
}

#[test]
fn clean_html_is_clean() {
    let v = classify(&resp(Some(200), "<html><body>Hello</body></html>", "OK"));
    assert!(matches!(v, Verdict::Clean), "got {v:?}");
}

#[test]
fn cloudflare_challenge_detected_by_body() {
    let html = r#"<html><body><script src="/cdn-cgi/challenge-platform/__cf_chl_"></script></body></html>"#;
    let v = classify(&resp(Some(403), html, "Just a moment..."));
    assert!(matches!(v, Verdict::BotDetected(_)));
}

#[test]
fn recaptcha_detected_by_class() {
    let html = r#"<div class="g-recaptcha" data-sitekey="abc"></div>"#;
    let v = classify(&resp(Some(200), html, "Login"));
    assert!(matches!(v, Verdict::BotDetected(_)));
}

#[test]
fn datadome_detected() {
    let v = classify(&resp(Some(200), "<script>var datadome='abc'</script>", "Loading"));
    assert!(matches!(v, Verdict::BotDetected(_)));
}

#[test]
fn title_verify_human_detected() {
    let v = classify(&resp(Some(200), "<html></html>", "Verify you are human"));
    assert!(matches!(v, Verdict::BotDetected(_)));
}

#[test]
fn rate_limit_detected() {
    let v = classify(&resp(Some(429), "<html></html>", "Too many requests"));
    assert!(matches!(v, Verdict::BotDetected(_)));
}
