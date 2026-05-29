// SPDX-License-Identifier: Apache-2.0
use origin_sandbox::{ProfileOrdinal, SandboxProfile};

#[test]
fn ordinals_are_stable() {
    assert_eq!(SandboxProfile::Inherit.ordinal().0, 0);
    assert_eq!(SandboxProfile::ReadFs.ordinal().0, 1);
    assert_eq!(SandboxProfile::WriteCwd.ordinal().0, 2);
    assert_eq!(SandboxProfile::Shell.ordinal().0, 3);
    assert_eq!(SandboxProfile::Network.ordinal().0, 4);
}

#[test]
fn default_is_inherit() {
    assert_eq!(SandboxProfile::default(), SandboxProfile::Inherit);
}

#[test]
fn round_trips_from_ordinal() {
    for raw in 0u8..=4 {
        let p = SandboxProfile::from_ordinal(ProfileOrdinal(raw)).expect("known ordinal");
        assert_eq!(p.ordinal().0, raw);
    }
    assert!(SandboxProfile::from_ordinal(ProfileOrdinal(255)).is_none());
}
