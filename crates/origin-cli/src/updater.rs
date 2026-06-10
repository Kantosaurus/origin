// SPDX-License-Identifier: Apache-2.0
//! Fully-automatic **npm-channel** auto-updater for the `origin` binary.
//!
//! Update decisions are driven entirely by npm: the *installed* version is read
//! from the npm package metadata shipped next to the binary, and the *latest*
//! version is read from the npm registry. There is **no `cosign` dependency** —
//! downloads are integrity-checked against the release's `SHA256SUMS` manifest
//! with the built-in `sha2` hasher, so auto-update works with zero external
//! tools installed.
//!
//! Every invocation of the CLI:
//!
//! 1. Calls [`apply_staged_if_present`] at startup. If a `<exe>.new` file is
//!    sitting next to the running binary (left there by a previous run's
//!    check), it is renamed over the current executable. On Windows the live
//!    process keeps using the now-renamed `.old` file, so the swap is safe for
//!    a running process.
//! 2. Calls [`check_and_stage_blocking`] synchronously. It first guards on
//!    install type: auto-update only runs for binaries distributed via npm (the
//!    running exe lives under a `node_modules` tree). Dev/source builds (cargo
//!    `target/`), `cargo install` (`~/.cargo/bin`), and direct downloads are
//!    left untouched so a local build is never clobbered, and the installed
//!    version is read from the adjacent npm `package.json`. It then checks the
//!    npm registry (`registry.npmjs.org/<pkg>/latest`) for the latest published
//!    version; when newer, it downloads the matching platform asset from the
//!    GitHub release for that version (where the npm channel also sources its
//!    binaries), verifies its SHA-256 against the release `SHA256SUMS`, and
//!    stages the result as `<exe>.new` for the caller to swap in + re-exec.
//!    Failures are logged via `tracing::warn!` and degraded to `Ok(false)` so
//!    offline / network-flaky users still run.
//!
//! Setting `ORIGIN_NO_UPDATE=1` (any value) short-circuits both the apply and
//! the network check — the binary then behaves as if the updater were absent. A
//! 24h on-disk cache at `$ORIGIN_HOME/.origin/update_check.json` (falling back
//! to `~/.origin/`) prevents hammering the npm registry.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// How long a successful update-check result is reused before re-querying.
///
/// 24 hours — short enough to pick up new releases the day after publish, long
/// enough to be a courteous registry citizen for typical CLI usage.
pub const UPDATE_CHECK_TTL_SECS: i64 = 86_400;

/// GitHub repository slug the binary *assets* are downloaded from. Hardcoded so
/// a hostile `$ORIGIN_*` env var can never redirect the binary's auto-update to
/// a third-party mirror. (npm only supplies the version number; the bytes come
/// from this release, sha256-verified.)
const RELEASES_REPO: &str = "Kantosaurus/origin";

/// npm registry base URL the *latest version* is read from.
const NPM_REGISTRY: &str = "https://registry.npmjs.org";

/// Scoped npm package whose published version drives the update decision.
const NPM_PACKAGE: &str = "@kantosaurus/origin";

/// HTTP timeout for the latest-version GET. Five seconds is long enough for
/// flaky links but short enough that a hung network never delays the user.
const HTTP_TIMEOUT_SECS: u64 = 5;

/// HTTP timeout for downloading a release asset. Asset binaries top out at a
/// few tens of MB; 60s covers slow links without blocking the next invocation
/// for an unreasonable time.
const DOWNLOAD_TIMEOUT_SECS: u64 = 60;

#[derive(Debug, Error)]
pub enum UpdateError {
    #[error("network: {0}")]
    Network(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("bad response: {0}")]
    BadResponse(String),
    #[error("checksum verification failed: {0}")]
    ChecksumFailed(String),
    #[error("unsupported platform: {0}")]
    UnsupportedPlatform(String),
}

impl From<reqwest::Error> for UpdateError {
    fn from(e: reqwest::Error) -> Self {
        Self::Network(e.to_string())
    }
}

/// On-disk cache entry. Written by [`write_cache`], read by [`cached_latest`].
/// The shape is forward-compatible: extra fields added in future versions are
/// ignored by older binaries, and missing fields produce a cache miss rather
/// than a panic.
#[derive(Debug, Serialize, Deserialize)]
struct CacheEntry {
    /// Unix epoch seconds at write time.
    checked_at: i64,
    /// The latest version string fetched from npm. Stored without any leading
    /// `v` prefix to keep the format stable.
    latest_version: String,
}

