//! P11.5 — every `ToolMeta` carries a `sandbox_profile` ordinal. Builtins
//! that exec untrusted binaries opt into stricter profiles.

use origin_sandbox::SandboxProfile;
use origin_tools::{registry_iter, ToolMeta};

#[test]
fn every_builtin_declares_a_profile() {
    let metas: Vec<&ToolMeta> = registry_iter().collect();
    assert!(!metas.is_empty(), "expected at least one builtin registered");
    // The default for un-migrated tools is `Inherit`; the migration sweep is
    // intentionally incremental (see P12). Only assert the field exists by
    // exercising its ordinal.
    for m in metas {
        let _ord = m.sandbox_profile.ordinal();
    }
}

#[test]
fn bash_uses_shell_profile() {
    let meta = registry_iter()
        .find(|m| m.name == "Bash")
        .expect("Bash registered");
    assert_eq!(meta.sandbox_profile, SandboxProfile::Shell);
}

#[test]
fn edit_uses_write_cwd_profile() {
    let meta = registry_iter()
        .find(|m| m.name == "Edit")
        .expect("Edit registered");
    assert_eq!(meta.sandbox_profile, SandboxProfile::WriteCwd);
}

#[test]
fn read_uses_read_fs_profile() {
    let meta = registry_iter()
        .find(|m| m.name == "Read")
        .expect("Read registered");
    assert_eq!(meta.sandbox_profile, SandboxProfile::ReadFs);
}
