// SPDX-License-Identifier: Apache-2.0
use std::fs;
use std::path::Path;

/// Stamp `{{VERSION}}` and `{{SHA256_*}}` placeholders in packaging templates
/// from a manifest JSON. Used by the post-release job once `release.yml`
/// uploads + records the per-target SHA256 set.
///
/// # Errors
/// Returns any error from filesystem I/O or JSON parsing.
pub fn stamp(version: &str, manifest: &Path, out_dir: &Path) -> anyhow::Result<()> {
    let m: serde_json::Value = serde_json::from_slice(&fs::read(manifest)?)?;
    fs::create_dir_all(out_dir)?;
    for tmpl in [
        "homebrew/origin.rb",
        "winget/manifests/Kantosaurus.origin.yaml",
        "winget/manifests/Kantosaurus.origin.installer.yaml",
        "winget/manifests/Kantosaurus.origin.locale.en-US.yaml",
        "aur/PKGBUILD",
        "scoop/origin.json",
        // Nix flake source pins (version + per-system raw-binary URL/sha256).
        // Stamped to out_dir/flake-sources.json; the `nix-sources-update`
        // release job commits it to packaging/nix/flake-sources.json on dev.
        "nix/flake-sources.json",
        // Chocolatey nuspec + tools scripts. Unlike the others these KEEP their
        // `chocolatey/` + `tools/` subtree (see the dest logic below) so
        // `choco pack` finds tools\chocolateyinstall.ps1. The uninstall script
        // carries no placeholders but is routed through the stamper so it lands
        // in the packed subtree.
        "chocolatey/origin.nuspec",
        "chocolatey/tools/chocolateyinstall.ps1",
        "chocolatey/tools/chocolateyuninstall.ps1",
    ] {
        let src = Path::new("packaging").join(format!("{tmpl}.tmpl"));
        let body = fs::read_to_string(&src)?;
        let stamped = body
            .replace("{{VERSION}}", version)
            .replace(
                "{{SHA256_MAC_ARM}}",
                m["aarch64-apple-darwin"].as_str().unwrap_or(""),
            )
            .replace(
                "{{SHA256_MAC_X64}}",
                m["x86_64-apple-darwin"].as_str().unwrap_or(""),
            )
            .replace(
                "{{SHA256_LINUX_ARM}}",
                m["aarch64-unknown-linux-gnu"].as_str().unwrap_or(""),
            )
            .replace(
                "{{SHA256_LINUX_X64}}",
                m["x86_64-unknown-linux-gnu"].as_str().unwrap_or(""),
            )
            .replace(
                "{{SHA256_WIN_X64}}",
                m["x86_64-pc-windows-msvc"].as_str().unwrap_or(""),
            )
            .replace(
                "{{SHA256_WIN_ARM}}",
                m["aarch64-pc-windows-msvc"].as_str().unwrap_or(""),
            );
        // Chocolatey needs its nuspec + tools/ subtree preserved so `choco pack`
        // can find tools\chocolateyinstall.ps1; every other template stays flat
        // (the GitHub Release `out/packaging/*` glob uploads only the flat ones).
        let dest = if tmpl.starts_with("chocolatey/") {
            out_dir.join(tmpl)
        } else {
            out_dir.join(Path::new(tmpl).file_name().unwrap_or_default())
        };
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(dest, stamped)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn stamp_substitutes_version_and_sha() {
        let dir = tempdir().expect("tempdir");
        let manifest = dir.path().join("m.json");
        fs::write(
            &manifest,
            r#"{"x86_64-unknown-linux-gnu":"deadbeef","x86_64-apple-darwin":"feedface","aarch64-apple-darwin":"feedfade","aarch64-unknown-linux-gnu":"feedfacf","x86_64-pc-windows-msvc":"feedfad0","aarch64-pc-windows-msvc":"feedfad1"}"#,
        )
        .expect("write manifest");
        let out = dir.path().join("out");

        if !Path::new("packaging").exists() {
            return; // skip when running from outside repo root
        }
        stamp("1.0.0", &manifest, &out).expect("stamp");
        let brew = fs::read_to_string(out.join("origin.rb")).expect("read");
        assert!(brew.contains("version \"1.0.0\""));
        // The Apple-Silicon (aarch64) sha is wired; the Intel (x86_64-apple-darwin)
        // branch was removed (not built), so its `feedface` hash must NOT appear.
        assert!(brew.contains("feedfade"), "mac-arm sha missing");
        assert!(!brew.contains("feedface"), "intel-mac sha leaked into formula");

        // Scoop manifest: valid JSON, Windows hashes wired.
        let scoop = fs::read_to_string(out.join("origin.json")).expect("read scoop");
        let scoop_json: serde_json::Value = serde_json::from_str(&scoop).expect("scoop is valid json");
        assert_eq!(scoop_json["architecture"]["64bit"]["hash"], "feedfad0");
        assert_eq!(scoop_json["architecture"]["arm64"]["hash"], "feedfad1");

        // Nix flake sources: valid JSON, version + the 3 built systems wired.
        let nix = fs::read_to_string(out.join("flake-sources.json")).expect("read nix");
        let nix_json: serde_json::Value = serde_json::from_str(&nix).expect("nix is valid json");
        assert_eq!(nix_json["version"], "1.0.0");
        assert_eq!(nix_json["systems"]["aarch64-darwin"]["sha256"], "feedfade");

        // Chocolatey: nuspec + tools subtree preserved; NU5035 (licenseUrl) gone.
        let nuspec = fs::read_to_string(out.join("chocolatey/origin.nuspec")).expect("read nuspec");
        assert!(nuspec.contains("<version>1.0.0</version>"));
        assert!(!nuspec.contains("<licenseUrl>"), "licenseUrl element triggers NU5035");
        let choco_install =
            fs::read_to_string(out.join("chocolatey/tools/chocolateyinstall.ps1")).expect("read install");
        assert!(choco_install.contains("feedfad0")); // SHA256_WIN_X64
        assert!(choco_install.contains("feedfad1")); // SHA256_WIN_ARM
    }
}