/// Resolve the cache file path. Honors `$ORIGIN_HOME` for tests and
/// alternate-root installs, matching the convention used by
/// `crates/origin-cli/src/config.rs::path`. Returns `None` only when neither
/// `$ORIGIN_HOME` nor a home directory can be resolved.
fn cache_path() -> Option<PathBuf> {
    let home = std::env::var_os("ORIGIN_HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)?;
    Some(home.join(".origin").join("update_check.json"))
}

/// Current unix-epoch seconds. Wrapped so tests don't have to read the system
/// clock and so a clock-skewed system can't underflow the math.
fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// Strip a single leading `v` or `V` prefix.
fn strip_v_prefix(s: &str) -> &str {
    s.strip_prefix('v').or_else(|| s.strip_prefix('V')).unwrap_or(s)
}

/// Read the on-disk cache.
///
/// Returns `Some(version)` (without a `v` prefix) when the cache exists,
/// parses, and was written within `ttl_secs`. Any other case — missing file,
/// parse error, stale entry — returns `None` so the caller falls back to a live
/// npm query.
#[must_use]
pub fn cached_latest(ttl_secs: i64) -> Option<String> {
    let path = cache_path()?;
    let bytes = std::fs::read(&path).ok()?;
    let entry: CacheEntry = serde_json::from_slice(&bytes).ok()?;
    let age = now_secs().saturating_sub(entry.checked_at);
    // Reject a non-semver cached value so a corrupt or tampered cache file can't
    // wedge the check (is_newer treats an unparseable version as "not newer").
    let parseable = parse_semver(strip_v_prefix(entry.latest_version.trim())).is_some();
    if age >= 0 && age < ttl_secs && parseable {
        Some(entry.latest_version)
    } else {
        None
    }
}

/// Write a cache entry.
///
/// Best-effort: any IO failure is logged via `tracing::warn!` and swallowed — a
/// missing or unwritable cache only costs an extra npm query next invocation.
pub fn write_cache(version: &str) {
    if let Err(e) = write_cache_inner(version) {
        tracing::warn!("updater: write cache failed: {e}");
    }
}

/// Inner body so `write_cache` itself stays trivially-low cognitive complexity.
fn write_cache_inner(version: &str) -> Result<(), String> {
    let path = cache_path().ok_or_else(|| "no home directory".to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create cache dir: {e}"))?;
    }
    let entry = CacheEntry {
        checked_at: now_secs(),
        latest_version: strip_v_prefix(version).to_string(),
    };
    let buf = serde_json::to_vec(&entry).map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(&path, buf).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(())
}

/// Compare two semver-ish version strings.
///
/// Returns `true` only when `latest` parses to a strictly greater semver than
/// `current`. Both sides accept an optional leading `v`. On parse failure of
/// either side, returns `false` — the safer default: an unparseable version
/// never triggers an update.
#[must_use]
pub fn is_newer(current: &str, latest: &str) -> bool {
    let c = strip_v_prefix(current.trim());
    let l = strip_v_prefix(latest.trim());
    match (parse_semver(c), parse_semver(l)) {
        (Some(cv), Some(lv)) => lv > cv,
        _ => false,
    }
}

/// Minimal semver triple parser — enough for `MAJOR.MINOR.PATCH[-pre]`
/// comparison without taking a dep on `semver`. Pre-release suffixes are
/// stripped before parsing so `0.1.0-rc1` is treated as `0.1.0`.
fn parse_semver(s: &str) -> Option<(u64, u64, u64)> {
    let core = s.split('-').next()?.split('+').next()?;
    let mut parts = core.split('.');
    let major: u64 = parts.next()?.parse().ok()?;
    let minor: u64 = parts.next()?.parse().ok()?;
    let patch: u64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// Map the current build host's OS+ARCH to the released asset filename.
///
/// The release workflow (`.github/workflows/release.yml`) stages binaries as
/// `origin-<target-triple>[.exe]`, e.g. `origin-x86_64-pc-windows-msvc.exe`.
///
/// # Errors
/// [`UpdateError::UnsupportedPlatform`] when the host doesn't match one of the
/// published targets.
pub fn current_target_asset_name() -> Result<String, UpdateError> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let (triple, ext) = match (os, arch) {
        ("linux", "x86_64") => ("x86_64-unknown-linux-gnu", ""),
        ("linux", "aarch64") => ("aarch64-unknown-linux-gnu", ""),
        ("macos", "x86_64") => ("x86_64-apple-darwin", ""),
        ("macos", "aarch64") => ("aarch64-apple-darwin", ""),
        ("windows", "x86_64") => ("x86_64-pc-windows-msvc", ".exe"),
        ("windows", "aarch64") => ("aarch64-pc-windows-msvc", ".exe"),
        _ => return Err(UpdateError::UnsupportedPlatform(format!("{os}/{arch}"))),
    };
    Ok(format!("origin-{triple}{ext}"))
}

/// `https://github.com/<repo>/releases/download/v<version>/<asset>` — the exact
/// URL the npm channel's `download.js` uses. npm supplies the version; the
/// bytes come from the GitHub release and are sha256-verified.
fn release_asset_url(version: &str, asset_name: &str) -> String {
    let v = strip_v_prefix(version.trim());
    format!("https://github.com/{RELEASES_REPO}/releases/download/v{v}/{asset_name}")
}

/// User-Agent string sent on every HTTP request. Format:
/// `origin-cli/<package-version>`.
fn user_agent() -> String {
    format!("origin-cli/{}", env!("CARGO_PKG_VERSION"))
}

// ── installed-version discovery (npm-channel guard) ───────────────────────────

/// Minimal `package.json` shape — `name` (to confirm it is origin's own npm
/// package) and `version`.
#[derive(Deserialize)]
struct PackageMeta {
    #[serde(default)]
    name: String,
    #[serde(default)]
    version: String,
}

/// npm registry `…/latest` manifest — we only read `version`.
#[derive(Deserialize)]
struct NpmManifest {
    version: String,
}

/// The npm-installed version of the running binary, or `None` when this is NOT
/// an npm-managed install (and therefore must not auto-update).
///
/// Returns `None` for dev/source builds, `cargo install`, and direct
/// downloads — auto-update is scoped to the npm distribution channel so a local
/// build is never clobbered and an unknown version never loops.
fn installed_npm_version() -> Option<String> {
    npm_version_for_exe(&current_exe().ok()?)
}

/// Pure core of [`installed_npm_version`], split out so it's testable without
/// stubbing `current_exe`.
fn npm_version_for_exe(exe: &Path) -> Option<String> {
    find_origin_package_json(exe).map(|(_, v)| v)
}

/// Locate origin's OWN `package.json` by walking up from `exe`, returning its
/// path and version. The binary must live under a `node_modules` tree (the npm
/// install marker); the package must be `@kantosaurus/origin` or one of its
/// `@kantosaurus/origin-<platform>` binary packages with a parseable semver — so
/// the walk can't latch onto a stray parent-project `package.json` or trust a
/// spoofed version (an unparseable version would wedge `is_newer`, which treats
/// it as "not newer").
fn find_origin_package_json(exe: &Path) -> Option<(PathBuf, String)> {
    if !exe.components().any(|c| c.as_os_str() == "node_modules") {
        return None;
    }
    let mut dir = exe.parent();
    while let Some(d) = dir {
        let pj = d.join("package.json");
        if let Ok(bytes) = std::fs::read(&pj) {
            if let Ok(meta) = serde_json::from_slice::<PackageMeta>(&bytes) {
                let is_origin =
                    meta.name == NPM_PACKAGE || meta.name.starts_with("@kantosaurus/origin-");
                let v = meta.version.trim();
                if is_origin && parse_semver(strip_v_prefix(v)).is_some() {
                    return Some((pj, v.to_string()));
                }
            }
        }
        dir = d.parent();
    }
    None
}

/// After staging an updated binary, write the new version into origin's own
/// `package.json` so the next launch's installed-version read matches the
/// swapped-in binary.
///
/// This is what makes the self-updater the safe default: npm rewrites
/// `package.json` only on its own install/update, but a self-update swaps just
/// the binary. Without recording the new version here, the version source would
/// keep reporting the pre-update version and the updater would re-download the
/// same release on every launch. Best-effort — a failure only costs one
/// redundant check next run (the staged-binary swap itself is unaffected).
fn record_staged_version(exe: &Path, version: &str) {
    let Some((pj, _)) = find_origin_package_json(exe) else {
        return;
    };
    let Ok(bytes) = std::fs::read(&pj) else {
        return;
    };
    let Ok(mut json) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return;
    };
    let Some(obj) = json.as_object_mut() else {
        return;
    };
    obj.insert(
        "version".to_string(),
        serde_json::Value::String(strip_v_prefix(version.trim()).to_string()),
    );
    if let Ok(buf) = serde_json::to_vec_pretty(&json) {
        let _ = std::fs::write(&pj, buf);
    }
}

