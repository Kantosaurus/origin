#![allow(clippy::unwrap_used, clippy::string_lit_as_bytes)]

use origin_tools::text_fmt::{denormalise, detect, normalise_to_lf, Bom, Encoding, Eol};

#[test]
fn detects_lf_no_bom() {
    let d = detect(b"line1\nline2\n");
    assert_eq!(d.eol, Eol::Lf);
    assert_eq!(d.bom, None);
    assert_eq!(d.encoding, Encoding::Utf8);
    assert!(d.trailing_newline);
}

#[test]
fn detects_crlf() {
    let d = detect(b"line1\r\nline2\r\n");
    assert_eq!(d.eol, Eol::Crlf);
}

#[test]
fn detects_cr_only() {
    let d = detect(b"line1\rline2\r");
    assert_eq!(d.eol, Eol::Cr);
}

#[test]
fn detects_mixed() {
    let d = detect(b"a\r\nb\nc\r\n");
    assert_eq!(d.eol, Eol::Mixed);
}

#[test]
fn detects_utf8_bom() {
    let d = detect(b"\xef\xbb\xbfhello");
    assert_eq!(d.bom, Some(Bom::Utf8));
    assert_eq!(d.encoding, Encoding::Utf8);
}

#[test]
fn detects_utf16_le_bom() {
    let d = detect(b"\xff\xfeh\0e\0");
    assert_eq!(d.bom, Some(Bom::Utf16Le));
    assert_eq!(d.encoding, Encoding::Utf16Le);
}

#[test]
fn round_trip_crlf_preserves_bytes() {
    let original = b"a\r\nb\r\nc";
    let det = detect(original);
    let text = normalise_to_lf(original, &det).unwrap();
    assert_eq!(text, "a\nb\nc");
    let back = denormalise(&text, &det);
    assert_eq!(back, original);
}

#[test]
fn round_trip_mixed_preserves_per_line() {
    let original = b"a\r\nb\nc\r";
    let det = detect(original);
    let text = normalise_to_lf(original, &det).unwrap();
    assert_eq!(text, "a\nb\nc\n");
    let back = denormalise(&text, &det);
    assert_eq!(back, original);
}

#[test]
fn insert_inherits_preceding_line_eol() {
    let original = b"a\r\nb\r\nc";
    let det = detect(original);
    let text = normalise_to_lf(original, &det).unwrap();
    // simulate model inserting "X" after line 1
    let edited = text.replace("a\n", "a\nX\n");
    let back = denormalise(&edited, &det);
    // inserted line inherits CRLF from preceding line
    assert_eq!(back, b"a\r\nX\r\nb\r\nc");
}

#[test]
fn non_utf8_without_bom_errors() {
    let bytes = &[0xff, 0xff, 0xff];
    let det = detect(bytes);
    let result = normalise_to_lf(bytes, &det);
    assert!(result.is_err());
}

#[test]
fn round_trip_non_ascii_utf8_lf_is_byte_preserving() {
    // Regression: the old UTF-8 branch did `out.push(b as char)`, lifting each
    // byte as Latin-1, which mojibaked every multi-byte char on read AND
    // silently corrupted files on edit. Pure-LF non-ASCII content must decode
    // verbatim and round-trip byte-for-byte.
    for original in [
        "café\nrésumé\n".as_bytes().to_vec(),
        "日本語テスト\n".as_bytes().to_vec(),
        "emoji 😀🎉 done\n".as_bytes().to_vec(),
    ] {
        let det = detect(&original);
        let text = normalise_to_lf(&original, &det).unwrap();
        assert_eq!(
            text.as_bytes(),
            &original[..],
            "non-ASCII UTF-8 must decode without mojibake"
        );
        let back = denormalise(&text, &det);
        assert_eq!(back, original, "non-ASCII content must round-trip byte-for-byte");
    }
}

#[test]
fn round_trip_non_ascii_utf8_crlf() {
    // Folds CRLF -> LF at the char level (not byte level) and restores CRLF,
    // all while preserving the multi-byte `é` bytes intact.
    let original = "café\r\nrésumé\r\n".as_bytes();
    let det = detect(original);
    let text = normalise_to_lf(original, &det).unwrap();
    assert_eq!(text, "café\nrésumé\n");
    let back = denormalise(&text, &det);
    assert_eq!(back, original);
}

use proptest::prelude::*;

fn arb_eol() -> impl Strategy<Value = &'static [u8]> {
    prop_oneof![
        Just("\r\n".as_bytes()),
        Just("\n".as_bytes()),
        Just("\r".as_bytes()),
    ]
}

fn arb_line_with_eol() -> impl Strategy<Value = Vec<u8>> {
    ("[a-zA-Z]{1,8}", arb_eol()).prop_map(|(s, eol)| {
        let mut v = s.into_bytes();
        v.extend_from_slice(eol);
        v
    })
}

proptest! {
    #[test]
    fn round_trip_arbitrary_mixed_eol(lines in proptest::collection::vec(arb_line_with_eol(), 0..20)) {
        let original: Vec<u8> = lines.into_iter().flatten().collect();
        let det = detect(&original);
        let text = normalise_to_lf(&original, &det).unwrap();
        let back = denormalise(&text, &det);
        prop_assert_eq!(back, original);
    }
}
