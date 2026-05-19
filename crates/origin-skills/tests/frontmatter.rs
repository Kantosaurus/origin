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
