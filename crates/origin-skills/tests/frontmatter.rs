use origin_skills::{parse_frontmatter, FrontmatterError};

const GOOD: &str = "---\nname: testing-basics\ndescription: How to write tests.\nallowed-tools: [Read, Bash]\n---\nBody text after frontmatter.\n";

const MISSING_END: &str = "---\nname: testing-basics\ndescription: missing close\nBody.\n";

#[test]
fn parses_valid_frontmatter() {
    let parsed = parse_frontmatter(GOOD).expect("parse good");
    assert_eq!(parsed.front.name, "testing-basics");
    assert_eq!(parsed.front.description, "How to write tests.");
    assert_eq!(
        parsed.front.allowed_tools,
        vec!["Read".to_string(), "Bash".to_string()]
    );
    assert_eq!(parsed.body.trim(), "Body text after frontmatter.");
}

#[test]
fn rejects_missing_close_delim() {
    match parse_frontmatter(MISSING_END) {
        Err(FrontmatterError::MissingDelimiter) => {}
        #[allow(clippy::panic)] // test assertion — intentional
        other => panic!("expected MissingDelimiter, got {other:?}"),
    }
}

#[test]
fn rejects_invalid_yaml() {
    let bad = "---\nname: [unclosed\n---\nbody\n";
    assert!(matches!(parse_frontmatter(bad), Err(FrontmatterError::Yaml(_))));
}

#[test]
fn accepts_crlf_line_endings() {
    // Windows checkouts (git core.autocrlf=true) convert LF → CRLF, so every
    // embedded SKILL.md on a Windows host arrives with \r\n. The parser must
    // tolerate this, not panic with MissingOpen on the leading `---\r\n`.
    let crlf = "---\r\nname: crlf-skill\r\ndescription: CRLF endings.\r\n---\r\nBody line one.\r\nBody line two.\r\n";
    let parsed = parse_frontmatter(crlf).expect("parse crlf");
    assert_eq!(parsed.front.name, "crlf-skill");
    assert_eq!(parsed.front.description, "CRLF endings.");
    assert!(parsed.body.contains("Body line one."));
    assert!(parsed.body.contains("Body line two."));
}

#[test]
fn strips_utf8_bom() {
    // Editors that save with a UTF-8 BOM (notably Notepad on older Windows)
    // would otherwise turn the leading `---` into `\u{FEFF}---` and fail.
    let bom = "\u{FEFF}---\nname: bom-skill\ndescription: Has BOM.\n---\nBody.\n";
    let parsed = parse_frontmatter(bom).expect("parse bom");
    assert_eq!(parsed.front.name, "bom-skill");
}