/// GET `https://registry.npmjs.org/<pkg>/latest` and return its `version`.
///
/// # Errors
/// [`UpdateError::Network`] on transport failure; [`UpdateError::BadResponse`]
/// on non-2xx status or a JSON shape without a non-empty `version`.
pub async fn fetch_latest_npm_version() -> Result<String, UpdateError> {
    let url = format!("{NPM_REGISTRY}/{NPM_PACKAGE}/latest");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(HTTP_TIMEOUT_SECS))
        .user_agent(user_agent())
        .build()
        .map_err(|e| UpdateError::Network(e.to_string()))?;
    let resp = client
        .get(&url)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await?;
    if !resp.status().is_success() {
        return Err(UpdateError::BadResponse(format!(
            "GET {url} returned {}",
            resp.status()
        )));
    }
    let manifest: NpmManifest = resp
        .json()
        .await
        .map_err(|e| UpdateError::BadResponse(format!("decode npm manifest JSON: {e}")))?;
    if manifest.version.trim().is_empty() {
        return Err(UpdateError::BadResponse("empty npm version".into()));
    }
    Ok(manifest.version)
}

// ── download + verify + stage ─────────────────────────────────────────────────

/// Download `url` fully into memory. Uses a longer timeout than the version
/// check so slow links can complete the binary fetch without artificially
/// failing. Returning bytes (rather than writing a file) lets us verify the
/// SHA-256 of exactly what we'll stage, closing the verify→stage swap window.
async fn download_bytes(url: &str) -> Result<Vec<u8>, UpdateError> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(DOWNLOAD_TIMEOUT_SECS))
        .user_agent(user_agent())
        .build()
        .map_err(|e| UpdateError::Network(e.to_string()))?;
    let resp = client.get(url).send().await?;
    if !resp.status().is_success() {
        return Err(UpdateError::BadResponse(format!(
            "GET {url} returned {}",
            resp.status()
        )));
    }
    Ok(resp.bytes().await?.to_vec())
}

