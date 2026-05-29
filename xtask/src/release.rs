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
        let dest = out_dir.join(Path::new(tmpl).file_name().unwrap_or_default());
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
        assert!(brew.contains("feedface"));
    }
}
