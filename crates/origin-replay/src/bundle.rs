use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use thiserror::Error;

const MAGIC: &[u8; 8] = b"ORIGREP1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub session_id: String,
    pub recorded_at_unix_ms: u64,
    pub origin_version: String,
}

#[derive(Debug, Error)]
#[allow(clippy::module_name_repetitions)]
pub enum BundleError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("zstd: {0}")]
    Zstd(String),
    #[error("tar: {0}")]
    Tar(String),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("bad magic")]
    BadMagic,
    #[error("missing manifest.json")]
    MissingManifest,
    #[error("entry not found: {0}")]
    NotFound(String),
}

#[allow(clippy::module_name_repetitions)]
pub struct BundleWriter {
    inner: tar::Builder<zstd::stream::AutoFinishEncoder<'static, File>>,
}

impl BundleWriter {
    #[allow(clippy::missing_errors_doc)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn create(path: &Path, manifest: Manifest) -> Result<Self, BundleError> {
        let mut f = File::create(path)?;
        f.write_all(MAGIC)?;
        let zenc = zstd::stream::Encoder::new(f, 3)
            .map_err(|e| BundleError::Zstd(e.to_string()))?
            .auto_finish();
        let mut tar = tar::Builder::new(zenc);
        let mj = serde_json::to_vec_pretty(&manifest)?;
        let mut hdr = tar::Header::new_gnu();
        hdr.set_size(mj.len() as u64);
        hdr.set_mode(0o644);
        hdr.set_cksum();
        tar.append_data(&mut hdr, "manifest.json", mj.as_slice())
            .map_err(|e| BundleError::Tar(e.to_string()))?;
        Ok(Self { inner: tar })
    }

    #[allow(clippy::missing_errors_doc)]
    pub fn write_entry(&mut self, name: &str, body: &[u8]) -> Result<(), BundleError> {
        let mut hdr = tar::Header::new_gnu();
        hdr.set_size(body.len() as u64);
        hdr.set_mode(0o644);
        hdr.set_cksum();
        self.inner
            .append_data(&mut hdr, name, body)
            .map_err(|e| BundleError::Tar(e.to_string()))
    }

    #[allow(clippy::missing_errors_doc)]
    pub fn finish(self) -> Result<(), BundleError> {
        // `AutoFinishEncoder` finalizes the zstd stream on drop, so dropping
        // here is sufficient — calling `.finish()` is unnecessary (and not
        // available on `AutoFinishEncoder`).
        drop(
            self.inner
                .into_inner()
                .map_err(|e| BundleError::Tar(e.to_string()))?,
        );
        Ok(())
    }
}

pub struct Bundle {
    manifest: Manifest,
    entries: HashMap<String, Vec<u8>>,
}

impl Bundle {
    #[allow(clippy::missing_errors_doc)]
    #[allow(clippy::cast_possible_truncation)]
    pub fn open(path: &Path) -> Result<Self, BundleError> {
        let mut f = File::open(path)?;
        let mut magic = [0u8; 8];
        f.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(BundleError::BadMagic);
        }
        let zdec = zstd::stream::Decoder::new(f).map_err(|e| BundleError::Zstd(e.to_string()))?;
        let mut tar = tar::Archive::new(zdec);
        let mut entries: HashMap<String, Vec<u8>> = HashMap::new();
        for e in tar.entries().map_err(|e| BundleError::Tar(e.to_string()))? {
            let mut e = e.map_err(|e| BundleError::Tar(e.to_string()))?;
            let path = e
                .path()
                .map_err(|e| BundleError::Tar(e.to_string()))?
                .into_owned();
            let name = path.to_string_lossy().into_owned();
            let mut buf = Vec::with_capacity(e.size() as usize);
            e.read_to_end(&mut buf)?;
            entries.insert(name, buf);
        }
        let manifest_bytes = entries.get("manifest.json").ok_or(BundleError::MissingManifest)?;
        let manifest: Manifest = serde_json::from_slice(manifest_bytes)?;
        Ok(Self { manifest, entries })
    }

    #[must_use]
    pub const fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    #[allow(clippy::missing_errors_doc)]
    pub fn read_entry(&self, name: &str) -> Result<&[u8], BundleError> {
        self.entries
            .get(name)
            .map(Vec::as_slice)
            .ok_or_else(|| BundleError::NotFound(name.to_string()))
    }

    pub fn entry_names(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }
}