/// Compute the lowercase hex SHA-256 of `bytes` using the built-in `sha2`
/// hasher. No external tool required.
fn sha256_hex_bytes(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Extract the expected lowercase SHA-256 for `file_name` from `sha256sum`-style
/// manifest text. Each line is `<64-hex><spaces>[*]<name>` — matching GNU
/// coreutils' text (`  `) and binary (` *`) formats, and the shape produced by
/// the release workflow's `SHA256SUMS` step. Comparison is on the basename so a
/// manifest entry of either `origin-<triple>` or `dist/origin-<triple>`
/// resolves. Returns `None` when the manifest has no matching row.
fn expected_hash_for(sums_text: &str, file_name: &str) -> Option<String> {
    for line in sums_text.lines() {
        let line = line.trim();
        let mut it = line.splitn(2, char::is_whitespace);
        let hex = it.next()?;
        let rest = it.next()?.trim_start();
        if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        let name = rest.strip_prefix('*').unwrap_or(rest);
        let base = Path::new(name)
            .file_name()
            .map_or(name, |n| n.to_str().unwrap_or(name));
        if base == file_name {
            return Some(hex.to_ascii_lowercase());
        }
    }
    None
}

/// Download the release's `SHA256SUMS` manifest text. Best-effort: returns
/// `None` when the release doesn't publish one or it's unreachable.
async fn fetch_checksums(version: &str) -> Option<String> {
    let v = strip_v_prefix(version.trim());
    let url = format!("https://github.com/{RELEASES_REPO}/releases/download/v{v}/SHA256SUMS");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(HTTP_TIMEOUT_SECS))
        .user_agent(user_agent())
        .build()
        .ok()?;
    let resp = client.get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.text().await.ok()
}

/// Verify freshly-downloaded `bytes` against the release `SHA256SUMS`.
///
/// Verification is **mandatory** now that `cosign` is gone: an unverifiable
/// download (no manifest, or no matching entry) is rejected rather than staged,
/// so a missing/garbled `SHA256SUMS` never lets an unchecked binary be swapped
/// in.
///
/// # Errors
/// [`UpdateError::ChecksumFailed`] when no manifest entry exists or the digest
/// doesn't match.
fn verify_sha256_bytes(
    bytes: &[u8],
    checksums: Option<&str>,
    asset_name: &str,
) -> Result<(), UpdateError> {
    let expected = checksums
        .and_then(|text| expected_hash_for(text, asset_name))
        .ok_or_else(|| {
            UpdateError::ChecksumFailed(format!(
                "no SHA256SUMS entry for {asset_name}; refusing to stage an unverified binary"
            ))
        })?;
    let actual = sha256_hex_bytes(bytes);
    if !actual.eq_ignore_ascii_case(&expected) {
        return Err(UpdateError::ChecksumFailed(format!(
            "expected {expected}, computed {actual}"
        )));
    }
    tracing::info!("updater: verified {asset_name} via SHA256SUMS checksum");
    Ok(())
}

/// Locate the running executable's path. Used to derive the `<exe>.new` /
/// `<exe>.old` neighbor paths and to read the adjacent npm `package.json`.
fn current_exe() -> Result<PathBuf, UpdateError> {
    std::env::current_exe().map_err(UpdateError::Io)
}

