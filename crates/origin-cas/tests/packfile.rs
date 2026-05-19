use origin_cas::{Hash, PackBuilder, PackReader};
use tempfile::tempdir;

#[test]
fn round_trip_single_payload() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("p0001.pack");

    let mut b = PackBuilder::create(&path).expect("create");
    let h = Hash::of(b"hello");
    b.append(h, b"hello").expect("append");
    let _ = b.finalize().expect("finalize");

    let r = PackReader::open(&path).expect("open");
    let bytes = r.read(h).expect("present");
    assert_eq!(bytes.as_ref(), b"hello");
}

#[test]
fn round_trip_multiple_payloads_in_order() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("p0002.pack");

    let payloads: Vec<&[u8]> = vec![b"alpha", b"beta", b"gamma-payload", b""];
    let hashes: Vec<Hash> = payloads.iter().map(|p| Hash::of(p)).collect();

    let mut b = PackBuilder::create(&path).expect("create");
    for (h, p) in hashes.iter().zip(&payloads) {
        b.append(*h, p).expect("append");
    }
    let _ = b.finalize().expect("finalize");

    let r = PackReader::open(&path).expect("open");
    for (h, p) in hashes.iter().zip(&payloads) {
        assert_eq!(r.read(*h).expect("present").as_ref(), *p);
    }
}

#[test]
fn missing_hash_returns_none() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("p0003.pack");
    let mut b = PackBuilder::create(&path).expect("create");
    b.append(Hash::of(b"a"), b"a").expect("append");
    let _ = b.finalize().expect("finalize");

    let r = PackReader::open(&path).expect("open");
    assert!(r.read(Hash::of(b"absent")).is_none());
}

proptest::proptest! {
    #[test]
    fn random_insertions_read_back_identically(payloads in proptest::collection::vec(proptest::collection::vec(proptest::num::u8::ANY, 0..4096), 1..32)) {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("p_prop.pack");
        let hashes: Vec<Hash> = payloads.iter().map(|p| Hash::of(p)).collect();

        let mut b = PackBuilder::create(&path).expect("create");
        for (h, p) in hashes.iter().zip(&payloads) {
            b.append(*h, p).expect("append");
        }
        let _ = b.finalize().expect("finalize");

        let r = PackReader::open(&path).expect("open");
        for (h, p) in hashes.iter().zip(&payloads) {
            let slice = r.read(*h).expect("present");
            proptest::prop_assert_eq!(slice.as_ref(), &p[..]);
        }
    }
}
