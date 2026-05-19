use origin_planner::{Band, CachePlanner, PrefixLedger, Section, SectionId};

#[test]
fn plan_emits_four_bands_in_canonical_order() {
    let ledger = PrefixLedger::new();
    let planner = CachePlanner::new(&ledger);
    let sections = vec![
        Section::new(SectionId::new("volatile-1"), Band::Volatile, 0..32),
        Section::new(SectionId::new("system"), Band::Frozen, 32..96),
        Section::new(SectionId::new("memories"), Band::Sticky, 96..160),
        Section::new(SectionId::new("history"), Band::Sliding, 160..224),
    ];

    let plan = planner.plan(&sections);
    let bands: Vec<Band> = plan.ordered_sections().iter().map(|s| s.band).collect();
    assert_eq!(
        bands,
        vec![Band::Frozen, Band::Sticky, Band::Sliding, Band::Volatile],
    );
}

#[test]
fn markers_are_emitted_at_band_boundaries_only() {
    let ledger = PrefixLedger::new();
    let planner = CachePlanner::new(&ledger);
    let sections = vec![
        Section::new(SectionId::new("a"), Band::Frozen, 0..32),
        Section::new(SectionId::new("b"), Band::Frozen, 32..64),
        Section::new(SectionId::new("c"), Band::Sticky, 64..128),
    ];
    let plan = planner.plan(&sections);
    let markers: Vec<usize> = plan.marker_indices().to_vec();
    // After section `b` ends at index 1 we cross Frozen→Sticky → one marker
    // sits between indices 1 and 2.
    assert_eq!(markers, vec![1]);
}