/// If a file exists at `<current_exe>.new`, atomically swap it into the running
/// binary's spot:
///
/// 1. Rename the current exe to `<exe>.old` (Windows lets the live process keep
///    using the renamed file).
/// 2. Rename `<exe>.new` to the original exe path.
///
/// Returns `Ok(true)` when a swap occurred, `Ok(false)` when no staged file was
/// found, and `Err` on IO failure mid-swap.
///
/// # Errors
/// [`UpdateError::Io`] on rename / canonicalize failures.
pub fn apply_staged_if_present() -> Result<bool, UpdateError> {
    if std::env::var_os("ORIGIN_NO_UPDATE").is_some() {
        return Ok(false);
    }
    let exe = current_exe()?;
    let staged = staged_path(&exe);
    if !staged.exists() {
        return Ok(false);
    }
    let old = old_path(&exe);
    let _ = std::fs::remove_file(&old);
    std::fs::rename(&exe, &old)?;
    if let Err(e) = std::fs::rename(&staged, &exe) {
        let _ = std::fs::rename(&old, &exe);
        return Err(UpdateError::Io(e));
    }
    tracing::info!(
        "updater: swapped staged binary into place; previous binary preserved at {}",
        old.display()
    );
    Ok(true)
}

/// Helper: file name with `.{suffix}` appended (or `origin.{suffix}` if the
/// path has no file name component).
fn neighbor_path(exe: &Path, suffix: &str) -> PathBuf {
    let mut p = exe.to_path_buf();
    let name = exe
        .file_name()
        .map_or_else(|| "origin".to_string(), |n| n.to_string_lossy().into_owned());
    p.set_file_name(format!("{name}.{suffix}"));
    p
}

/// `<exe>.new` neighbor path.
fn staged_path(exe: &Path) -> PathBuf {
    neighbor_path(exe, "new")
}

/// `<exe>.old` neighbor path.
fn old_path(exe: &Path) -> PathBuf {
    neighbor_path(exe, "old")
}

/// Synchronous update check that resolves to `Ok(true)` iff a new binary was
/// downloaded, verified, and staged this call.
///
/// Returns `Ok(false)` when there's nothing to do (not an npm install, cache
/// fresh, no newer release, already up-to-date, or `ORIGIN_NO_UPDATE` is set).
/// Network and verification failures all resolve to `Ok(false)` (with a
/// `tracing::warn`) so the caller can fall through to running the current
/// binary.
///
/// # Errors
/// Currently never; the signature reserves room for future failure modes.
pub async fn check_and_stage_blocking() -> Result<bool, UpdateError> {
    if std::env::var_os("ORIGIN_NO_UPDATE").is_some() {
        return Ok(false);
    }
    Ok(check_and_stage_inner().await)
}

/// Print a user-visible failure message + log via `tracing::warn!`, then return
/// `false` so the caller can `return` directly.
fn skip_with_warn(stage: &str, err: impl std::fmt::Display) -> bool {
    eprintln!("Update check failed ({err}); continuing with current version.");
    tracing::warn!("updater: {stage} failed: {err}");
    false
}

/// Inner body returning `bool` so the public entry stays trivially shaped.
async fn check_and_stage_inner() -> bool {
    // Install-type guard + version source in one: only npm-distributed binaries
    // auto-update, and their installed version comes from the adjacent npm
    // package metadata. A dev/source build or non-npm install returns `None`
    // and is left untouched.
    let Some(current) = installed_npm_version() else {
        return false;
    };

    // Cache hit: if we recently saw the same-or-older latest version, skip the
    // network round trip entirely.
    if let Some(cached) = cached_latest(UPDATE_CHECK_TTL_SECS) {
        if !is_newer(&current, &cached) {
            return false;
        }
    } else {
        eprintln!("Checking for updates…");
    }

    let latest = match fetch_latest_npm_version().await {
        Ok(v) => v,
        Err(e) => return skip_with_warn("fetch npm version", e),
    };
    write_cache(&latest);

    if !is_newer(&current, &latest) {
        return false;
    }

    let asset_name = match current_target_asset_name() {
        Ok(n) => n,
        Err(e) => return skip_with_warn("current_target_asset_name", e),
    };
    let asset_url = release_asset_url(&latest, &asset_name);

    eprintln!("origin {current} → {latest}: downloading…");
    tracing::info!(
        "updater: update available (current={current} latest={latest}); downloading {asset_name}"
    );

    let exe = match current_exe() {
        Ok(p) => p,
        Err(e) => return skip_with_warn("current_exe", e),
    };

    // Pull the checksum manifest so verification can run; with cosign gone this
    // is the sole integrity gate and is mandatory.
    let checksums = fetch_checksums(&latest).await;

    download_verify_stage(&asset_url, checksums.as_deref(), &asset_name, &latest, &exe).await
}

/// Download, verify (SHA-256, mandatory), and stage. Cleans up partial
/// downloads on any failure so a later run never picks up an unverified binary.
async fn download_verify_stage(
    asset_url: &str,
    checksums: Option<&str>,
    asset_name: &str,
    version: &str,
    exe: &Path,
) -> bool {
    let bytes = match download_bytes(asset_url).await {
        Ok(b) => b,
        Err(e) => return skip_with_warn("download asset", e),
    };

    eprintln!("Verifying download…");
    if let Err(e) = verify_sha256_bytes(&bytes, checksums, asset_name) {
        return skip_with_warn("verify", e);
    }

    // Write the verified bytes to a temp neighbor, then atomically publish to
    // `<exe>.new`. Verifying the in-memory bytes (rather than a file we later
    // rename) closes the verify→stage swap window; the final rename keeps
    // staging atomic so a partial write never becomes a swapped-in binary.
    let Some(parent) = exe.parent() else {
        return skip_with_warn("exe parent", "exe has no parent directory");
    };
    let tmp = parent.join(format!("{asset_name}.download"));
    if let Err(e) = std::fs::write(&tmp, &bytes) {
        let _ = std::fs::remove_file(&tmp);
        return skip_with_warn("stage write", e);
    }
    // The downloaded release asset must be executable on unix once swapped in;
    // `std::fs::write` creates a 0644 file, so set the mode here (matching the
    // npm channel's `download.js`). No-op on Windows.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if let Err(e) = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)) {
            let _ = std::fs::remove_file(&tmp);
            return skip_with_warn("stage chmod", e);
        }
    }
    let staged = staged_path(exe);
    if let Err(e) = std::fs::rename(&tmp, &staged) {
        let _ = std::fs::remove_file(&tmp);
        return skip_with_warn("stage rename", e);
    }
    // Record the new version so the next launch doesn't see the (un-rewritten)
    // npm package.json and re-download this same release in a loop.
    record_staged_version(exe, version);
    eprintln!("Update staged; relaunching…");
    tracing::info!("updater: staged {} for swap-in this invocation", staged.display());
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use tokio::sync::Mutex;

    // All cache tests mutate the process-global `ORIGIN_HOME` env var. cargo
    // test runs in parallel by default, so without serialization they race. A
    // tokio Mutex is async-aware and safe to hold across awaits.
    static ENV_LOCK: Mutex<()> = Mutex::const_new(());

    #[test]
    fn is_newer_recognizes_patch_bump() {
        assert!(is_newer("0.0.1", "0.0.2"));
    }

    #[test]
    fn is_newer_recognizes_minor_bump() {
        assert!(is_newer("0.0.9", "0.1.0"));
    }

    #[test]
    fn is_newer_handles_v_prefix() {
        assert!(is_newer("v0.0.1", "v0.0.2"));
        assert!(is_newer("0.0.1", "v0.0.2"));
        assert!(is_newer("v0.0.1", "0.0.2"));
    }

    #[test]
    fn is_newer_returns_false_for_equal() {
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("v0.1.0", "0.1.0"));
    }

    #[test]
    fn is_newer_returns_false_for_older() {
        assert!(!is_newer("0.2.0", "0.1.0"));
        assert!(!is_newer("v1.0.0", "v0.9.9"));
    }

    #[test]
    fn is_newer_returns_false_on_unparseable() {
        assert!(!is_newer("garbage", "0.1.0"));
        assert!(!is_newer("0.1.0", "also garbage"));
        assert!(!is_newer("not.a.version", "0.0.1"));
    }

    #[test]
    #[allow(clippy::panic)]
    fn current_target_asset_name_includes_target_triple() {
        let name = current_target_asset_name().expect("supported test host");
        let os = std::env::consts::OS;
        let needle = match os {
            "windows" => "windows",
            "linux" => "linux",
            "macos" => "darwin",
            other => panic!("unexpected test host OS: {other}"),
        };
        assert!(name.contains(needle), "asset {name} should contain {needle}");
        assert!(name.starts_with("origin-"), "asset {name} should start with origin-");
    }

    #[test]
    fn release_asset_url_points_at_versioned_github_release() {
        assert_eq!(
            release_asset_url("0.9.0", "origin-x86_64-pc-windows-msvc.exe"),
            "https://github.com/Kantosaurus/origin/releases/download/v0.9.0/origin-x86_64-pc-windows-msvc.exe"
        );
        // A leading `v` on the version is normalized so we never emit `vv0.9.0`.
        assert_eq!(
            release_asset_url("v0.9.0", "origin-x86_64-unknown-linux-gnu"),
            "https://github.com/Kantosaurus/origin/releases/download/v0.9.0/origin-x86_64-unknown-linux-gnu"
        );
    }

    #[test]
    fn npm_manifest_parses_version() {
        // The `…/latest` registry endpoint returns far more than `version`; the
        // parser must ignore the rest.
        let raw = r#"{"name":"@kantosaurus/origin","version":"0.9.0","dist":{"tarball":"x"}}"#;
        let m: NpmManifest = serde_json::from_str(raw).expect("parse");
        assert_eq!(m.version, "0.9.0");
    }

    #[test]
    fn npm_version_for_exe_reads_adjacent_package_json() {
        // Mirror the npm platform-package layout:
        //   <tmp>/node_modules/pkg/package.json   (version)
        //   <tmp>/node_modules/pkg/bin/origin(.exe)
        let tmp = tempdir().expect("tempdir");
        let pkg = tmp.path().join("node_modules").join("pkg");
        let bin = pkg.join("bin");
        std::fs::create_dir_all(&bin).expect("mkdir");
        std::fs::write(
            pkg.join("package.json"),
            r#"{"name":"@kantosaurus/origin-win32-x64","version":"1.2.3"}"#,
        )
        .expect("write package.json");
        let exe = bin.join("origin");
        std::fs::write(&exe, b"binary").expect("write exe");

        assert_eq!(npm_version_for_exe(&exe).as_deref(), Some("1.2.3"));
    }

    #[test]
    fn npm_version_for_exe_rejects_foreign_package_and_garbage_version() {
        // The version source must be origin's OWN package.json with a valid
        // semver — a stray foreign package.json or a spoofed garbage version is
        // ignored (so a tampered file can't block updates).
        let tmp = tempdir().expect("tempdir");
        let pkg = tmp.path().join("node_modules").join("x");
        let bin = pkg.join("bin");
        std::fs::create_dir_all(&bin).expect("mkdir");
        let exe = bin.join("origin");
        std::fs::write(&exe, b"binary").expect("write exe");
        let pj = pkg.join("package.json");

        std::fs::write(&pj, r#"{"name":"some-other-pkg","version":"1.2.3"}"#).expect("w");
        assert!(npm_version_for_exe(&exe).is_none(), "foreign package must be ignored");

        std::fs::write(&pj, r#"{"name":"@kantosaurus/origin","version":"garbage"}"#).expect("w");
        assert!(npm_version_for_exe(&exe).is_none(), "non-semver version must be ignored");

        std::fs::write(&pj, r#"{"name":"@kantosaurus/origin","version":"0.9.0"}"#).expect("w");
        assert_eq!(npm_version_for_exe(&exe).as_deref(), Some("0.9.0"));
    }

    #[test]
    fn record_staged_version_breaks_the_self_update_loop() {
        // npm install at 0.9.0, then a self-update stages 0.10.0. The installed
        // version read MUST then report 0.10.0 (not the un-rewritten 0.9.0), or
        // the updater would re-download 0.10.0 on every launch forever.
        let tmp = tempdir().expect("tempdir");
        let pkg = tmp
            .path()
            .join("node_modules")
            .join("@kantosaurus")
            .join("origin-win32-x64");
        let bin = pkg.join("bin");
        std::fs::create_dir_all(&bin).expect("mkdir");
        std::fs::write(
            pkg.join("package.json"),
            r#"{"name":"@kantosaurus/origin-win32-x64","version":"0.9.0","bin":{"origin":"bin/origin.exe"}}"#,
        )
        .expect("write package.json");
        let exe = bin.join("origin.exe");
        std::fs::write(&exe, b"binary").expect("write exe");

        assert_eq!(npm_version_for_exe(&exe).as_deref(), Some("0.9.0"));

        record_staged_version(&exe, "0.10.0");

        assert_eq!(
            npm_version_for_exe(&exe).as_deref(),
            Some("0.10.0"),
            "installed-version read must reflect the staged version"
        );
        // Unrelated package.json fields must survive the rewrite.
        let raw = std::fs::read_to_string(pkg.join("package.json")).expect("read");
        let json: serde_json::Value = serde_json::from_str(&raw).expect("parse");
        assert_eq!(json["version"], "0.10.0");
        assert!(json.get("bin").is_some(), "unrelated fields must be preserved");
    }

    #[test]
    fn npm_version_for_exe_is_none_outside_node_modules() {
        // A dev/source build (cargo target dir, no node_modules) must NOT
        // auto-update — the guard returns None even if a package.json exists.
        let tmp = tempdir().expect("tempdir");
        let dir = tmp.path().join("target").join("debug");
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("package.json"), r#"{"version":"9.9.9"}"#).expect("write");
        let exe = dir.join("origin");
        std::fs::write(&exe, b"binary").expect("write exe");

        assert!(
            npm_version_for_exe(&exe).is_none(),
            "a build outside node_modules must not be treated as an npm install"
        );
    }

    #[tokio::test]
    async fn cache_round_trips() {
        let _g = ENV_LOCK.lock().await;
        let tmp = tempdir().expect("tempdir");
        std::env::set_var("ORIGIN_HOME", tmp.path());

        assert!(cached_latest(UPDATE_CHECK_TTL_SECS).is_none());

        write_cache("v0.1.0");
        let v = cached_latest(UPDATE_CHECK_TTL_SECS).expect("cache hit");
        assert_eq!(v, "0.1.0", "v-prefix should be stripped on write");

        let v = cached_latest(60).expect("within ttl");
        assert_eq!(v, "0.1.0");

        assert!(cached_latest(0).is_none(), "TTL of 0 should always miss");

        std::env::remove_var("ORIGIN_HOME");
    }

    #[test]
    fn strip_v_prefix_handles_both_cases() {
        assert_eq!(strip_v_prefix("v1.2.3"), "1.2.3");
        assert_eq!(strip_v_prefix("V1.2.3"), "1.2.3");
        assert_eq!(strip_v_prefix("1.2.3"), "1.2.3");
        assert_eq!(strip_v_prefix(""), "");
    }

    #[test]
    fn parse_semver_strips_prerelease() {
        assert_eq!(parse_semver("0.1.0-rc1"), Some((0, 1, 0)));
        assert_eq!(parse_semver("1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_semver("1.2"), None);
        assert_eq!(parse_semver("1.2.3.4"), None);
    }

    #[tokio::test]
    async fn env_var_bypass_short_circuits_check() {
        let _g = ENV_LOCK.lock().await;
        std::env::set_var("ORIGIN_NO_UPDATE", "1");
        let result = check_and_stage_blocking().await;
        std::env::remove_var("ORIGIN_NO_UPDATE");
        assert!(matches!(result, Ok(false)), "bypass should return Ok(false)");
    }

    #[tokio::test]
    async fn env_var_bypass_short_circuits_apply() {
        let _g = ENV_LOCK.lock().await;
        std::env::set_var("ORIGIN_NO_UPDATE", "1");
        let result = apply_staged_if_present();
        std::env::remove_var("ORIGIN_NO_UPDATE");
        assert!(matches!(result, Ok(false)), "bypass should return Ok(false)");
    }

    #[test]
    fn staged_and_old_paths_append_suffix() {
        let p = Path::new("/tmp/origin");
        assert_eq!(staged_path(p), PathBuf::from("/tmp/origin.new"));
        assert_eq!(old_path(p), PathBuf::from("/tmp/origin.old"));

        let pe = Path::new("C:/bin/origin.exe");
        assert_eq!(staged_path(pe), PathBuf::from("C:/bin/origin.exe.new"));
        assert_eq!(old_path(pe), PathBuf::from("C:/bin/origin.exe.old"));
    }

    #[test]
    fn sha256_hex_bytes_matches_known_vector() {
        // SHA-256("abc") NIST test vector — confirms lowercase zero-padded hex.
        assert_eq!(
            sha256_hex_bytes(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn expected_hash_for_parses_both_manifest_formats() {
        let h = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let text = format!(
            "{h}  origin-x86_64-unknown-linux-gnu\n\
             {h} *origin-x86_64-pc-windows-msvc.exe\n\
             {h}  dist/origin-aarch64-apple-darwin\n"
        );
        assert_eq!(
            expected_hash_for(&text, "origin-x86_64-unknown-linux-gnu").as_deref(),
            Some(h)
        );
        assert_eq!(
            expected_hash_for(&text, "origin-x86_64-pc-windows-msvc.exe").as_deref(),
            Some(h)
        );
        assert_eq!(
            expected_hash_for(&text, "origin-aarch64-apple-darwin").as_deref(),
            Some(h)
        );
        assert_eq!(expected_hash_for(&text, "origin-not-present"), None);
    }

    #[test]
    fn expected_hash_for_ignores_malformed_lines() {
        let text = "# comment\nnot-a-hash  origin-foo\n";
        assert_eq!(expected_hash_for(text, "origin-foo"), None);
    }

    #[test]
    fn verify_sha256_bytes_is_mandatory_and_matches_manifest() {
        let bytes = b"abc";
        let h = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let sums = format!("{h}  origin-test-asset\n");

        // Matching manifest entry → ok (case-insensitive digest).
        assert!(verify_sha256_bytes(bytes, Some(&sums), "origin-test-asset").is_ok());
        let upper = format!("{}  origin-test-asset\n", h.to_uppercase());
        assert!(verify_sha256_bytes(bytes, Some(&upper), "origin-test-asset").is_ok());

        // NO manifest → rejected (never stage an unverified binary).
        assert!(matches!(
            verify_sha256_bytes(bytes, None, "origin-test-asset"),
            Err(UpdateError::ChecksumFailed(_))
        ));

        // Manifest present but no entry for this asset → rejected.
        let other = format!("{h}  some-other-asset\n");
        assert!(matches!(
            verify_sha256_bytes(bytes, Some(&other), "origin-test-asset"),
            Err(UpdateError::ChecksumFailed(_))
        ));

        // Manifest present but mismatching digest → rejected.
        let bad = format!("{}  origin-test-asset\n", "0".repeat(64));
        assert!(matches!(
            verify_sha256_bytes(bytes, Some(&bad), "origin-test-asset"),
            Err(UpdateError::ChecksumFailed(_))
        ));
    }
}
